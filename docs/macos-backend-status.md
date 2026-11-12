# macOS backend implementation status

**Comparison ready: no.** This work validates the native WebKit path behind a first engine-neutral boundary. It does not implement WPE, an engine selector, or live engine switching.

## Native evidence

Checked on **12 November 2026**, **macOS 27.0 (26A428), arm64**, with full Xcode/Metal available. The native seam and diagnostics are in implementation commits `9fda117` and `5546661` on `port-macos-backend-boundary`, based on `a1c9c80` from `origin/multiplatform`. The final smoke/capture run rebuilt the Rust binaries at `5546661`; only the validation tooling and documentation were then uncommitted. The embedded Git hash identifies source history, not a guarantee that the entire working tree is clean.

- [`src/page.rs`](../src/page.rs) defines the browser-facing `PageBackend`, shared page events, optional capabilities and safe diagnostic metadata. Browser tabs, live-folder pages and automation now use that boundary instead of storing a concrete `NativePage`.
- [`src/page/macos.rs`](../src/page/macos.rs) adapts the existing WKWebView implementation. Native objects and existing callbacks remain in [`src/webview.rs`](../src/webview.rs); download internals remain macOS-specific. No native view/process/storage implementation was replaced.
- Optional operations default to explicit unsupported errors, including an error callback for unsupported snapshots. Required navigation/presentation operations have no default successful no-ops.
- `felinaut-ctl backend` reports the current backend, engine family, runtime version and the first optional-capability flags. It works on New Tab without loading a website and does not report URLs, profile paths or account information.

Observed diagnostic output on this machine:

```json
{
  "backend": "native-webkit",
  "engine_family": "webkit",
  "runtime_version": "22625.1.29.11.27",
  "capabilities": {
    "zoom": true,
    "audio_control": true,
    "trusted_input": true,
    "snapshot": true,
    "certificate_exceptions": true
  }
}
```

The version is the installed WebKit framework version reported by Wry, not a Safari marketing version or a claim that every WebKit media/site feature works. The result is an observation, not a required version for another machine.

## Validation performed

Passed:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --bins
cargo build --locked --bins
cargo fmt --manifest-path tools/dist/Cargo.toml --all --check
cargo clippy --locked --manifest-path tools/dist/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path tools/dist/Cargo.toml
python3 -m unittest discover -s scripts -p 'test_*.py'
FELINAUT_NATIVE_TEST=1 cargo test --locked --test native_webview
FELINAUT_NATIVE_TEST=1 cargo test --locked --test native_audio
FELINAUT_NATIVE_TEST=1 cargo test --locked --test brand_ui
python3 scripts/smoke_macos_backend.py --run --capture --output target/native-backend-commit-validation
```

Results: **129 browser unit tests passed, 2 ignored; 4 control-client tests passed; 1 packaging-launcher test passed; 190 Python tests passed, including 28 tests for the preserved smoke tooling.** All three opt-in native harnesses actually ran and printed their `PASS` markers; they were not default skipped runs. Native webview testing exercised adapter metadata, rejected schemes, zoom, JavaScript/DOM/storage/history, failed loads, link events and download confinement/hidden-page denial. Native audio and brand/UI tests also passed. WebKit printed two sandbox-extension file warnings in the webview harness; the behavioral checks still passed. This is not a separate audit of Apple's sandbox.

The reproducible rebuilt-browser [smoke tool](../scripts/smoke_macos_backend.py) used a temporary app-managed HOME and a local HTTP fixture, driven by `felinaut-ctl`. It also sets the child process's `CFFIXED_USER_HOME`; full production profile selection/isolation remains separate contract work:

- Verified backend diagnostics before navigation and after page load.
- Took fresh element snapshots, inserted text through native input and clicked a real page button. The fixture confirmed the expected value and `isTrusted` pointer event.
- Evaluated the result through the page adapter and saved/decoded the engine's PNG snapshot.
- Captured only the test browser's window; no personal browser session, credentials or accounts were used. Incidental PNG text/EXIF metadata was removed.

The development process was stopped after testing. The installed app and any existing personal browser process were not replaced, restarted or modified.

### Reproduce without scratch files

Build the debug binaries, then explicitly opt into the real-browser check:

```sh
cargo build --locked --bins
python3 scripts/smoke_macos_backend.py --run
# Optional: actual browser-window captures, never the whole desktop:
python3 scripts/smoke_macos_backend.py --run --capture
```

The tool creates a new private output directory under `target/` by default, stops its browser/server and removes its temporary HOME on exit. `--output NEW_DIRECTORY` can choose a destination but refuses an existing directory; the recorded validation path above must not be reused without choosing a new name. Default mode performs no desktop capture. Capture mode needs a macOS desktop session, Swift tools and Screen Recording permission.

The [fixture HTML](../scripts/fixtures/native_backend.html), [PID-scoped window helper](../scripts/backend_window.swift), and [offline unit tests](../scripts/test_smoke_macos_backend.py) are all tracked source. Generated helpers, logs, snapshots, tokens and profiles are not release/source assets and remain outside Git. No build or GUI starts when the Python module is imported.

### Actual running UI

New Tab, with decorative animation disabled for a stable capture:

![Native WebKit adapter build showing Felinaut's Milky Way New Tab page in an empty test profile.](screenshots/macos-native-webkit-home.png)

Local fixture after trusted native input and click:

![Felinaut displaying a loopback page that confirms native text input and pointer interaction through the page backend.](screenshots/macos-native-webkit-page.png)

These are smoke-test captures, not a complete six-theme visual baseline or a performance benchmark. No latency, battery, frame-rate, whole-process memory or cross-engine comparison result is claimed.

## What is not finished in the shared boundary

This is a first behavior-preserving seam, not the entire contract in [engine-backends.md](engine-backends.md). Generation-scoped callback invalidation, a complete cancellation/decision layer, independent profile selection, portable host services and backend-owned download adapters still need implementation and review. Existing macOS callback behavior is preserved rather than advertised as newly generation-safe. The GPUI semantic-accessibility baseline is still unresolved. Other platform agents should coordinate against this explicit scope, not infer those facilities from the trait's existence.

## Concrete WPE macOS blockers

Read-only environment checks found CMake, Ninja and pkg-config. The following modules were **not discoverable in the active pkg-config search path**:

```sh
pkg-config --modversion wpe-webkit-2.0 wpe-platform-2.0
```

A successful WPE configure/build was not attempted. Missing local packages alone do not prove a port is impossible. The independent upstream check found:

- WPE's [official FAQ](https://wpewebkit.org/about/faq.html) still documents Linux-based OS support and Android work, not a supported macOS embedder.
- Modern [WPEPlatform](https://wpewebkit.org/about/architecture.html) supplies Wayland, DRM/KMS and headless implementations, not a ready-made AppKit/Metal integration.
- At WebKit revision [`2fbc5b5f4b86d36fd212c3b5207d70e6f8a97ec7`](https://github.com/WebKit/WebKit/tree/2fbc5b5f4b86d36fd212c3b5207d70e6f8a97ec7), the [headless EGL path](https://github.com/WebKit/WebKit/blob/2fbc5b5f4b86d36fd212c3b5207d70e6f8a97ec7/Source/WebKit/WPEPlatform/wpe/headless/WPEDisplayHeadless.cpp) uses GBM or a Mesa surfaceless extension. Offscreen buffers do not establish a runnable macOS engine or Metal bridge.
- [WPE build options](https://github.com/WebKit/WebKit/blob/2fbc5b5f4b86d36fd212c3b5207d70e6f8a97ec7/Source/cmake/OptionsWPE.cmake) and the GLib launcher do not establish a maintained macOS confinement path. Separate engine processes must not be presented as equivalent to a validated sandbox.

No package was installed, no upstream outreach performed and no unsupported WPE dependency added. A maintained native Darwin build/graphics/sandbox recipe is the next prerequisite to establish; Linux results or a VM do not validate a native Mac comparison.

## When a real comparison can start

Only mark the picker ready after:

1. A pinned WPE runtime and its helpers build/run natively on the comparison Mac.
2. Its page renders inside the GPUI content area with correct resize/Retina, native input/IME, focus, buffer lifetimes and shutdown.
3. Isolation, TLS/permission behavior, clean backend-specific profiles and signing/distribution requirements are established; capability gaps are explicit.
4. Startup selection actually instantiates the requested backend, reports its real version, and fails explicitly if unavailable. No silent native fallback.
5. Equivalent local workloads can be repeated against both backends with comparable configuration and an identified security posture.

Until then, the running Felinaut cannot compare native WebKit with WPE. There is no reliable delivery date from the evidence available yet. Preserve the useful native refactor and investigate WPE feasibility separately rather than shipping a nonfunctional selector.
