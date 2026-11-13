#!/usr/bin/env python3
"""Build a local experimental Felinaut app, not a distribution package.

Default: print a JSON plan without creating files or running commands.
--build: freshly build and assemble under target/felinaut-engines-*.
--run: also launch and return, retaining the app, logs and fresh private HOME.
--smoke: run bounded mixed WebKit/CEF loopback checks, then quit gracefully.
--webkit-only: omit CEF entirely (no SDK access or helper build).
The assembled executable's --list-engines manifest is verified before success
or launch, so concurrent Cargo artifact replacement cannot silently omit CEF.

CEF uses ONLY the already verified target/cef-runtime/156.0.26/macosarm64 SDK
and its original archive. No SDK acquisition, installation, signing, quarantine
changes, loader overrides or ordinary profiles. Cargo may fetch locked Rust
build dependencies. Native launches inherit only locale variables, not secrets.

Smoke requires both engines and tests same-origin cookie/storage isolation,
shared CEF contexts, DOM-only input, tab closure, rejected/confirmed switching,
fresh replacement documents and graceful root exit. Only the loopback fixture
allows indirect evaluation; no sandbox or TLS settings change. The test HOME
marker permits the native runtime's test-only mock keychain; --run never sets
native-test mode or creates that marker. No process scans or helper kills.
"""

from __future__ import annotations

import argparse
import http.server
import importlib.util
import json
import os
import platform
import plistlib
import re
import secrets
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TARGET = ROOT / "target"
SDK = TARGET / "cef-runtime/156.0.26/macosarm64"
MAIN = TARGET / "debug/felinaut"
HELPER = TARGET / "debug/felinaut-helper"
CLIENT = TARGET / "debug/felinaut-ctl"
HELPER_MANIFEST = ROOT / "tools/cef-helper/Cargo.toml"
TEMP_ROOT = Path("/private/tmp")
MARKER_NAME = ".felinaut-engine-test"
MARKER = b"felinaut-engine-test-v1\n"
FIXTURE = ROOT / "scripts/fixtures/mixed_engines.html"
MAX_ENGINE_MANIFEST_BYTES = 64 * 1024
CSP = (
    "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline' 'unsafe-eval'; "
    "connect-src 'none'; frame-src 'none'; frame-ancestors 'none'; "
    "form-action 'none'; base-uri 'none'; object-src 'none'"
)


class EngineError(RuntimeError):
    pass


class CommandError(EngineError):
    def __init__(self, program, returncode, diagnostic=""):
        # Keep diagnostics in memory for expected rejection checks, never in logs.
        super().__init__(f"{program} exited {returncode}; inspect retained logs")
        self.returncode = returncode
        self.diagnostic = diagnostic[:8192] if isinstance(diagnostic, str) else ""


STATE_SCRIPT = """(() => {
  const prefix = 'felinaut_engine_fixture=';
  const cookie = document.cookie.split('; ').find(value => value.startsWith(prefix));
  const input = document.querySelector('#text');
  const result = document.querySelector('#result');
  return {
    ready: document.documentElement.dataset.fixtureReady === 'yes',
    document: document.documentElement.dataset.fixtureDocument,
    cookie: cookie ? decodeURIComponent(cookie.slice(prefix.length)) : null,
    storage: localStorage.getItem('felinaut-engine-fixture'),
    input: input ? input.value : null,
    result: result ? (result.dataset.value ?? null) : null,
    trusted: result ? (result.dataset.trusted ?? null) : null
  };
})()"""


def probe_tools():
    """Load trusted local helpers lazily; restore import state and leave probe settings alone."""

    def load(name, dependency=None):
        path = ROOT / "tools/cef-probe" / (name + ".py")
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        # dataclasses needs its module registered while executing prepare_runtime.
        previous = sys.modules.get(name)
        old_dependency = sys.modules.get("prepare_runtime")
        sys.modules[name] = module
        if dependency is not None:
            sys.modules["prepare_runtime"] = dependency
        try:
            spec.loader.exec_module(module)
        finally:
            if previous is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = previous
            if dependency is not None:
                if old_dependency is None:
                    sys.modules.pop("prepare_runtime", None)
                else:
                    sys.modules["prepare_runtime"] = old_dependency
        return module

    runtime = load("prepare_runtime")
    return runtime, load("bundle_probe", runtime)


def build_commands(cef=True, runtime_shaders=False):
    features = (["engine-cef"] if cef else []) + (
        ["gpui/runtime_shaders"] if runtime_shaders else []
    )
    main = ["cargo", "build", "--locked"]
    if features:
        main += ["--features", ",".join(features)]
    main += ["--bins", "--target-dir", str(TARGET)]
    commands = [main]
    if cef:
        commands.append(
            [
                "cargo",
                "build",
                "--locked",
                "--manifest-path",
                str(HELPER_MANIFEST),
                "--bin",
                "felinaut-helper",
                "--target-dir",
                str(TARGET),
            ]
        )
    return commands


def build_environment(inherited, cef=True):
    env = {
        key: value
        for key, value in inherited.items()
        if not key.startswith(
            (
                "DYLD_",
                "__XPC_DYLD_",
                "WEBKIT_",
                "CEF_",
                "CHROME_",
                "FELINAUT_",
                "FELIFOX_",
            )
        )
        and key
        not in (
            "NIX_CEF_BINARY",
            "FLATPAK",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET",
        )
    }
    if cef:
        env["CEF_PATH"] = str(SDK)
    return env


def system_environment(inherited):
    allowed = ("LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "__CF_USER_TEXT_ENCODING")
    env = {key: inherited[key] for key in allowed if key in inherited}
    env["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
    return env


def private_file(path):
    return os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb")


def write_json(path, value):
    with private_file(path) as file:
        file.write((json.dumps(value, indent=2) + "\n").encode())


def run_command(command, *, env, timeout, log=None):
    try:
        result = subprocess.run(
            [str(arg) for arg in command],
            cwd=ROOT,
            env=env,
            timeout=timeout,
            check=True,
            stdout=log if log is not None else subprocess.PIPE,
            stderr=subprocess.STDOUT if log is not None else subprocess.PIPE,
            text=log is None,
        )
    except subprocess.TimeoutExpired as error:
        raise EngineError(f"{command[0]} timed out after {timeout:g}s") from error
    except subprocess.CalledProcessError as error:
        # Avoid echoing untrusted browser output or environment values into terminals.
        raise CommandError(command[0], error.returncode, error.stderr) from error
    return result.stdout if log is None else None


def parse_engine_manifest(data, cef=True):
    """Validate schema 1 and return only its bounded, known metadata fields."""
    if not isinstance(data, bytes) or not 0 < len(data) <= MAX_ENGINE_MANIFEST_BYTES:
        raise EngineError("Engine manifest is empty or exceeds the 64 KiB limit")

    def unique_fields(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise EngineError("Engine manifest contains duplicate JSON fields")
            value[key] = item
        return value

    def invalid_constant(value):
        raise EngineError("Engine manifest contains a non-JSON numeric constant")

    try:
        manifest = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=unique_fields,
            parse_constant=invalid_constant,
        )
    except (ValueError, UnicodeError, RecursionError) as error:
        raise EngineError("Malformed engine manifest JSON") from error
    if (
        not isinstance(manifest, dict)
        or type(manifest.get("schema_version")) is not int
        or manifest["schema_version"] != 1
        or not isinstance(manifest.get("engines"), list)
        or not 0 < len(manifest["engines"]) <= 64
    ):
        raise EngineError(
            "Engine manifest must have schema_version 1 and a nonempty engine list"
        )
    strings = ("id", "feature", "family", "limitation")
    booleans = ("built", "adapter_implemented", "requires_isolated_launcher")
    engines, ids, enabled = [], set(), set()
    for entry in manifest["engines"]:
        if (
            not isinstance(entry, dict)
            or any(
                not isinstance(entry.get(key), str)
                or not 0 < len(entry[key]) <= 4096
                or any(ord(char) < 32 or ord(char) == 127 for char in entry[key])
                for key in strings
            )
            or any(type(entry.get(key)) is not bool for key in booleans)
            or not re.fullmatch(r"[a-z][a-z0-9-]{0,63}", entry["id"])
        ):
            raise EngineError(
                "Engine manifest contains missing fields or invalid engine metadata types"
            )
        if entry["id"] in ids:
            raise EngineError("Engine manifest contains duplicate engine ids")
        ids.add(entry["id"])
        if entry["built"]:
            enabled.add(entry["id"])
        engines.append({key: entry[key] for key in (*strings, *booleans)})
    expected = {"system-webkit", "cef"} if cef else {"system-webkit"}
    if enabled != expected:
        raise EngineError(
            f"Engine manifest enabled set mismatch: expected {sorted(expected)}, found {sorted(enabled)}"
        )
    return {"schema_version": 1, "engines": engines}


def verify_app_engines(app, cef=True):
    """Inspect the copied executable, never the mutable shared Cargo artifact.

    --list-engines exits before native runtime/profile initialization. Capture to
    a temporary file so oversized output cannot cause an unbounded memory read.
    The command has a ten-second wall-clock budget and no HOME/loader overrides.
    """
    try:
        with tempfile.TemporaryFile(dir=app.parent) as metadata:
            run_command(
                [app / "Contents/MacOS/felinaut", "--list-engines"],
                env=system_environment(os.environ),
                timeout=10,
                log=metadata,
            )
            if os.fstat(metadata.fileno()).st_size > MAX_ENGINE_MANIFEST_BYTES:
                raise EngineError("Engine manifest exceeds the 64 KiB limit")
            metadata.seek(0)
            return parse_engine_manifest(
                metadata.read(MAX_ENGINE_MANIFEST_BYTES + 1), cef
            )
    except (EngineError, OSError) as error:
        raise EngineError(
            f"Assembled app engine verification refused: {error}. "
            "Possible concurrent Cargo artifact overwrite of target/debug/felinaut; "
            "stop other Cargo commands and rerun this launcher sequentially. No app launched."
        ) from error


def create_output(bundle):
    bundle.no_symlink_ancestors(TARGET)
    TARGET.mkdir(exist_ok=True)
    return Path(tempfile.mkdtemp(prefix="felinaut-engines-", dir=TARGET))


def write_plist(contents, executable, identifier, helper=False):
    value = {
        "CFBundleName": executable,
        "CFBundleDisplayName": executable,
        "CFBundleExecutable": executable,
        "CFBundleIdentifier": "io.github.casaroli.felinaut.engines-dev" + identifier,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundlePackageType": "APPL",
        "CFBundleVersion": "1",
        "NSHighResolutionCapable": True,
    }
    if helper:
        value["LSUIElement"] = True
    with (contents / "Info.plist").open("xb") as file:
        plistlib.dump(value, file, sort_keys=True)


def verify_sdk(bundle, deadline):
    # verify_tree's private_directory helper can create a missing directory. Refuse
    # that case first: this launcher must remain a read-only consumer of the SDK.
    bundle.no_symlink_ancestors(SDK)
    if not SDK.is_dir():
        raise EngineError(
            f"Prepared SDK is missing: {SDK}; prepare it separately (no acquisition attempted)"
        )
    return bundle.verify_prepared_sdk(SDK, deadline)


def assemble(output, runtime, bundle, cef=True):
    """Copy fresh binaries into a new private app; never mutate probe globals/SDK."""
    output = bundle.absolute_path(output)
    bundle.no_symlink_ancestors(output)
    if output.parent != TARGET or not output.name.startswith("felinaut-engines-"):
        raise EngineError(
            "App output must be a private target/felinaut-engines-* directory"
        )
    runtime.private_directory(output)
    destination = output / "Felinaut Engines.app"
    if destination.exists() or destination.is_symlink():
        raise EngineError("App already exists; refusing to overwrite")
    deadline = runtime.Deadline(180)
    # Verify again after Cargo, before copying anything from the SDK.
    sdk, manifest = verify_sdk(bundle, deadline) if cef else (None, {})
    main, main_info = bundle.executable_info(MAIN, deadline)
    helper, helper_info = (
        bundle.executable_info(HELPER, deadline) if cef else (None, {})
    )
    if cef and os.path.samefile(main, helper):
        raise EngineError("Root and helper must be distinct executable files")
    required = main_info["size"] + 256 * 1024**2
    if cef:
        prefix = "Release/" + bundle.FRAMEWORK
        required += len(bundle.HELPERS) * helper_info["size"] + sum(
            entry.get("size", 0)
            for name, entry in manifest.items()
            if name.startswith(prefix + "/") or name in bundle.LICENSES
        )
    if shutil.disk_usage(output).free < required:
        raise EngineError(f"Insufficient app disk budget; require {required} bytes")
    destination.mkdir(mode=0o700)
    try:
        contents = destination / "Contents"
        macos = contents / "MacOS"
        macos.mkdir(parents=True, mode=0o755)
        bundle.copy_verified_file(main, macos / "felinaut", main_info, deadline)
        write_plist(contents, "felinaut", "")
        if cef:
            frameworks = contents / "Frameworks"
            frameworks.mkdir(mode=0o755)
            bundle.copy_framework(
                sdk, frameworks / bundle.FRAMEWORK, manifest, deadline
            )
            for suffix, identifier in bundle.HELPERS:
                name = "felinaut " + suffix
                helper_contents = frameworks / (name + ".app") / "Contents"
                helper_macos = helper_contents / "MacOS"
                helper_macos.mkdir(parents=True, mode=0o755)
                bundle.copy_verified_file(
                    helper, helper_macos / name, helper_info, deadline
                )
                write_plist(helper_contents, name, "." + identifier, helper=True)
            resources = contents / "Resources/cef"
            resources.mkdir(parents=True, mode=0o755)
            for name in bundle.LICENSES:
                bundle.copy_verified_file(
                    sdk / name,
                    resources / name,
                    manifest[name],
                    deadline,
                    mode=0o444,
                )
        return destination
    except BaseException:
        shutil.rmtree(destination)
        raise


def validate_profile(home, *, smoke=False, fresh=False):
    if (
        not home.is_absolute()
        or home.parent != TEMP_ROOT
        or home.resolve(strict=True) != home
        or not home.name.startswith(
            "felinaut-engines-test-" if smoke else "felinaut-engines-"
        )
        or (not smoke and home.name.startswith("felinaut-engines-test-"))
    ):
        raise EngineError("Expected a canonical, fresh, private engines HOME")
    info = home.lstat()
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) != 0o700
    ):
        raise EngineError("Engines HOME must be owned and mode 0700")
    marker = home / MARKER_NAME
    if smoke:
        info = marker.lstat()
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) != 0o600
            or info.st_size != len(MARKER)
            or marker.read_bytes() != MARKER
        ):
            raise EngineError("Invalid native-test HOME marker")
    elif marker.exists() or marker.is_symlink():
        raise EngineError("Interactive HOME must not carry the native-test marker")
    if fresh and set(home.iterdir()) != ({marker} if smoke else set()):
        raise EngineError("HOME must be fresh before launch")


def create_profile(smoke=False):
    prefix = "felinaut-engines-test-" if smoke else "felinaut-engines-"
    home = Path(tempfile.mkdtemp(prefix=prefix, dir=TEMP_ROOT))
    if smoke:
        with private_file(home / MARKER_NAME) as marker:
            marker.write(MARKER)
    validate_profile(home, smoke=smoke, fresh=True)
    return home


def launch_environment(home, inherited, *, smoke=False):
    validate_profile(home, smoke=smoke, fresh=True)
    env = system_environment(inherited)
    env.update(
        HOME=str(home),
        CFFIXED_USER_HOME=str(home),
        FELINAUT_AGENT="1" if smoke else "0",
    )
    if smoke:
        env["FELINAUT_NATIVE_TEST"] = "1"
    return env


def launch(app, home, output, *, smoke=False):
    env = launch_environment(home, os.environ, smoke=smoke)
    with private_file(output / "browser.log") as log:
        process = subprocess.Popen(
            [str(app / "Contents/MacOS/felinaut")],
            cwd=app.parent,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=log,
            start_new_session=True,
        )
    return process, env


def stop_owned_root(process):
    """Never scan for processes, signal a process group, or delete a live profile."""
    if process.poll() is not None:
        return
    try:
        process.terminate()
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


@contextmanager
def fixture_server():
    html = FIXTURE.read_bytes()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            # Reject alternate Host headers, queries and paths; never serve files.
            if (
                self.path != "/"
                or self.headers.get("Host") != f"127.0.0.1:{self.server.server_port}"
            ):
                self.send_error(404)
                return
            # A different document marker for every real HTTP load, not every
            # evaluation. This tests replacement, not privileged document identity.
            page = html.replace(
                b"__FELINAUT_DOCUMENT_ID__", secrets.token_hex(16).encode()
            )
            self.send_response(200)
            for name, value in (
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Length", str(len(page))),
                ("Cache-Control", "no-store"),
                ("Content-Security-Policy", CSP),
                ("X-Content-Type-Options", "nosniff"),
            ):
                self.send_header(name, value)
            self.end_headers()
            self.wfile.write(page)

        def log_message(self, format, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(
        target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
    )
    try:
        thread.start()
        yield f"http://127.0.0.1:{server.server_port}/"
    finally:
        if thread.is_alive():
            server.shutdown()
            thread.join(timeout=2)
        server.server_close()


def single_tab_at(tabs, url, *, loaded=False):
    return (
        isinstance(tabs, list)
        and len(tabs) == 1
        and isinstance(tabs[0], dict)
        and tabs[0].get("url") == url
        and (not loaded or tabs[0].get("loading") is False)
    )


def wait_for(process, check, end):
    while True:
        if process.poll() is not None:
            raise EngineError("Browser exited; inspect browser.log")
        remaining = end - time.monotonic()
        if remaining <= 0:
            raise EngineError("Smoke timed out waiting for the loopback app/agent")
        try:
            result = check(min(3, remaining))
            if result:
                return result
        except (CommandError, json.JSONDecodeError):
            pass
        time.sleep(min(0.1, max(0, end - time.monotonic())))


def capture_window(process, output, end):
    def remaining():
        seconds = end - time.monotonic()
        if seconds <= 0:
            raise EngineError("Smoke capture time budget exhausted")
        return seconds

    env = system_environment(os.environ)
    # System capture tools use the desktop user's context, not the test profile.
    for key in ("HOME", "TMPDIR"):
        if key in os.environ:
            env[key] = os.environ[key]
    helper = output / "backend-window"
    run_command(
        [
            "/usr/bin/xcrun",
            "swiftc",
            ROOT / "scripts/backend_window.swift",
            "-module-cache-path",
            output / "swift-module-cache",
            "-o",
            helper,
        ],
        env=env,
        timeout=min(60, remaining()),
    )
    if process.poll() is not None:
        raise EngineError("Browser exited before capture")
    window = run_command(
        [helper, process.pid], env=env, timeout=min(10, remaining())
    ).strip()
    if not re.fullmatch(r"[1-9][0-9]*", window):
        raise EngineError("Window helper returned no owned window ID")
    run_command(
        [
            "/usr/sbin/screencapture",
            "-x",
            "-o",
            "-l",
            window,
            output / "window.png",
        ],
        env=env,
        timeout=min(10, remaining()),
    )


def remaining_seconds(end, limit=3):
    remaining = end - time.monotonic()
    if remaining <= 0:
        raise EngineError("Smoke time budget exhausted")
    return min(limit, remaining)


def engine_status(value):
    if (
        not isinstance(value, dict)
        or value.get("selected") not in ("webkit", "cef")
        or "pending" not in value
        or value["pending"] not in (None, "webkit", "cef")
        or "error" not in value
        or value["error"] is not None
        or not isinstance(value.get("available"), list)
    ):
        raise EngineError("Malformed engine status or explicit engine failure")
    available = value["available"]
    if (
        any(not isinstance(item, dict) for item in available)
        or len(
            {item.get("id") for item in available if isinstance(item.get("id"), str)}
        )
        != len(available)
        or any(
            not any(
                item.get("id") == name
                and item.get("ready") is True
                and item.get("error") is None
                for item in available
            )
            for name in ("webkit", "cef")
        )
    ):
        raise EngineError("Both WebKit and CEF must report runtime-ready availability")
    return value


def active_tab(tabs, count):
    if (
        not isinstance(tabs, list)
        or len(tabs) != count
        or any(not isinstance(tab, dict) for tab in tabs)
    ):
        raise EngineError("Unexpected tab count or malformed tab response")
    if any(
        tab.get("error") is not None or tab.get("switch_error") is not None
        for tab in tabs
    ):
        raise EngineError("Browser reported a page or engine-switch error")
    active = [tab for tab in tabs if tab.get("active") is True]
    if len(active) != 1:
        raise EngineError("Expected exactly one active tab")
    return active[0]


def snapshot_ref(snapshot, role, label):
    matches = re.findall(
        rf"^(@e[0-9]+) {re.escape(role)} {re.escape(json.dumps(label))}(?: = .*)?$",
        snapshot,
        re.MULTILINE,
    )
    if len(matches) != 1:
        raise EngineError("Fixture snapshot must contain exactly one matching control")
    return matches[0]


def invalid_engine_request(home, *, timeout):
    """Exercise the host rejection, not just the CLI's argument parser.

    Only this marked private smoke HOME's authenticated Unix socket is used.
    Neither the token nor any raw response is retained in artifacts or errors.
    """
    validate_profile(home, smoke=True)
    directory = home / "Library/Application Support/Felifox"
    if directory.resolve(strict=True) != directory:
        raise EngineError("Redirected agent directory refused")
    token_path, socket_path = directory / "agent.token", directory / "agent.sock"
    for path, expected in ((token_path, stat.S_ISREG), (socket_path, stat.S_ISSOCK)):
        info = path.lstat()
        if (
            not expected(info.st_mode)
            or info.st_uid != os.getuid()
            or stat.S_IMODE(info.st_mode) & 0o077
        ):
            raise EngineError(
                "Agent endpoint must be owned, private and not redirected"
            )
    if not 0 < token_path.stat().st_size <= 4096:
        raise EngineError("Invalid agent token size")
    payload = (
        json.dumps(
            {
                "cmd": "engine",
                "engine": "invalid-engine",
                "confirm": True,
                "token": token_path.read_text(encoding="utf-8"),
            }
        ).encode()
        + b"\n"
    )
    end = time.monotonic() + timeout
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(remaining_seconds(end, timeout))
        client.connect(str(socket_path))
        client.settimeout(remaining_seconds(end, timeout))
        client.sendall(payload)
        data = bytearray()
        while b"\n" not in data:
            client.settimeout(remaining_seconds(end, timeout))
            chunk = client.recv(min(4096, 65537 - len(data)))
            if not chunk:
                raise EngineError("Agent closed without a complete rejection response")
            data.extend(chunk)
            if len(data) > 65536:
                raise EngineError("Agent rejection response exceeded the size limit")
    try:
        response = json.loads(data.split(b"\n", 1)[0])
    except (ValueError, UnicodeError) as error:
        raise EngineError("Malformed agent rejection response") from error
    if (
        not isinstance(response, dict)
        or response.get("ok") is not False
        or not isinstance(response.get("error"), str)
        or not re.search(
            r"(?:unknown|invalid|unsupported|unavailable).*engine|engine.*(?:unknown|invalid|unsupported|unavailable)",
            response["error"],
            re.IGNORECASE,
        )
    ):
        raise EngineError("Invalid engine must receive an explicit host rejection")


def smoke(app, output, *, timeout=90, capture=False):
    home = create_profile(smoke=True)
    report = {
        "status": "failed",
        "home": str(home),
        "app": str(app),
        "checks": [],
        "input_mode": "DOM-only (not trusted native input)",
        "helper_exit": "not independently verified; profile retained",
        "cef_shutdown": "not independently instrumented; inspect retained browser.log",
        "graceful_quit": False,
    }
    process = None
    end = time.monotonic() + timeout

    def stage(name):
        report["stage"] = name

    def passed():
        report["checks"].append(report["stage"])

    try:
        with fixture_server() as url:
            stage("startup")
            process, env = launch(app, home, output, smoke=True)
            report.update(pid=process.pid, fixture=url)

            def ctl(*args, seconds=None):
                return run_command(
                    [CLIENT, *args],
                    env=env,
                    timeout=remaining_seconds(end)
                    if seconds is None
                    else min(seconds, remaining_seconds(end)),
                )

            def query(*args, seconds=None):
                try:
                    return json.loads(ctl(*args, seconds=seconds))
                except (ValueError, UnicodeError) as error:
                    raise EngineError("Malformed JSON from browser control") from error

            def ready(engine, count, location=url):
                def check(seconds):
                    selected = engine_status(query("engine", seconds=seconds))
                    tab = active_tab(
                        query("tabs", seconds=remaining_seconds(end)), count
                    )
                    return (
                        selected["selected"] == engine
                        and selected["pending"] is None
                        and tab.get("engine") == engine
                        and tab.get("url") == location
                        and tab.get("loading") is False
                    )

                wait_for(process, check, end)

            def identity(engine):
                backend = query("backend")
                family = "webkit" if engine == "webkit" else "chromium"
                if (
                    not isinstance(backend, dict)
                    or backend.get("backend")
                    != ("native-webkit" if engine == "webkit" else "cef")
                    or not isinstance(backend.get("engine_family"), str)
                    or backend["engine_family"].casefold() != family
                    or not isinstance(backend.get("runtime_version"), str)
                    or not backend["runtime_version"].strip()
                ):
                    raise EngineError(
                        "Actual backend identity does not match the selected engine"
                    )
                report.setdefault("backends", {})[engine] = {
                    key: backend[key]
                    for key in ("backend", "engine_family", "runtime_version")
                }

            def state(cookie, storage, *, fresh=False, same=None, different=None):
                def document_ready(seconds):
                    value = query("eval", STATE_SCRIPT, seconds=seconds)
                    if not isinstance(value, dict):
                        raise EngineError("Malformed fixture DOM state")
                    return False if value.get("ready") is False else value

                value = wait_for(process, document_ready, end)
                if (
                    not isinstance(value, dict)
                    or value.get("ready") is not True
                    or not isinstance(value.get("document"), str)
                    or not re.fullmatch(r"[0-9a-f]{32}", value["document"])
                    or "cookie" not in value
                    or value["cookie"] != cookie
                    or "storage" not in value
                    or value["storage"] != storage
                    or (
                        fresh
                        and (
                            value.get("input") != "" or value.get("result") is not None
                        )
                    )
                    or (
                        same is not None
                        and (
                            value["document"] != same
                            or value.get("input") != storage
                            or value.get("result") != storage
                            or value.get("trusted") != "false"
                        )
                    )
                    or (different is not None and value["document"] in different)
                ):
                    raise EngineError(
                        "Fixture DOM identity, cookie or storage isolation check failed"
                    )
                return value["document"]

            def type_marker(marker):
                reference = snapshot_ref(ctl("snapshot"), "textbox", "Test input")
                ctl("type", reference, marker)
                # References expire after mutations; always take a fresh snapshot.
                reference = snapshot_ref(ctl("snapshot"), "button", "Verify input")
                ctl("click", reference)
                value = query("eval", STATE_SCRIPT)
                if (
                    not isinstance(value, dict)
                    or any(
                        value.get(key) != marker
                        for key in ("cookie", "storage", "input", "result")
                    )
                    or value.get("trusted") != "false"
                ):
                    raise EngineError(
                        "DOM-only fixture typing/click/cookie/storage check failed"
                    )

            def new_tab(engine, count):
                index = query("new-tab", engine)
                if type(index) is not int or index != 0:
                    raise EngineError("New tab must return insertion index zero")
                ready(engine, count, "felinaut://newtab")
                ctl("navigate", url)
                ready(engine, count)
                identity(engine)

            wait_for(
                process,
                lambda seconds: single_tab_at(
                    query("tabs", seconds=seconds), "felinaut://newtab"
                ),
                end,
            )
            ready("webkit", 1, "felinaut://newtab")
            identity("webkit")
            passed()

            stage("webkit-empty-context-and-dom-input")
            ctl("navigate", url)
            ready("webkit", 1)
            webkit_document = state(None, None, fresh=True)
            type_marker("webkit-marker")
            state("webkit-marker", "webkit-marker", same=webkit_document)
            passed()

            stage("cef-isolated-context-and-dom-input")
            new_tab("cef", 2)
            cef_document = state(None, None, fresh=True, different={webkit_document})
            type_marker("cef-marker")
            state("cef-marker", "cef-marker", same=cef_document)
            passed()

            stage("second-cef-tab-shares-context")
            new_tab("cef", 3)
            state(
                "cef-marker",
                "cef-marker",
                fresh=True,
                different={webkit_document, cef_document},
            )
            passed()

            stage("close-cef-tab-preserves-first-page")
            ctl("close-tab")
            ctl("tab", "0")
            ready("cef", 2)
            state("cef-marker", "cef-marker", same=cef_document)
            type_marker("cef-marker")
            passed()

            stage("unconfirmed-switch-rejected-without-page-change")
            try:
                ctl("engine", "webkit")
            except CommandError as error:
                if (
                    error.returncode != 1
                    or "confirm" not in error.diagnostic.casefold()
                ):
                    raise EngineError(
                        "Expected an explicit reload-confirmation rejection"
                    ) from error
            else:
                raise EngineError(
                    "Loaded engine switch was accepted without confirmation"
                )
            ready("cef", 2)
            state("cef-marker", "cef-marker", same=cef_document)
            passed()

            stage("invalid-engine-rejected-without-page-change")
            invalid_engine_request(home, timeout=remaining_seconds(end))
            ready("cef", 2)
            state("cef-marker", "cef-marker", same=cef_document)
            passed()

            stage("confirmed-cef-to-webkit-switch")
            ctl("engine", "webkit", "--confirm")
            ready("webkit", 2)
            identity("webkit")
            switched_webkit_document = state(
                "webkit-marker",
                "webkit-marker",
                fresh=True,
                different={webkit_document, cef_document},
            )
            passed()

            stage("confirmed-webkit-to-cef-fresh-document")
            ctl("engine", "cef", "--confirm")
            ready("cef", 2)
            identity("cef")
            switched_cef_document = state(
                "cef-marker",
                "cef-marker",
                fresh=True,
                different={webkit_document, cef_document, switched_webkit_document},
            )
            type_marker("cef-marker")
            passed()

            stage("original-webkit-tab-still-usable")
            ctl("tab", "1")
            ready("webkit", 2)
            state("webkit-marker", "webkit-marker", same=webkit_document)
            type_marker("webkit-marker")
            passed()

            stage("new-webkit-tab-shares-only-webkit-context")
            new_tab("webkit", 3)
            state(
                "webkit-marker",
                "webkit-marker",
                fresh=True,
                different={webkit_document, switched_cef_document},
            )
            ctl("close-tab")
            ctl("tab", "0")
            ready("cef", 2)
            state("cef-marker", "cef-marker", same=switched_cef_document)
            passed()

            if capture:
                stage("owned-cef-browser-window-capture")
                capture_window(process, output, end)
                report["window_capture"] = "window.png"
                passed()

            stage("graceful-quit")
            ctl("quit")
            try:
                returncode = process.wait(timeout=remaining_seconds(end, 15))
            except subprocess.TimeoutExpired as error:
                raise EngineError("Graceful browser quit timed out") from error
            if returncode != 0:
                raise EngineError("Browser exited unsuccessfully during graceful quit")
            report.update(
                status="passed", mixed_engine_validation="passed", graceful_quit=True
            )
            passed()
    except BaseException as error:
        report["error"] = type(error).__name__
        raise
    finally:
        try:
            if process is not None:
                # Success must never send SIGTERM/SIGKILL. Only failure cleanup
                # may stop this owned root; helpers and profiles are not touched.
                if not report["graceful_quit"] and process.poll() is None:
                    report["failure_cleanup"] = "owned root only"
                    stop_owned_root(process)
                report["root_returncode"] = process.poll()
        finally:
            write_json(output / "smoke.json", report)
    return report


def plan(args):
    return {
        "mode": "plan",
        "engines": ["webkit", "cef"] if not args.webkit_only else ["webkit"],
        "sdk": str(SDK) if not args.webkit_only else None,
        "sdk_acquisition": False,
        "build_commands": build_commands(not args.webkit_only, args.runtime_shaders),
        "build_timeout_seconds_each": args.build_timeout,
        "output": str(TARGET / "felinaut-engines-<fresh>/Felinaut Engines.app"),
        "root_executable": "Contents/MacOS/felinaut",
        "helper_executable": "Contents/Frameworks/felinaut Helper.app/Contents/MacOS/felinaut Helper"
        if not args.webkit_only
        else None,
        "launch": "bounded mixed-engine smoke"
        if args.smoke
        else "retained user-test process"
        if args.run
        else False,
        "home": str(
            TEMP_ROOT
            / (
                "felinaut-engines-test-<fresh>"
                if args.smoke
                else "felinaut-engines-<fresh>"
            )
        ),
        "smoke_timeout_seconds": args.smoke_timeout,
        "mixed_engine_validation": "not run; --smoke tests WebKit and CEF",
        "notice": "Unsigned local dev app, not a distribution; no normal profiles, installs or security overrides",
    }


def bounded_seconds(minimum, maximum):
    def parse(value):
        try:
            seconds = int(value)
        except ValueError as error:
            raise argparse.ArgumentTypeError("Expected integer seconds") from error
        if not minimum <= seconds <= maximum:
            raise argparse.ArgumentTypeError(f"Expected {minimum}..{maximum} seconds")
        return seconds

    return parse


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--build", action="store_true", help="Build and assemble a fresh dev app"
    )
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument(
        "--run",
        action="store_true",
        help="Implies --build; launch and return, retaining app/HOME",
    )
    modes.add_argument(
        "--smoke",
        action="store_true",
        help="Implies --build; bounded mixed-engine loopback smoke and graceful quit",
    )
    parser.add_argument(
        "--webkit-only", action="store_true", help="No CEF SDK/helper access"
    )
    parser.add_argument(
        "--runtime-shaders",
        action="store_true",
        help="Use GPUI runtime shaders without full Xcode/Metal",
    )
    parser.add_argument(
        "--capture",
        action="store_true",
        help="Smoke only: capture the owned window (Screen Recording required)",
    )
    parser.add_argument("--build-timeout", type=bounded_seconds(1, 1800), default=600)
    parser.add_argument("--smoke-timeout", type=bounded_seconds(5, 120), default=90)
    args = parser.parse_args(argv)
    if args.capture and not args.smoke:
        parser.error("--capture requires --smoke")
    if args.smoke and args.webkit_only:
        parser.error("--smoke requires both engines; omit --webkit-only")
    if not (args.build or args.run or args.smoke):
        print(json.dumps(plan(args), indent=2))
        return 0
    output = None
    try:
        if platform.system() != "Darwin" or platform.machine() != "arm64":
            raise EngineError(
                "This experimental launcher currently requires native macOS arm64"
            )
        runtime, bundle = probe_tools()
        cef = not args.webkit_only
        if cef:
            verify_sdk(bundle, runtime.Deadline(180))
            bundle.no_symlink_ancestors(HELPER_MANIFEST)
            runtime.regular_file(HELPER_MANIFEST)
        output = create_output(bundle)
        write_json(output / "plan.json", plan(args))
        env = build_environment(os.environ, cef)
        for index, command in enumerate(
            build_commands(cef, args.runtime_shaders), start=1
        ):
            with private_file(output / f"build-{index}.log") as log:
                run_command(command, env=env, timeout=args.build_timeout, log=log)
        app = assemble(output, runtime, bundle, cef)
        manifest = verify_app_engines(app, cef)
        write_json(output / "engines.json", manifest)
        result = {"app": str(app), "output": str(output), "distribution": False}
        if args.smoke:
            result["smoke"] = smoke(
                app, output, timeout=args.smoke_timeout, capture=args.capture
            )
        elif args.run:
            home = create_profile()
            process, _ = launch(app, home, output)
            result.update(
                pid=process.pid,
                home=str(home),
                log=str(output / "browser.log"),
                readiness="not checked; inspect the app and browser.log",
            )
        write_json(output / "result.json", result)
        print(json.dumps(result, indent=2))
        return 0
    except (OSError, ValueError, EngineError, KeyError, TypeError) as error:
        location = f"; retained output: {output}" if output is not None else ""
        parser.exit(1, f"Engine dev launch refused: {error}{location}\n")


if __name__ == "__main__":
    raise SystemExit(main())
