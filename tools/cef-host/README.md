# Felinaut CEF native host

Reusable macOS AppKit/GPUI 0.2.2 bootstrap, external CEF message pump, and **per-page** native container ownership. The default build is SDK-free; enabling `runtime` verifies the prepared SDK header and registers its actual Objective-C application protocols. No acquisition, downloading, CEF initialization, page policy, or GPUI browser state is hidden in this crate.

The browser-facing integration is `src/page/cef/runtime.rs`, with native callbacks and pure URL/error policy alongside it. The existing probe is not modified.

## Parent integration contract

Use an optional macOS dependency named `cef-host`:

```toml
cef-host = { package = "felinaut-cef-host", path = "tools/cef-host", optional = true, features = ["runtime"] }
```

The root runtime also requires `cef` and `cef-dll-sys` exactly `156.4.0` with sandbox/resources as in the probe, plus `raw-window-handle` 0.6.2. Re-export `src/page/cef/protocol.rs` as `crate::evaluation_protocol`; enable `crate::page::cef` under `engine-cef`. The agreed `PageBackend::creation_ready(&self) -> Result<bool, String>` default is `Ok(true)`; the runtime proxy overrides it.

- `runtime::initialize() -> Result<(), String>`: main thread, **before** `Application::new()`. Requires a prepared bundle, exact runtime/API, SDK protocols and sandboxed helper. Treat failure as a startup failure, not permission to continue with a partially bootstrapped application or silently select another backend.
- `runtime::create(&gpui::Window, url, hidden) -> Result<Page, String>`: independent container and pending page proxy, including pending layout/visibility. Only `creation_ready() == Ok(true)` establishes successful replacement creation. Other required operations fail explicitly until ready. Accepted creation is retained even if the proxy is dropped before its native callback.
- `runtime::poll()`: main/UI thread, e.g. the existing 100 ms drain; **not render or native callbacks**. Call regardless of which backend is visible. This dispatches creation/close and drains fixed-size lifecycle mailboxes. It does not pump CEF inline.
- `runtime::available() -> bool`: successful initialized runtime accepting new pages, including pending context initialization; false during shutdown or pump failure.
- `runtime::begin_shutdown()`: idempotently rejects new pages, invalidates script delivery, and requests every pending/live page close. Continue polling.
- `runtime::shutdown_completed() -> bool`: true only after all accepted creations received `OnBeforeClose`, browser/context references were released, the external pump stopped, deferred native `cef::shutdown()` returned and the loader was released.

The application subclass intercepts normal `NSApplication::terminate:`. `cef_host::close_requested()` is polled automatically by `runtime::poll()`. Successful shutdown then calls `cef_host::finish_termination()` outside GPUI borrows. **The parent must also defer actual GPUI window removal:** route window-close to the application quit request and return false from `on_window_should_close` until the shutdown barrier completes. Retaining an NSWindow is not authorization for GPUI to destroy its Rust window state while CEF is using the content view.

The loader stays alive through shutdown. Unexpected destruction of the runtime without the barrier deliberately retains its native owners until process exit rather than racing an active CEF callback with library unloading.

## Bundle and profile requirements

For a main executable `Contents/MacOS/felinaut`, use:

- `Contents/Frameworks/Chromium Embedded Framework.framework` from prepared CEF `156.0.26+g8ea0c73+chromium-156.0.8078.88`;
- `Contents/Frameworks/felinaut Helper.app/Contents/MacOS/felinaut Helper`, plus the corresponding CEF GPU/Renderer/Plugin/Alerts helper bundles;
- the sandbox library, resources, notices and maintained renderer evaluation transport, as in the validated probe helper, but **without** its probe-canary startup requirement.

Helper naming derives from the main executable filename. The loaded framework's own version symbols (including CEF commit number 3671) and API negotiation are checked, not just its filename. Sandboxing is requested, command-line argument processing is disabled, and browser logging is disabled to avoid collecting browsing contents. Packaging/signing and production helper integration remain the parent's responsibility.

The runtime resolves the startup profile through the existing `session::default_path()` and uses `engines/cef/runtime-v156` below it for CEF's separate root cache/instance lock. A **single explicit, in-memory `RequestContext`** is shared among CEF tabs. Its empty `cache_path` does not open the default persistent browser context. Engine subdirectories reject symlinks/files before writing beneath them. WebKit data paths are untouched; no cookie or account migration occurs.

Production uses the real macOS keychain. The public mock key is only selected when `FELINAUT_NATIVE_TEST=1`, HOME and CFFIXED_USER_HOME canonically match a private mode-0700 `/private/tmp/felinaut-engines-test-*` directory, and its regular `.felinaut-engine-test` marker contains exactly `felinaut-engine-test-v1\n`. Existing keychains or a prior `.felinaut-cef-test-claimed` file are rejected; startup exclusively creates that claim before loading CEF. Do not reuse a test HOME. Invalid opt-in fails closed instead of accessing the user's real keychain.

## Security and current limitations

Top-level/frame navigation permits only credential-free HTTP(S) and exact `about:blank`. Web resources additionally permit bounded data URLs, web-origin blob URLs and canonical opaque-origin blob UUIDs; file, browser UI, extension, JavaScript, and external protocols are denied. Native popups are suppressed and allowed target URLs become bounded `Open` events. Media capture, all permission prompts (including geolocation/notifications), downloads, external protocol execution, certificate exceptions and client certificates are denied. Native JavaScript dialogs are suppressed; before-unload cannot indefinitely block forced shutdown. Unsupported page capabilities remain explicit errors.

Callbacks never call GPUI. Request/resource handlers, including context-level service-worker policy, retain no UI-only Rc, Evaluation, Browser or AppKit state across IO-thread callbacks/destruction. Page events use the existing bounded notification queue; lifecycle notifications use one bounded mailbox per page, with a 128-page bound including pending closes. Main-frame errors are normalized without renderer-supplied diagnostic text; renderer termination is an explicit recoverable load failure. A 30-second creation timeout reports an error but **does not free accepted pending native creation**: it closes the eventual browser safely. There is no unsafe forced shutdown if CEF fails to deliver its required native close callback.

Input/IME/accessibility completeness, full-root mixed-WK/CEF behavior, production real-keychain startup and distributable signing are not established by the standalone native smoke below.

## Validation

```sh
cargo fmt --manifest-path tools/cef-host/Cargo.toml --all --check
cargo clippy --locked --manifest-path tools/cef-host/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path tools/cef-host/Cargo.toml
# Also compile the actual SDK protocol bridge (offline, no downloads):
CEF_PATH=/Users/marco/felifox/target/cef-runtime/156.0.26/macosarm64 cargo test --locked --manifest-path tools/cef-host/Cargo.toml --features runtime
```

Implementation checkpoint on source `0e321cea9e5f720c7c412e841a1dcf857b6cd0ae` plus these uncommitted changes, macOS 27.0 (26A428), arm64, Xcode 27.0 (27A266a): default host tests passed (6); native bridge clippy passed with warnings denied. A temporary ignored integration harness compiled the real CEF adapter/runtime with the agreed readiness extension and passed clippy plus 80 behavioral tests. This is not a root application build; root manifests/lifecycle remain a separate integration task.

The bounded loopback native smoke used those actual runtime sources inside a GPUI window and the unchanged probe helper with its required canary: two pages loaded; one independently closed while the other still evaluated `2 + 3` to `5`; a pending creation was dropped immediately after a creation drain, before the native loop resumed; and application quit with an externally retained page proxy reached `shutdown_completed()` **before** GPUI's quit callback. A second native smoke produced a normalized main-frame `ERR_EMPTY_RESPONSE` (-324) with no subsequent false `Finished` event, then shut down in the same order. Renderer canary checks returned EPERM. Smoke log and temporary harness are under the ignored `tools/cef-host/target/`; no user's browsing profile or logged-in service was used. Actual root-browser screenshot/video and root/WK coexistence verification are pending the parent integration.
