#!/usr/bin/env python3
"""Opt-in, isolated macOS arm64 CEF probe. Default: print a plan, do nothing.

--run permits a bounded native launch; --build additionally permits two bounded
Cargo builds. --capture requires Screen Recording and captures only the owned
root's window. No arbitrary URLs, profiles, engine flags, signing or installation.
Only owned process roots are stopped. Announced helpers are independently
observed for identity, policy and exit; HOME is retained for audit inspection.
"""

import argparse
import http.server
import json
import os
import platform
import re
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit

sys.dont_write_bytecode = True
import bundle_probe
import helper_observer
import prepare_runtime

ROOT = Path(__file__).resolve().parents[2]
TARGET = ROOT / "target"
SDK = TARGET / "cef-runtime/156.0.26/macosarm64"
FIXTURE = ROOT / "tools/cef-probe/fixtures/loopback.html"
MAIN = TARGET / "debug/felinaut-cef-probe"
HELPER = TARGET / "debug/felinaut-cef-probe-helper"
MARKER_NAME = ".felinaut-cef-probe"
MARKER = b"felinaut-cef-runtime-v1\n"
MARKERS = (
    "READY: real CEF loopback document loaded and JavaScript executed",
    "PASS: native text input and trusted pointer click",
    "PASS: CEF viewport followed the GPUI/native container resize",
    "PASS: CEF OnBeforeClose, callback release, pump cancellation and shutdown before GPUI termination",
    "PASS: original GPUI shutdown callback after CEF shutdown",
)
VIEWPORT_MARKER = MARKERS[2]
MAX_LOG_BYTES = 8 * 1024**2
CSP = (
    "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; "
    "connect-src 'none'; frame-src 'none'; frame-ancestors 'none'; "
    "form-action 'none'; base-uri 'none'; object-src 'none'"
)


class ProbeError(RuntimeError):
    pass


def support():
    scripts = str(ROOT / "scripts")
    if scripts not in sys.path:
        sys.path.insert(0, scripts)
    import compare_macos_webkit

    return compare_macos_webkit


def sanitized_environment(inherited):
    """Build-only environment: preserve Cargo/Xcode configuration, scrub engine overrides."""
    return {
        key: value
        for key, value in inherited.items()
        if not key.startswith(
            ("DYLD_", "__XPC_DYLD_", "WEBKIT_", "CEF_", "CHROME_", "FELINAUT_")
        )
        and key not in ("FLATPAK", "NIX_CEF_BINARY")
    }


def system_environment(inherited):
    """Only locale/text encoding is inherited by native executable processes."""
    allowed = ("LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "__CF_USER_TEXT_ENCODING")
    env = {key: inherited[key] for key in allowed if key in inherited}
    env["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
    return env


def capture_environment(inherited):
    # Trusted system capture tools run in the original desktop user's context,
    # not the disposable CEF HOME. Do not forward credentials or engine settings.
    env = system_environment(inherited)
    for key in ("HOME", "TMPDIR"):
        if key in inherited:
            env[key] = inherited[key]
    return env


def build_commands():
    common = [
        "cargo",
        "build",
        "--locked",
        "--manifest-path",
        str(ROOT / "tools/cef-probe/Cargo.toml"),
        "--target-dir",
        str(TARGET),
    ]
    return [
        common + ["--package", "felinaut-cef-probe", "--features", "runtime"],
        common + ["--package", "felinaut-cef-probe-helper"],
    ]


def plan(build=False, capture=False):
    return {
        "mode": "dry-run",
        "sdk": str(SDK),
        "build_commands": build_commands() if build else [],
        "build_timeout_seconds_each": 180,
        "binaries": [str(MAIN), str(HELPER)],
        "output": str(TARGET / "cef-probe-run-<fresh>/felinaut-cef-probe.app"),
        "command": ["<fresh app>/Contents/MacOS/felinaut-cef-probe", "--run-cef"],
        "home": str(Path("/tmp").resolve() / "felinaut-cef-probe-<fresh>"),
        "fixture": "own ephemeral http://127.0.0.1:<port>/ only",
        "native_timeout_seconds": 60,
        "capture": capture,
        "cleanup": "owned roots only; retain HOME and helper audit artifacts",
        "note": "No action without --run; targeted helper identity, policy, canary and exit checks are required",
    }


def private_log(path):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    return os.fdopen(fd, "wb", buffering=0)


def create_output():
    bundle_probe.no_symlink_ancestors(TARGET)
    if not TARGET.is_dir():
        raise ProbeError("Repository target/ must exist with the prepared SDK")
    output = Path(tempfile.mkdtemp(prefix="cef-probe-run-", dir=TARGET)).resolve()
    output.chmod(0o700)
    return output


@dataclass(frozen=True)
class Profile:
    path: Path
    device: int
    inode: int


def validate_profile(home, fresh=False):
    temporary = Path("/tmp").resolve()
    if (
        not home.is_absolute()
        or home.resolve(strict=True) != home
        or home.parent != temporary
        or not home.name.startswith("felinaut-cef-probe-")
    ):
        raise ProbeError("Expected a canonical fresh marked /tmp probe HOME")
    info = home.lstat()
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) != 0o700
    ):
        raise ProbeError("Probe HOME must be an owned private 0700 directory")
    marker = home / MARKER_NAME
    info = marker.lstat()
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_size != len(MARKER)
        or marker.read_bytes() != MARKER
    ):
        raise ProbeError("Probe HOME marker is missing, redirected or invalid")
    if fresh and set(home.iterdir()) != {marker}:
        raise ProbeError("Probe HOME must be fresh before launch")


def create_profile():
    home = Path(
        tempfile.mkdtemp(prefix="felinaut-cef-probe-", dir=Path("/tmp").resolve())
    )
    try:
        home.chmod(0o700)
        with private_log(home / MARKER_NAME) as marker:
            marker.write(MARKER)
        validate_profile(home, fresh=True)
    except BaseException:
        print(f"Preserved HOME {home}: profile preparation failed", file=sys.stderr)
        raise
    info = home.lstat()
    return Profile(home, info.st_dev, info.st_ino)


def confirmed_stopped(roots):
    try:
        return all(root.poll() is not None for root in roots)
    except OSError:
        return False


def finish_profile(profile, success, roots, helper_exit_verified=False):
    # Always retain audit artifacts. A root exit alone is not a helper-exit barrier.
    status = "preserved"
    if not confirmed_stopped(roots):
        reason = "owned root stop is unconfirmed"
    elif not success:
        reason = "unsuccessful run/shutdown"
    elif helper_exit_verified:
        reason = "native audit passed; announced helper exits verified; retained for inspection"
        status = "retained-audit-artifacts"
    else:
        reason = "native root passed; helper exit is unverified"
        status = "retained-helper-exit-unverified"
    print(
        f"Preserved HOME {profile.path}: {reason}",
        file=sys.stderr,
    )
    return status


def launch_environment(inherited, profile, url, canary=None):
    validate_profile(profile.path, fresh=True)
    parsed = urlsplit(url)
    if (
        parsed.scheme != "http"
        or parsed.hostname != "127.0.0.1"
        or parsed.username is not None
        or parsed.password is not None
        or not parsed.port
        or parsed.path != "/"
        or parsed.query
        or parsed.fragment
        or url != f"http://127.0.0.1:{parsed.port}/"
    ):
        raise ProbeError("Only this runner's literal IPv4 loopback root URL is allowed")
    env = system_environment(inherited)
    env.update(
        HOME=str(profile.path),
        CFFIXED_USER_HOME=str(profile.path),
        FELINAUT_CEF_PROBE_HOME=str(profile.path),
        FELINAUT_CEF_PROBE_URL=url,
    )
    if canary is not None:
        env["FELINAUT_CEF_PROBE_CANARY"] = str(canary)
    return env


CANARY_BYTES = b"Felinaut sandbox canary v1\n"


def create_canary(output):
    path = output / "sandbox-canary"
    with private_log(path) as file:
        file.write(CANARY_BYTES)
    return path, validate_canary(path)


def validate_canary(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or os.read(fd, 129) != CANARY_BYTES:
            raise ProbeError(
                "Generated canary changed or is not a regular fixture file"
            )
        return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns
    finally:
        os.close(fd)


def validate_renderer_canaries(text, audit):
    expected = {
        helper["pid"] for helper in audit["helpers"] if helper["role"] == "renderer"
    }
    seen = set()
    for line in text.splitlines():
        if not line.startswith("CEF_RENDERER_CANARY"):
            continue
        match = re.fullmatch(
            r"CEF_RENDERER_CANARY pid=([1-9][0-9]{0,9}) outcome=denied errno=1", line
        )
        if match is None or int(match[1]) not in expected or int(match[1]) in seen:
            return False
        seen.add(int(match[1]))
    return bool(expected) and seen == expected


@contextmanager
def fixture_server():
    prepare_runtime.regular_file(FIXTURE)
    if FIXTURE.stat().st_size > 1024**2:
        raise ProbeError("Oversized loopback fixture")
    body = FIXTURE.read_bytes()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, format, *args):
            pass

        def reply(self, status, content=b"", content_type="text/plain; charset=utf-8"):
            self.send_response(status)
            for key, value in (
                ("Content-Type", content_type),
                ("Content-Length", str(len(content))),
                ("Content-Security-Policy", CSP),
                ("Cache-Control", "no-store"),
                ("X-Content-Type-Options", "nosniff"),
                ("Referrer-Policy", "no-referrer"),
                ("Connection", "close"),
            ):
                self.send_header(key, value)
            self.end_headers()
            if content:
                self.wfile.write(content)
            self.close_connection = True

        def do_GET(self):
            if self.headers.get("Host") != f"127.0.0.1:{self.server.server_port}":
                self.reply(403)
            elif self.path == "/":
                self.reply(200, body, "text/html; charset=utf-8")
            elif self.path == "/favicon.ico":
                self.reply(204)
            else:
                self.reply(404)

        def do_POST(self):
            self.reply(405)

        do_HEAD = do_POST
        do_PUT = do_POST
        do_DELETE = do_POST
        do_OPTIONS = do_POST
        do_PATCH = do_POST
        do_TRACE = do_POST
        do_CONNECT = do_POST

        def send_error(self, code, message=None, explain=None):
            self.reply(code)

    class Server(http.server.ThreadingHTTPServer):
        daemon_threads = True
        allow_reuse_address = False

        def get_request(self):
            connection, address = super().get_request()
            connection.settimeout(1)
            return connection, address

        def handle_error(self, request, client_address):
            pass

    server = Server(("127.0.0.1", 0), Handler)
    worker = threading.Thread(
        target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
    )
    started = False
    try:
        worker.start()
        started = True
        yield f"http://127.0.0.1:{server.server_port}/"
    finally:
        with support().cleanup_signals():
            if started:
                server.shutdown()
            server.server_close()
            if started:
                worker.join(timeout=2)
            if started and worker.is_alive():
                raise ProbeError("Fixture server did not stop")


@contextmanager
def interruption_cleanup():
    previous = {}

    def interrupted(signum, frame):
        raise ProbeError(f"Probe interrupted by signal {signum}")

    try:
        for signum in (signal.SIGINT, signal.SIGTERM):
            previous[signum] = signal.signal(signum, interrupted)
        yield
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def spawn(command, env, log, roots):
    process = subprocess.Popen(
        [str(arg) for arg in command],
        cwd=ROOT,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    roots.append(process)
    return process


def run_command(command, env, log_path, roots, timeout):
    with private_log(log_path) as log:
        process = spawn(command, env, log, roots)
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise ProbeError(
                f"Owned command timed out after {timeout:g}s; inspect {log_path}"
            ) from error
        if code != 0:
            raise ProbeError(f"Owned command exited {code}; inspect {log_path}")


def build_binaries(env, output, roots):
    # This exact SDK validation precedes every Cargo invocation; never rely on
    # cef-dll-sys's permissive version check or its download fallback.
    for index, command in enumerate(build_commands()):
        sdk, _ = bundle_probe.verify_prepared_sdk(SDK, prepare_runtime.Deadline())
        build_env = sanitized_environment(env)
        build_env["CEF_PATH"] = str(sdk)
        run_command(command, build_env, output / f"build-{index + 1}.log", roots, 180)


def compile_capture_helper(env, output, roots):
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
        env,
        output / "capture-build.log",
        roots,
        60,
    )
    return helper


class Budget:
    def __init__(self, seconds=60):
        self.deadline = time.monotonic() + seconds

    def remaining(self, maximum=60):
        value = self.deadline - time.monotonic()
        if value <= 0:
            raise ProbeError("CEF root exceeded the runner's 60-second native budget")
        return min(value, maximum)


def capture_root(root, helper, output, env, roots, budget):
    if root.poll() is not None:
        raise ProbeError("Root exited before its completed-window capture")
    selector_log = output / "window-id.log"
    run_command([helper, str(root.pid)], env, selector_log, roots, budget.remaining(2))
    if selector_log.stat().st_size > 128:
        raise ProbeError("Invalid root window selector output")
    window = selector_log.read_text().strip()
    if not re.fullmatch(r"[1-9][0-9]*", window):
        raise ProbeError("Window selector did not return an owned positive window ID")
    destination = output / "cef-probe-window.png"

    def capture(image):
        if root.poll() is not None:
            raise ProbeError("Root exited before window-only capture")
        run_command(
            ["/usr/sbin/screencapture", "-x", "-o", "-l", window, image],
            env,
            output / "capture.log",
            roots,
            budget.remaining(5),
        )

    # The existing publisher strips PNG metadata in private staging, then renames.
    # Never fall back to a display capture or publish an unsanitized image.
    support().publish_capture(destination, capture)
    return destination


def read_log(path):
    with path.open("rb") as source:
        data = source.read(MAX_LOG_BYTES + 1)
    if len(data) > MAX_LOG_BYTES:
        raise ProbeError("Native log exceeds the bounded inspection limit")
    return data.decode("utf-8", errors="replace")


def validate_completion(text, exit_code):
    lines = text.splitlines()
    if exit_code != 0:
        raise ProbeError(f"Native CEF root exit was {exit_code!r}, not zero")
    if any(line.startswith("FAIL:") for line in lines):
        raise ProbeError("Native log reported a failure")
    if any(lines.count(marker) != 1 for marker in MARKERS):
        raise ProbeError(
            "Native log is missing or duplicates required completion markers"
        )
    positions = [lines.index(marker) for marker in MARKERS]
    if positions != sorted(positions):
        raise ProbeError("Native completion markers appeared out of order")


def monitor(
    root,
    log_path,
    output,
    capture_env,
    roots,
    capture_helper=None,
    budget=None,
    observer=None,
):
    budget = budget or Budget()
    captured = None
    consumed = 0

    def observe(text):
        nonlocal consumed
        end = text.rfind("\n") + 1
        if observer is not None and end > consumed:
            observer.observe(text[consumed:end])
            consumed = end

    while True:
        budget.remaining()
        text = read_log(log_path)
        observe(text)
        lines = text.splitlines()
        if any(line.startswith("FAIL:") for line in lines):
            raise ProbeError("Native failure reported; inspect the private native log")
        if capture_helper is not None and captured is None and VIEWPORT_MARKER in lines:
            captured = capture_root(
                root, capture_helper, output, capture_env, roots, budget
            )
        code = root.poll()
        if code is not None:
            # Read again after confirmed exit so final buffered shutdown output counts.
            final_text = read_log(log_path)
            observe(final_text)
            validate_completion(final_text, code)
            if capture_helper is not None and captured is None:
                raise ProbeError("Requested completed-window capture was not obtained")
            return captured
        time.sleep(min(0.02, budget.remaining()))


def stop_owned(roots):
    try:
        support().stop_owned(roots)
    except support().ComparisonError as error:
        raise ProbeError(
            "Could not stop/reap every owned root; CEF helper lifecycle is unverified"
        ) from error


def execute(build=False, capture=False):
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise ProbeError("Native CEF probe requires a macOS arm64 desktop session")
    bundle_probe.verify_prepared_sdk(SDK, prepare_runtime.Deadline())
    output = create_output()
    print(f"PROBE_OUTPUT={output}", flush=True)
    roots, profile, root = [], None, None
    observer, canary, canary_identity = None, None, None
    success = False
    report = {"success": False, "output": str(output), "helper_lifecycle": "unverified"}
    with interruption_cleanup():
        try:
            inherited = dict(os.environ)
            build_env = sanitized_environment(inherited)
            capture_env = capture_environment(inherited)
            if build:
                build_binaries(build_env, output, roots)
            app = bundle_probe.create_bundle(
                output / "felinaut-cef-probe.app", SDK, MAIN, HELPER
            )
            capture_helper = (
                compile_capture_helper(build_env, output, roots) if capture else None
            )
            canary, canary_identity = create_canary(output)
            profile = create_profile()
            report["home"] = str(profile.path)
            with fixture_server() as url:
                env = launch_environment(inherited, profile, url, canary)
                report["fixture_url"] = url
                with private_log(output / "native.log") as log:
                    budget = Budget()
                    root = spawn(
                        [app / "Contents/MacOS/felinaut-cef-probe", "--run-cef"],
                        env,
                        log,
                        roots,
                    )
                    report["root_pid"] = root.pid
                    observer = helper_observer.HelperObserver(
                        root.pid,
                        app,
                        canary,
                        app
                        / "Contents/Frameworks/Chromium Embedded Framework.framework/Chromium Embedded Framework",
                    )
                    image = monitor(
                        root,
                        output / "native.log",
                        output,
                        capture_env,
                        roots,
                        capture_helper,
                        budget,
                        observer,
                    )
                    report["capture"] = str(image) if image else None
            success = True
        finally:
            with support().cleanup_signals():
                try:
                    stop_owned(roots)
                except ProbeError:
                    success = False
                    raise
                finally:
                    if observer is not None:
                        audit = observer.finish(deadline_seconds=3)
                        report["helper_audit"] = audit
                        try:
                            report["renderer_canary_denied"] = (
                                validate_renderer_canaries(
                                    read_log(output / "native.log"), audit
                                )
                            )
                        except (OSError, ProbeError) as error:
                            report["renderer_canary_denied"] = False
                            report["native_log_error"] = str(error)
                        try:
                            report["canary_control_unchanged"] = (
                                validate_canary(canary) == canary_identity
                            )
                        except (OSError, ProbeError) as error:
                            report["canary_control_unchanged"] = False
                            report["canary_control_error"] = str(error)
                        roles_observed = all(
                            role["count"] > 0 for role in audit["roles"].values()
                        )
                        seatbelt_requested = all(
                            helper["seatbelt"] == 1 for helper in audit["helpers"]
                        )
                        report["notification_broker_absent"] = all(
                            helper.get("subtype")
                            != "mac_notifications.mojom.MacNotificationProvider"
                            for helper in audit["helpers"]
                        )
                        audit_passed = (
                            audit["ok"]
                            and report["notification_broker_absent"]
                            and roles_observed
                            and seatbelt_requested
                            and report["renderer_canary_denied"]
                            and report["canary_control_unchanged"]
                        )
                        report["helper_audit_passed"] = audit_passed
                        report["helper_lifecycle"] = (
                            "verified-announced-helper-exit"
                            if audit["ok"]
                            else "unverified"
                        )
                        success = success and audit_passed
                    if profile is not None:
                        report["home_cleanup"] = finish_profile(
                            profile,
                            success,
                            roots,
                            report["helper_lifecycle"]
                            == "verified-announced-helper-exit",
                        )
                        report["home_retained"] = True
                    report["root_exit"] = root.returncode if root is not None else None
                    report["success"] = success
                    with private_log(output / "report.json") as record:
                        record.write((json.dumps(report, indent=2) + "\n").encode())
        if not success:
            raise ProbeError(
                "Native checks, helper audit or owned-root stop failed; inspect report.json"
            )
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument(
        "--run", action="store_true", help="explicitly permit native execution"
    )
    parser.add_argument(
        "--build",
        action="store_true",
        help="with --run, build main runtime then helper (180s each)",
    )
    parser.add_argument(
        "--capture",
        action="store_true",
        help="with --run, capture only the completed owned window",
    )
    args = parser.parse_args(argv)
    if not args.run:
        print(json.dumps(plan(args.build, args.capture), indent=2))
        return 0
    try:
        report = execute(args.build, args.capture)
    except (RuntimeError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"CEF probe failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
