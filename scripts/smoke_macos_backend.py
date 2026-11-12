#!/usr/bin/env python3
"""Opt-in, loopback-only smoke check of the prebuilt macOS native backend.

First run `cargo build --locked --bins`, then:
    python3 scripts/smoke_macos_backend.py --run [--capture] [--output NEW_DIR]

A fresh private directory under target/ retains logs, result.json and a page PNG.
--capture additionally needs Swift command-line tools, a desktop session and
Screen Recording permission; it captures only this run's browser window, never
an entire display. The short temporary HOME (including the app profile) is
removed on exit. No installed app, existing browser process or profile is used.
Importing this module does not launch processes, build code or create files.
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
import re
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from contextlib import contextmanager
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
PNG_METADATA = {b"eXIf", b"tEXt", b"iTXt", b"zTXt", b"tIME"}
CAPABILITIES = (
    "zoom",
    "audio_control",
    "trusted_input",
    "snapshot",
    "certificate_exceptions",
)


class SmokeError(RuntimeError):
    """A failed smoke check or invalid artifact."""


def strip_png_metadata(data: bytes) -> bytes:
    """Check PNG framing/CRCs and remove textual, EXIF and timestamp chunks.

    Retain image/color chunks verbatim; this is not a pixel decoder. Validate
    even discarded chunks, and reject trailing bytes rather than retaining
    potentially identifying data outside the PNG stream.
    """
    if not data.startswith(PNG_SIGNATURE):
        raise SmokeError("Invalid PNG signature")
    kept = bytearray(PNG_SIGNATURE)
    offset = len(PNG_SIGNATURE)
    seen_header = seen_image = False
    while offset < len(data):
        if len(data) - offset < 12:
            raise SmokeError("Truncated PNG chunk header/CRC")
        length, kind = struct.unpack_from(">I4s", data, offset)
        end = offset + length + 12
        if length > 0x7FFFFFFF or end > len(data):
            raise SmokeError("Truncated or oversized PNG chunk")
        if not re.fullmatch(rb"[A-Za-z]{4}", kind) or kind[2] & 0x20:
            raise SmokeError("Invalid PNG chunk type")
        payload = data[offset + 8 : end - 4]
        crc = struct.unpack_from(">I", data, end - 4)[0]
        if zlib.crc32(kind + payload) != crc:
            raise SmokeError("Invalid PNG chunk CRC")
        if not seen_header and kind != b"IHDR":
            raise SmokeError("PNG must start with IHDR")
        if kind == b"IHDR":
            if seen_header or length != 13:
                raise SmokeError("Invalid or duplicate PNG IHDR")
            width, height = struct.unpack_from(">II", payload)
            if not width or not height:
                raise SmokeError("Invalid PNG dimensions")
            seen_header = True
        if kind == b"IDAT":
            seen_image = True
        if kind not in PNG_METADATA:
            kept.extend(data[offset:end])
        if kind == b"IEND":
            if length or not seen_image or end != len(data):
                raise SmokeError("Invalid PNG IEND, missing image, or trailing data")
            return bytes(kept)
        offset = end
    raise SmokeError("PNG is missing IEND")


def clean_png(path: Path) -> None:
    # Validate before writing so a malformed artifact is not partially rewritten.
    cleaned = strip_png_metadata(path.read_bytes())
    path.write_bytes(cleaned)


def snapshot_ref(snapshot: str, role: str, label: str) -> str:
    quoted = json.dumps(label, ensure_ascii=False)
    matches = re.findall(
        rf"^(@e[0-9]+) {re.escape(role)} {re.escape(quoted)}(?: = .*)?$",
        snapshot,
        re.MULTILINE,
    )
    if len(matches) != 1:
        raise SmokeError(f"Expected one {role} named {label!r}, found {len(matches)}")
    return matches[0]


def validate_backend(info: object) -> dict:
    if not isinstance(info, dict):
        raise SmokeError("Backend diagnostics must be an object")
    if info.get("backend") != "native-webkit" or info.get("engine_family") != "webkit":
        raise SmokeError("Expected the native-webkit / webkit backend")
    version = info.get("runtime_version")
    if not isinstance(version, str) or not version.strip():
        raise SmokeError("Backend runtime_version must be a nonempty string")
    capabilities = info.get("capabilities")
    if not isinstance(capabilities, dict) or any(
        capabilities.get(name) is not True for name in CAPABILITIES
    ):
        raise SmokeError("Native backend must report all five supported capabilities")
    return info


def run(*args: str | Path, env: dict | None = None, timeout: float = 12) -> str:
    command = [str(arg) for arg in args]
    try:
        result = subprocess.run(
            command,
            env=env,
            check=True,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as error:
        raise SmokeError(f"{command[0]} timed out after {timeout:g}s") from error
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "no diagnostic output").strip()
        raise SmokeError(f"{command[0]} failed: {detail}") from error
    except OSError as error:
        raise SmokeError(f"Cannot run {command[0]}: {error}") from error
    return result.stdout.strip()


def wait_for(process, check, description: str, timeout: float = 20):
    deadline = time.monotonic() + timeout
    last_error = None
    while True:
        if process.poll() is not None:
            raise SmokeError("Browser exited; inspect browser.log")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            detail = f": {last_error}" if last_error else ""
            raise SmokeError(f"Timed out waiting for {description}{detail}")
        try:
            result = check(min(12, remaining))
            if result:
                return result
        except SmokeError as error:
            last_error = error
        time.sleep(min(0.1, max(0, deadline - time.monotonic())))


def stop_browser(process) -> None:
    if process.poll() is not None:
        return
    try:
        process.terminate()
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


@contextmanager
def fixture_server():
    html = (ROOT / "scripts/fixtures/native_backend.html").read_bytes()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path != "/":
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(html)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(html)

        def log_message(self, format, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    try:
        thread.start()
        yield f"http://127.0.0.1:{server.server_port}/"
    finally:
        if thread.is_alive():
            server.shutdown()
            thread.join(timeout=5)
        server.server_close()


def create_output(path: Path | None) -> Path:
    if path is not None:
        output = path.expanduser().absolute()
        # No exist_ok: even an empty existing directory or symlink is refused.
        output.mkdir(mode=0o700)
        return output
    target = ROOT / "target"
    target.mkdir(exist_ok=True)
    return Path(tempfile.mkdtemp(prefix="macos-backend-", dir=target))


def isolated_environment(home: Path) -> dict:
    profile = home / "Library/Application Support/Felifox"
    profile.mkdir(parents=True)
    (profile / "settings").write_text(
        "theme=milky-way\nanimate=false\nsidebar=true\n", encoding="utf-8"
    )
    return dict(
        os.environ, HOME=str(home), CFFIXED_USER_HOME=str(home), FELINAUT_AGENT="1"
    )


def capture_window(process, helper: Path, destination: Path, env: dict) -> None:
    if process.poll() is not None:
        raise SmokeError("Browser exited before window capture")
    window = run(helper, str(process.pid), env=env, timeout=10)
    if not re.fullmatch(r"[1-9][0-9]*", window):
        raise SmokeError("Window helper did not return a positive window ID")
    time.sleep(0.3)
    # Never fall back to display capture, including when permissions are missing.
    run(
        "/usr/sbin/screencapture",
        "-x",
        "-o",
        "-l",
        window,
        destination,
        env=env,
        timeout=10,
    )
    clean_png(destination)


def single_tab_at(tabs: object, url: str, *, loaded: bool = False) -> bool:
    return (
        isinstance(tabs, list)
        and len(tabs) == 1
        and isinstance(tabs[0], dict)
        and tabs[0].get("url") == url
        and (not loaded or tabs[0].get("loading") is False)
    )


def smoke(output: Path, capture: bool) -> dict:
    browser = ROOT / "target/debug/felinaut"
    client = ROOT / "target/debug/felinaut-ctl"
    for binary in (browser, client):
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise SmokeError(
                f"Missing executable {binary}; run cargo build --locked --bins first"
            )

    # macOS's per-user TMPDIR is often too long for the agent's Unix socket.
    with tempfile.TemporaryDirectory(prefix="fn-", dir="/tmp") as temporary:
        env = isolated_environment(Path(temporary).resolve())
        helper = output / "backend-window"
        if capture:
            run(
                "/usr/bin/xcrun",
                "swiftc",
                ROOT / "scripts/backend_window.swift",
                "-module-cache-path",
                output / "swift-module-cache",
                "-o",
                helper,
                env=env,
                timeout=60,
            )

        def ctl(*args, timeout=12):
            return run(client, *args, env=env, timeout=timeout)

        with fixture_server() as url, (output / "browser.log").open("w") as log:
            process = subprocess.Popen([str(browser)], env=env, stdout=log, stderr=log)
            try:
                before = validate_backend(
                    wait_for(
                        process,
                        lambda timeout: json.loads(ctl("backend", timeout=timeout)),
                        "control socket",
                    )
                )
                if not single_tab_at(json.loads(ctl("tabs")), "felinaut://newtab"):
                    raise SmokeError("Fresh profile did not open a single New Tab")
                if capture:
                    time.sleep(1)
                    capture_window(
                        process, helper, output / "macos-native-webkit-home.png", env
                    )
                ctl("navigate", url)
                wait_for(
                    process,
                    lambda timeout: single_tab_at(
                        json.loads(ctl("tabs", timeout=timeout)), url, loaded=True
                    ),
                    "loopback page to finish loading",
                )
                reference = snapshot_ref(ctl("snapshot"), "textbox", "Test input")
                ctl("--native", "type", reference, "Hello from Felinaut")
                reference = snapshot_ref(ctl("snapshot"), "button", "Verify input")
                ctl("--native", "click", reference)
                snapshot = ctl("snapshot")
                (output / "snapshot.txt").write_text(snapshot + "\n", encoding="utf-8")
                result = json.loads(
                    ctl(
                        "eval",
                        "({text: result.textContent, trusted: result.dataset.trusted})",
                    )
                )
                if result != {
                    "text": "Native input verified: Hello from Felinaut",
                    "trusted": "true",
                }:
                    raise SmokeError(
                        f"Trusted native input/click check failed: {result!r}"
                    )
                page = output / "page-snapshot.png"
                ctl("screenshot", str(page))
                clean_png(page)
                after = validate_backend(json.loads(ctl("backend")))
                if after != before:
                    raise SmokeError("Backend diagnostics changed after navigation")
                if capture:
                    capture_window(
                        process, helper, output / "macos-native-webkit-page.png", env
                    )
                report = {
                    "backend": after,
                    "native_input": result,
                    "page_snapshot_bytes": page.stat().st_size,
                    "window_capture": capture,
                }
            finally:
                stop_browser(process)
    (output / "result.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8"
    )
    return report


def interrupt(signum, frame):
    raise KeyboardInterrupt


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--run", action="store_true", help="explicitly allow launching the test browser"
    )
    parser.add_argument(
        "--capture",
        action="store_true",
        help="also compile Swift and capture only the browser window",
    )
    parser.add_argument(
        "--output", type=Path, help="new output directory (must not already exist)"
    )
    args = parser.parse_args(argv)
    if not args.run:
        parser.error("--run is required; this check launches a real browser")
    if sys.platform != "darwin":
        parser.error("this smoke check requires macOS")
    previous = signal.signal(signal.SIGTERM, interrupt)
    try:
        output = create_output(args.output)
        print(f"Artifacts: {output}", flush=True)
        smoke(output, args.capture)
    except (SmokeError, OSError, ValueError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print(
            "Interrupted; stopped the test browser and loopback server", file=sys.stderr
        )
        return 130
    finally:
        signal.signal(signal.SIGTERM, previous)
    print(
        "PASS: backend diagnostics, trusted native input/click, JavaScript and page snapshot"
        + ("; app-window captures saved" if args.capture else " (no desktop capture)")
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
