#!/usr/bin/env python3
"""Trusted-loopback developer comparison of system and upstream Cocoa WebKit.

Build system only: cargo build --locked --bins
Build both: cargo build --locked --bins --features engine-webkit-upstream
(Add -F gpui/runtime_shaders if needed.)
  python3 scripts/compare_macos_webkit.py                     # native picker
  python3 scripts/compare_macos_webkit.py --engine both       # until windows close
  python3 scripts/compare_macos_webkit.py --engine both --smoke [--capture]

--smoke without --engine selects both, never the picker, and requires both built
adapters. Use --engine system --smoke with the default system-only build.
The bounded --list-engines preflight checks compiled adapters, not availability of
external runtime products. The picker disables choices excluded from this build.
Actual loaded source/UUID checks and upstream products validation remain mandatory.
No arbitrary URL input,
installed app, downloads, signing changes, security overrides or dependencies.
The Rust comparison mode must enforce nonpersistent stores and loopback-only
navigation. Each owned browser root gets a marked temporary HOME, removed only
when that root is confirmed stopped; otherwise its HOME is preserved and reported.
Cleanup only manages owned browser roots; WebKit helper lifecycle is unverified.
Logs/sanitized PNGs remain in a fresh private target/ directory. --capture
adds actual window-only UI captures and requires Screen Recording permission.
Manifest preflight has a 12s timeout. Smoke work has one 65s budget plus bounded
root cleanup; optional Swift helper compilation has a separate 60s timeout. Interactive waiting is unbounded.

Default products: official archive 326536@main,
0d0d05bf3c5e563157c36caf89ecbc9d9c51bf78. Reference archive SHA256:
314ddb19d3a2a320135fe8c1faa6c5e250bf4d2d345780c5cf05129e540b133d.
The already-extracted tree is trusted local developer input, NOT a verified
archive or distribution bundle. UUID checks prove the loaded WebKit selection,
not archive authenticity or signatures. This is Cocoa WebKit, not WPE; running
both is not a performance benchmark. No product files are modified.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import plistlib
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from contextlib import ExitStack, contextmanager
from pathlib import Path

import smoke_macos_backend as smoke

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_PRODUCTS = ROOT / "target/engine-comparison/upstream/Release"
MARKER = b"felinaut-comparison-v1\n"
UUID_PATTERN = r"[0-9A-Fa-f]{8}(?:-[0-9A-Fa-f]{4}){3}-[0-9A-Fa-f]{12}"
FRAMEWORKS = ("WebKit", "WebCore", "JavaScriptCore", "WebGPU", "WebKitLegacy")
SERVICES = (
    "WebContent",
    "WebContent.Development",
    "WebContent.CaptivePortal",
    "WebContent.EnhancedSecurity",
    "Networking",
    "GPU",
    "Model",
)
DYLD_PATHS = (
    "DYLD_FRAMEWORK_PATH",
    "DYLD_LIBRARY_PATH",
    "__XPC_DYLD_FRAMEWORK_PATH",
    "__XPC_DYLD_LIBRARY_PATH",
)
STORAGE_KEY = "felinaut-comparison-isolation-v1"
NATIVE_ENGINES = {
    "system-webkit": ("system", "engine-webkit-system"),
    "upstream-webkit": ("upstream", "engine-webkit-upstream"),
}
REBUILD = "cargo build --locked --bins --features engine-webkit-upstream"


class ComparisonError(smoke.SmokeError):
    """Invalid comparison policy, products, or observed runtime."""


class Budget:
    def __init__(self, seconds=65):
        self.deadline = time.monotonic() + seconds

    def timeout(self, maximum=12):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise ComparisonError("Comparison exceeded its shared 65s work budget")
        return min(maximum, remaining)


def canonical_uuid(value: object) -> str:
    if not isinstance(value, str) or not re.fullmatch(UUID_PATTERN, value):
        raise ComparisonError("Expected a canonical WebKit image UUID")
    return value.upper()


def extract_uuid(output: str, architecture: str) -> str:
    matches = []
    for line in output.splitlines():
        match = re.fullmatch(rf"UUID: ({UUID_PATTERN}) \(([^)]+)\) .+", line)
        if match and match[2] == architecture:
            matches.append(canonical_uuid(match[1]))
    if len(matches) != 1:
        raise ComparisonError(
            f"dwarfdump must report exactly one {architecture} UUID; found {len(matches)}"
        )
    return matches[0]


def sanitized_environment(inherited: dict) -> dict:
    return {
        key: value
        for key, value in inherited.items()
        if not key.startswith(
            ("DYLD_", "__XPC_DYLD_", "WEBKIT_", "FELINAUT_COMPARISON")
        )
        and key != "FELINAUT_EXPECTED_WEBKIT_UUID"
    }


def parse_engine_manifest(text):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"Duplicate manifest field: {key}")
            result[key] = value
        return result

    try:
        info = json.loads(text, object_pairs_hook=unique_object)
        if (
            not isinstance(info, dict)
            or type(info.get("schema_version")) is not int
            or info["schema_version"] != 1
            or not isinstance(info.get("engines"), list)
        ):
            raise ValueError("Expected schema_version=1 and an engines array")
        engines = {}
        features = set()
        for entry in info["engines"]:
            if not isinstance(entry, dict):
                raise ValueError("Engine entries must be objects")
            for field in ("id", "feature", "family"):
                if not isinstance(entry.get(field), str) or not re.fullmatch(
                    r"[a-z][a-z0-9-]*", entry[field]
                ):
                    raise ValueError(f"Invalid engine {field}")
            if not entry["feature"].startswith("engine-"):
                raise ValueError("Invalid engine feature name")
            for field in ("built", "adapter_implemented", "requires_isolated_launcher"):
                if type(entry.get(field)) is not bool:
                    raise ValueError(f"Engine {field} must be a boolean")
            if not isinstance(entry.get("limitation"), str):
                raise ValueError("Engine limitation must be a string")
            if entry["id"] in engines or entry["feature"] in features:
                raise ValueError("Duplicate engine id or feature")
            engines[entry["id"]] = entry
            features.add(entry["feature"])
        modes = []
        for identifier, (mode, feature) in NATIVE_ENGINES.items():
            entry = engines.get(identifier)
            if (
                entry is None
                or entry["feature"] != feature
                or entry["family"] != "webkit"
            ):
                raise ValueError(f"Missing/inconsistent native engine: {identifier}")
            if entry["built"] and entry["adapter_implemented"]:
                modes.append(mode)
        if "system" in modes and "upstream" in modes:
            modes.append("both")
        return tuple(modes)
    except (ValueError, TypeError, RecursionError) as error:
        raise ComparisonError(
            f"Missing/old/malformed --list-engines manifest: {error}. Rebuild: {REBUILD}"
        ) from error


def built_modes(env, timeout=12):
    browser = ROOT / "target/debug/felinaut"
    if not browser.is_file() or not os.access(browser, os.X_OK):
        raise ComparisonError(f"Missing executable {browser}; rebuild: {REBUILD}")
    # Old binaries may ignore --list-engines. Do not give them a user's profile
    # or enable agent control while waiting for the bounded preflight to fail.
    environment = sanitized_environment(env)
    environment.update(
        HOME="/dev/null", CFFIXED_USER_HOME="/dev/null", FELINAUT_AGENT="0"
    )
    try:
        text = smoke.run(browser, "--list-engines", env=environment, timeout=timeout)
    except smoke.SmokeError as error:
        raise ComparisonError(
            f"Cannot read --list-engines manifest; rebuild: {REBUILD}. {error}"
        ) from error
    return parse_engine_manifest(text)


def require_mode(engine, supported):
    if engine not in ("system", "upstream", "both"):
        raise ComparisonError("Engine must be system, upstream or both")
    if engine not in supported:
        raise ComparisonError(
            f"Requested {engine} comparison adapter(s) excluded or not implemented in "
            f"this build; rebuild: {REBUILD}. Runtime products are checked separately"
        )


def validate_home(home: Path) -> None:
    if (
        not home.is_absolute()
        or home.resolve() != home
        or home.parent != Path("/tmp").resolve()
        or not home.name.startswith("felinaut-compare-")
        or not home.is_dir()
        or stat.S_IMODE(home.stat().st_mode) != 0o700
        or (home / ".felinaut-comparison").is_symlink()
        or (home / ".felinaut-comparison").read_bytes() != MARKER
    ):
        raise ComparisonError(
            "Comparison requires a canonical marked private /tmp HOME"
        )


@contextmanager
def cleanup_signals():
    # Repeated interrupts must not bypass the remaining bounded root waits or
    # filesystem cleanup. Restore the caller's handlers, including on failure.
    with ExitStack() as stack:
        for signum in (signal.SIGINT, signal.SIGTERM):
            previous = signal.signal(signum, signal.SIG_IGN)
            stack.callback(signal.signal, signum, previous)
        yield


@contextmanager
def private_home(roots=()):
    # No TemporaryDirectory finalizer: an unconfirmed root must retain its HOME.
    home = Path(tempfile.mkdtemp(prefix="felinaut-compare-", dir="/tmp")).resolve()
    try:
        home.chmod(0o700)
        marker = home / ".felinaut-comparison"
        marker.write_bytes(MARKER)
        marker.chmod(0o600)
        # Reuse the smoke profile's deterministic theme/Reduce Motion settings.
        smoke.isolated_environment(home)
        validate_home(home)
        yield home
    finally:
        with cleanup_signals():
            unconfirmed = []
            for root in roots:
                try:
                    stopped = root.poll() is not None
                except OSError:
                    stopped = False
                if not stopped:
                    unconfirmed.append(str(root.pid))
            if unconfirmed:
                print(
                    f"Preserved temporary HOME {home}: owned browser root PID(s) "
                    f"{', '.join(unconfirmed)} not confirmed stopped; "
                    "WebKit helper lifecycle is unverified",
                    file=sys.stderr,
                )
            else:
                shutil.rmtree(home)


def browser_environment(inherited, home, engine, products=None, expected_uuid=None):
    if engine not in ("system", "upstream"):
        raise ComparisonError("Engine must be system or upstream")
    validate_home(home)
    env = sanitized_environment(inherited)
    env.update(
        HOME=str(home),
        CFFIXED_USER_HOME=str(home),
        FELINAUT_AGENT="1",
        FELINAUT_COMPARISON=engine,
        FELINAUT_COMPARISON_HOME=str(home),
    )
    if engine == "upstream":
        if (
            products is None
            or not products.is_absolute()
            or products.resolve() != products
        ):
            raise ComparisonError(
                "Upstream requires an absolute canonical products path"
            )
        env["FELINAUT_EXPECTED_WEBKIT_UUID"] = canonical_uuid(expected_uuid)
        env.update({key: str(products) for key in DYLD_PATHS})
        env["WEBKIT_UNSET_DYLD_FRAMEWORK_PATH"] = "YES"
    elif products is not None or expected_uuid is not None:
        raise ComparisonError("System selection must not inject an external framework")
    return env


def validate_identity(info, engine, expected_uuid=None):
    smoke.validate_backend(info)
    if engine not in ("system", "upstream"):
        raise ComparisonError("Unknown engine selection")
    if engine == "upstream" and expected_uuid is None:
        raise ComparisonError(
            "Upstream identity verification requires an expected UUID"
        )
    identity = info.get("runtime_identity")
    source = "system-shared-cache" if engine == "system" else "external-framework"
    if not isinstance(identity, dict) or identity.get("source") != source:
        raise ComparisonError(
            f"{engine}: loaded runtime source must be {source}; no fallback"
        )
    observed = canonical_uuid(identity.get("image_uuid"))
    if identity["image_uuid"] != observed:
        raise ComparisonError(
            "Runtime diagnostics must use an uppercase canonical UUID"
        )
    if expected_uuid is not None and observed != canonical_uuid(expected_uuid):
        raise ComparisonError(
            f"{engine}: loaded WebKit UUID does not match products; no fallback"
        )
    return info


def contained(path: Path, products: Path) -> Path:
    try:
        resolved = path.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise ComparisonError(f"Missing/broken products path: {path}") from error
    if not resolved.is_relative_to(products):
        raise ComparisonError(f"Required runtime path escapes products: {path}")
    return resolved


def image_uuid(path, architecture, env, budget):
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ComparisonError(f"Missing executable: {path}")
    return extract_uuid(
        smoke.run(
            "/usr/bin/xcrun",
            "dwarfdump",
            "--uuid",
            path,
            env=env,
            timeout=budget.timeout(),
        ),
        architecture,
    )


def validate_products(path, architecture, env, budget):
    try:
        products = path.expanduser().resolve(strict=True)
    except (OSError, RuntimeError) as error:
        raise ComparisonError(f"Missing upstream products directory: {path}") from error
    if not products.is_dir():
        raise ComparisonError("Upstream products must be a directory")
    webkit_uuid = None
    for name in FRAMEWORKS:
        framework = products / f"{name}.framework"
        for link in (
            framework / "Versions/Current",
            framework / name,
            framework / "Resources",
        ):
            if not link.is_symlink():
                raise ComparisonError(f"Required framework symlink missing: {link}")
            contained(link, products)
        if not contained(framework / "Resources/Info.plist", products).is_file():
            raise ComparisonError(f"Missing framework Info.plist: {framework}")
        value = image_uuid(
            contained(framework / name, products), architecture, env, budget
        )
        if name == "WebKit":
            webkit_uuid = value
    xpcs = products / "WebKit.framework/XPCServices"
    if not xpcs.is_symlink():
        raise ComparisonError("WebKit.framework/XPCServices symlink is missing")
    contained(xpcs, products)
    for service in SERVICES:
        contents = xpcs / f"com.apple.WebKit.{service}.xpc/Contents"
        try:
            with contained(contents / "Info.plist", products).open("rb") as source:
                info = plistlib.load(source)
        except (plistlib.InvalidFileException, ValueError) as error:
            raise ComparisonError(f"Malformed XPC Info.plist: {contents}") from error
        executable = info.get("CFBundleExecutable") if isinstance(info, dict) else None
        if (
            not isinstance(executable, str)
            or not re.fullmatch(r"[A-Za-z0-9_.-]+", executable)
            or executable in (".", "..")
        ):
            raise ComparisonError(f"Invalid XPC executable name: {contents}")
        image_uuid(
            contained(contents / "MacOS" / executable, products),
            architecture,
            env,
            budget,
        )
    return products, webkit_uuid


def create_output(path=None):
    target = (ROOT / "target").resolve()
    target.mkdir(exist_ok=True)
    if path is None:
        return Path(tempfile.mkdtemp(prefix="webkit-comparison-", dir=target))
    output = path.expanduser().absolute()
    if not output.resolve().is_relative_to(target):
        raise ComparisonError(
            "Comparison output must be a fresh directory under target/"
        )
    output.mkdir(mode=0o700)  # Existing directories/symlinks are intentionally refused.
    return output


def compile_helper(source, destination, env):
    smoke.run(
        "/usr/bin/xcrun",
        "swiftc",
        ROOT / "scripts" / source,
        "-module-cache-path",
        destination.parent / "swift-module-cache",
        "-o",
        destination,
        env=env,
        timeout=60,
    )


def choose_engine(output, env, supported):
    modes = tuple(supported)
    if (
        not modes
        or any(mode not in ("system", "upstream", "both") for mode in modes)
        or len(set(modes)) != len(modes)
        or ("both" in modes) != ("system" in modes and "upstream" in modes)
    ):
        raise ComparisonError(
            f"Invalid/empty supported picker modes; rebuild: {REBUILD}"
        )
    helper = output / "choose-webkit"
    compile_helper("choose_webkit.swift", helper, env)
    # User-driven picker, not part of bounded --smoke (which never calls this).
    selected = smoke.run(helper, "--supported-modes", *modes, env=env, timeout=None)
    if selected not in (*modes, "cancel"):
        raise ComparisonError("Native picker returned an unknown or excluded engine")
    return None if selected == "cancel" else selected


def ctl_json(ctl, *args, timeout=12):
    try:
        return json.loads(ctl(*args, timeout=timeout))
    except (ValueError, TypeError) as error:
        raise ComparisonError("Control client returned malformed JSON") from error


def verify_storage(ctl, expected):
    # ctl prints bare strings and nothing for null; wrap in an object so both
    # absence and string sentinels retain their JSON types over its text protocol.
    value = ctl_json(
        ctl, "eval", f"({{value: localStorage.getItem({json.dumps(STORAGE_KEY)})}})"
    )
    if value != {"value": expected}:
        raise ComparisonError("Loopback localStorage isolation check failed")


@cleanup_signals()
def stop_owned(processes):
    # Terminate all owned roots before waiting; never signal an existing app or a
    # process group that might contain another user's WebKit XPC processes.
    failures = []
    for process in processes:
        try:
            if process.poll() is None:
                process.terminate()
        except ProcessLookupError:
            pass
        except OSError as error:
            failures.append(error)
    for process in processes:
        try:
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                process.wait(timeout=1)
        except (OSError, subprocess.TimeoutExpired) as error:
            # A failure to reap one child must not bypass stopping the others.
            failures.append(error)
    if failures:
        raise ComparisonError(
            "Could not stop/reap every owned browser root; "
            "WebKit helper lifecycle is unverified"
        ) from failures[0]


def publish_capture(destination, capture):
    staging = Path(tempfile.mkdtemp(prefix=".capture-", dir=destination.parent))
    try:
        image = staging / "capture.png"
        capture(image)
        smoke.clean_png(image)
        # Same-filesystem rename publishes only a completely sanitized PNG.
        image.replace(destination)
    finally:
        with cleanup_signals():
            shutil.rmtree(staging)


def exercise(process, ctl, url, output, engine, expected_uuid, budget):
    before = validate_identity(
        smoke.wait_for(
            process,
            lambda timeout: ctl_json(ctl, "backend", timeout=timeout),
            f"{engine} control socket",
            timeout=budget.timeout(15),
        ),
        engine,
        expected_uuid,
    )
    ctl("navigate", url)
    smoke.wait_for(
        process,
        lambda timeout: smoke.single_tab_at(
            ctl_json(ctl, "tabs", timeout=timeout),
            url,
            loaded=True,
        ),
        f"{engine} loopback fixture",
        timeout=budget.timeout(15),
    )
    reference = smoke.snapshot_ref(ctl("snapshot"), "textbox", "Test input")
    ctl("--native", "type", reference, "Hello from Felinaut")
    reference = smoke.snapshot_ref(ctl("snapshot"), "button", "Verify input")
    ctl("--native", "click", reference)
    result = ctl_json(
        ctl, "eval", "({text: result.textContent, trusted: result.dataset.trusted})"
    )
    if result != {
        "text": "Native input verified: Hello from Felinaut",
        "trusted": "true",
    }:
        raise ComparisonError(
            f"{engine}: trusted native input/click verification failed"
        )
    (output / f"{engine}-snapshot.txt").write_text(
        ctl("snapshot") + "\n", encoding="utf-8"
    )
    verify_storage(ctl, None)
    sentinel = engine + "-" + secrets.token_hex(16)
    ctl(
        "eval",
        f"localStorage.setItem({json.dumps(STORAGE_KEY)}, {json.dumps(sentinel)})",
    )
    verify_storage(ctl, sentinel)
    page = output / f"{engine}-page.png"
    publish_capture(page, lambda image: ctl("screenshot", str(image)))
    after = validate_identity(ctl_json(ctl, "backend"), engine, expected_uuid)
    if after != before:
        raise ComparisonError(f"{engine}: backend diagnostics changed after navigation")
    return {
        "backend": after,
        "native_input": result,
        "page_snapshot_bytes": page.stat().st_size,
    }, sentinel


def compare(output, engine, smoke_mode, capture, products_path=DEFAULT_PRODUCTS):
    if engine not in ("system", "upstream", "both"):
        raise ComparisonError("Engine must be system, upstream or both")
    architecture = platform.machine()
    if architecture not in ("arm64", "x86_64"):
        raise ComparisonError(f"Unsupported macOS host architecture: {architecture}")
    clean_env = sanitized_environment(os.environ)
    require_mode(engine, built_modes(clean_env))
    helper = output / "backend-window"
    if capture:
        compile_helper("backend_window.swift", helper, clean_env)
    budget = Budget()
    browser, client = (
        ROOT / "target/debug" / name for name in ("felinaut", "felinaut-ctl")
    )
    for binary in (browser, client):
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ComparisonError(
                f"Missing executable {binary}; run cargo build --locked --bins first"
            )
        image_uuid(binary, architecture, clean_env, budget)
    engines = ("system", "upstream") if engine == "both" else (engine,)
    products = expected = None
    if "upstream" in engines:
        products, expected = validate_products(
            products_path, architecture, clean_env, budget
        )
    report = {
        "mode": "smoke" if smoke_mode else "interactive",
        "host_architecture": architecture,
        "window_capture": capture,
        "cross_engine_storage_isolation": len(engines) == 2,
        "cleanup_scope": "owned-browser-roots-only",
        "webkit_helper_lifecycle": "unverified",
        "note": "Trusted local Cocoa WebKit developer comparison, not a benchmark or distribution bundle; extracted archive authenticity not verified",
        "engines": {},
    }
    processes = []
    controls = []
    with ExitStack() as stack:
        url = stack.enter_context(smoke.fixture_server())  # ONE origin for both stores.
        try:
            for selected in engines:
                roots = []
                home = stack.enter_context(private_home(roots))
                env = browser_environment(
                    os.environ,
                    home,
                    selected,
                    products if selected == "upstream" else None,
                    expected if selected == "upstream" else None,
                )
                control_env = sanitized_environment(env)

                def ctl(*args, timeout=12, environment=control_env):
                    return smoke.run(
                        client, *args, env=environment, timeout=budget.timeout(timeout)
                    )

                log = stack.enter_context(
                    (output / f"{selected}-browser.log").open("w")
                )
                process = subprocess.Popen(
                    [str(browser)], env=env, stdout=log, stderr=log
                )
                processes.append(process)
                roots.append(process)
                result, sentinel = exercise(
                    process,
                    ctl,
                    url,
                    output,
                    selected,
                    expected if selected == "upstream" else None,
                    budget,
                )
                controls.append((process, ctl, sentinel))
                report["engines"][selected] = result
                identity = result["backend"]["runtime_identity"]
                print(
                    f"{selected}: {identity['source']} {identity['image_uuid']} "
                    f"(WebKit {result['backend']['runtime_version']})",
                    flush=True,
                )
                if capture:
                    # Existing capture helper has two 10s calls plus a short wait.
                    if budget.timeout(65) < 22:
                        raise ComparisonError(
                            "Insufficient shared time budget for window capture"
                        )
                    publish_capture(
                        output / f"{selected}-window.png",
                        lambda image: smoke.capture_window(
                            process, helper, image, control_env
                        ),
                    )
            for process, ctl, sentinel in controls:
                if process.poll() is not None:
                    raise ComparisonError(
                        "Browser exited before store isolation verification"
                    )
                verify_storage(ctl, sentinel)
            if not smoke_mode:
                print(
                    "Verified. Close the comparison windows to finish; Ctrl-C requests "
                    "owned browser root cleanup. WebKit helper lifecycle is unverified.",
                    flush=True,
                )
                while any(process.poll() is None for process in processes):
                    time.sleep(0.2)
                if any(process.returncode != 0 for process in processes):
                    raise ComparisonError(
                        "Comparison browser exited unsuccessfully; inspect private logs"
                    )
        finally:
            with cleanup_signals():
                try:
                    stop_owned(processes)
                finally:
                    stack.close()
    (output / "result.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def parser():
    result = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    result.add_argument(
        "--engine",
        choices=("system", "upstream", "both"),
        help="default: build-aware native picker; with --smoke: both "
        "(requires --features engine-webkit-upstream); system works with default features",
    )
    result.add_argument(
        "--smoke",
        action="store_true",
        help="verify and terminate within a bounded budget, without a picker",
    )
    result.add_argument(
        "--capture",
        action="store_true",
        help="also capture each real app window (Screen Recording permission required)",
    )
    result.add_argument(
        "--products",
        type=Path,
        default=DEFAULT_PRODUCTS,
        help="trusted extracted Cocoa WebKit Release products; never modified",
    )
    result.add_argument(
        "--output",
        type=Path,
        help="fresh private artifact directory under target/ (must not exist)",
    )
    return result


def main(argv=None):
    arguments = parser()
    args = arguments.parse_args(argv)
    if sys.platform != "darwin":
        arguments.error("this comparison requires a macOS desktop session")
    previous = signal.signal(signal.SIGTERM, smoke.interrupt)
    try:
        output = create_output(args.output)
        print(f"Private artifacts: {output}", flush=True)
        engine = args.engine or (
            "both"
            if args.smoke
            else choose_engine(
                output, sanitized_environment(os.environ), built_modes(os.environ)
            )
        )
        if engine is None:
            return 0
        compare(output, engine, args.smoke, args.capture, args.products)
    except (smoke.SmokeError, OSError, ValueError, RuntimeError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print(
            "Interrupted; owned browser root cleanup attempted; "
            "see any preserved-HOME diagnostics. WebKit helper lifecycle is unverified",
            file=sys.stderr,
        )
        return 130
    finally:
        signal.signal(signal.SIGTERM, previous)
    print(
        "PASS: actual runtime identity, trusted input/click, JavaScript, page PNG and "
        "store isolation; owned browser roots stopped; WebKit helper lifecycle "
        "unverified; no performance claim"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
