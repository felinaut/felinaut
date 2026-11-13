# macOS optional engines: CEF and Gecko feasibility

**Research checked: 12 November 2026; native visual evidence updated 13 November 2026. Status: standalone native CEF/GPUI functional, static-fixture visual and scoped helper policy/syscall/exit checks pass; no browser adapter. WKWebView coexistence and production security integration remain pending.**

Scope: build-time optional engines, eventually selectable at runtime **per tab in one GPUI window**, starting with native macOS arm64. This report adds no dependencies, Cargo features, adapter, launcher, preference, or selector. Build-request gates and the existing Cocoa choices are documented separately in [engine builds](engine-builds.md); neither enables an alternate adapter. Read alongside [the backend contract](engine-backends.md), [porting plan](porting-plan.md), [GPUI controls](gpui-controls.md), and [macOS status](macos-backend-status.md).

## Decision

- **Keep system WKWebView as the working/default backend.** The existing upstream Cocoa WebKit comparison launches separate processes because both Cocoa builds define the same WebKit/Objective-C classes. It is neither CEF nor mixed-engine tabs. Do not try to load both Cocoa WebKit builds into one process.
- **CEF is a realistic Chromium/Blink/V8 embedding candidate on macOS arm64.** Official C/C++ APIs, arm64 binary distributions, sandbox initialization, AppKit child views, and maintained Rust bindings exist. They are sufficient to specify a concrete prototype, not to claim integration already works.
- **First resolve the GPUI application-class, event-pump, and shutdown boundary.** Published GPUI 0.2.2 is not an out-of-the-box CEF host. A child-view constructor alone does not solve this.
- **Gecko remains unavailable as a Felinaut backend.** No maintained macOS desktop embedding SDK/binding with CEF-like lifecycle, native-view and security contracts was established by this investigation. GeckoView is Android; desktop Firefox and Firefox automation are not an embeddable AppKit control. This is a bounded research conclusion, not proof that a custom Gecko fork is impossible.
- **Do not source-build Chromium/CEF or Firefox on this machine for this task.** The initial free-space constraint (approximately **16 GiB** at the initial lookup) rules out a full source-build attempt in this investigation. The original research used metadata/source pages only; the later standalone probe explicitly acquired a matching prebuilt CEF 156.0.26 SDK and compiled its host/helper. No full Chromium/Firefox source build or browser installation was performed.

## Native lifecycle checkpoint

The [SDK-free GPUI lifecycle probe](../tools/cef-probe/README.md) now passes on this macOS 27 arm64 host. It creates a new subclass of the already registered `GPUIApplication` before singleton acquisition, preserves GPUI's inherited state/delegate, verifies nested/exception event scope, rejects an existing singleton, and holds quit until explicit asynchronous release. AppKit's KVO subclass requires ancestry checks rather than exact class equality. No GPUI patch, object-class replacement or blanket swizzling was needed for this probe.

A later opt-in runtime checkpoint uses the published `cef`/`cef-dll-sys` **156.4.0+156.0.26** pair and matching prebuilt CEF **156.0.26+g8ea0c73+chromium-156.0.8078.88**, not the newer research-only 156.0.28 pin below. Native checks now pass real protocol conformance, framework initialization, reference-compatible pumping, native child creation, loopback HTTP/JavaScript, AppKit text/trusted pointer interaction, viewport resize and CEF shutdown before GPUI termination. The disposable fixture uses Chromium's mock-Keychain test switch; this is not production secret storage.

**The scoped helper audit now passes; WKWebView coexistence remains unverified.** Announced helper identities and exits are independently verified; selected renderer/GPU/utility read policies deny a generated canary, and both renderers' native canary opens return `EPERM`. Two final fresh runs reconciled all five launched helpers with exit evidence. The unused system-notification backend is disabled through Chromium's supported `SystemNotifications` feature gate because its native notification broker is intentionally unsandboxed. No unsandboxed-helper exception is accepted. This is not complete sandbox/broker coverage; see the [audit scope and native evidence](../tools/cef-probe/README.md#scoped-sandbox-and-helper-exit-audit). After unlocking the desktop, two complete window-only capture runs passed. The [reviewed CEF/GPUI screenshot](screenshots/macos-cef-runtime-probe.png) shows the actual document, native text and trusted-click confirmation after resize; it verifies this static fixture's presentation, not full browser behavior or accessibility. Earlier locked-session capture failures remain recorded in the probe README. The [probe README](../tools/cef-probe/README.md#real-cef-runtime-probe) records reproducible commands, safety boundaries and remaining gates. The original research sections below remain historical source evidence; the browser's CEF feature remains blocked.

## 1. Evidence boundary and exact pins

Local source baseline: `ac9f00c23590d5497a2118ba2b5aaae5c213114e`, plus the existing uncommitted working implementation. In particular, existing comparison code and updated architecture documents are uncommitted; the commit alone does not describe that implementation.

Read-only host checks actually executed:

```text
sw_vers:                     macOS 27.0, build 26A428
uname -m:                    arm64
xcodebuild -version:          Xcode 27.0, build 27A266a
xcrun --show-sdk-version:     27.0
rustc --version:              1.94.1 (e408947bf 2026-03-25)
command -v cmake:             /opt/homebrew/bin/cmake
command -v ninja:             /opt/homebrew/bin/ninja
xcrun --find clang:           XcodeDefault.xctoolchain/usr/bin/clang
initial df -h .:              approximately 19 GiB available
```

The fluctuating free-space observation does not establish enough capacity for a full engine source build. Presence of tools does not establish a successful CEF compile, deployment compatibility, signing, or a working macOS 27 sandbox. CMake/Ninja versions were not measured.

### Upstream versions used

| Item                                    | Checked evidence / pin                                                                                      | Meaning                                                                                                                                   |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Official CEF stable macOS arm64 runtime | **`156.0.28+g57389f0+chromium-156.0.8078.88`**; CEF revision **`57389f04a61ab0e9603d9f96602843d0e08bb11c`** | Stable entry in the official download index at lookup. APIs below refer to this revision. Not downloaded.                                 |
| Official CEF development head           | `e6035cfb35235e19ea5d5eae62c989b9ef0818dc`                                                                  | Queried for freshness/discovery, not selected as the runtime.                                                                             |
| Tauri-maintained `cef-rs` source        | **`3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd`**, workspace **`156.5.0+156.0.28`**                            | Concrete source candidate matching CEF 156.0.28. Includes `aarch64_apple_darwin` bindings and macOS examples. Not built.                  |
| Published `cef` crate                   | Registry reported **`156.4.0+156.0.26`**, published 8 November 2026                                         | Different from the newer source candidate. Do not claim 156.5 is published or silently substitute 156.0.28 for its expected distribution. |
| Firefox current source                  | **`mozilla-firefox/firefox` revision `5fb56bef1f2d93bf32a9452c4442a6360bc6e3be`**                           | Current Mozilla source checked for Gecko architecture/native lifecycle, not a desktop embedding release.                                  |
| GPUI                                    | **published `gpui = 0.2.2`**                                                                                | Inspected the existing local Cargo registry source, not current Zed main. Wry remains pinned to 0.57.1 in Felinaut.                       |

The old `mozilla/gecko-dev` master lookup returned a July 2025 commit; it was **not** used as evidence of current Gecko maintenance. Current documentation directs development to `mozilla-firefox/firefox`. Some guessed legacy Mozilla/CEF documentation/source URLs returned 404; conclusions below use the successfully inspected current files, not those missing pages.

The CEF [download index][cef-index] listed these exact stable arm64 archives (metadata only):

| Type            | Filename suffix after `cef_binary_156.0.28+g57389f0+chromium-156.0.8078.88` | Compressed bytes |
| --------------- | --------------------------------------------------------------------------- | ---------------: |
| Standard        | `_macosarm64.tar.bz2`                                                       |      312,278,280 |
| Minimal         | `_macosarm64_minimal.tar.bz2`                                               |      134,978,631 |
| Release symbols | `_macosarm64_release_symbols.tar.bz2`                                       |    2,124,664,205 |

Standard archive index SHA-1: `30919cc28e5cae6b82f7817e6829d4ac863454ae`; sandbox compatibility metadata: `3b66cadf26145637`. These are **reported metadata**, not locally verified hashes or a signature/trust guarantee. No extracted-size measurement was possible without acquisition. A future acquisition must authenticate provenance, verify the archive, record a strong digest, inspect architecture/deployment load commands, and budget archive + extraction + Rust/C++ outputs + app copies. Do not download symbols, tools/client archives, both architectures, or a full source tree by default.

## 2. Existing code: where engine selection would actually go

Inspected [`src/page.rs`](../src/page.rs), [`src/page/macos.rs`](../src/page/macos.rs), relevant [`src/webview.rs`](../src/webview.rs) and [`src/main.rs`](../src/main.rs), and the architecture sections of README.

- `Page = Rc<dyn PageBackend>` already erases the concrete page type. `PageBackend` exposes navigation, document state, bounds/visibility, focus, event draining, script evaluation, and explicit unsupported optional operations.
- `page::create(window, url)` and `create_hidden(window, url)` are still re-exported from the macOS adapter and always construct `NativePage`/WKWebView. `page::backend_info()` is also a single native-WebKit answer. There is **no implemented CEF/Gecko selection** in this seam.
- GPUI supplies window-relative logical bounds through a canvas. Native notifications are queued for the browser's approximately 100 ms UI drain. That drain is **not a suitable CEF message-pump scheduler**.
- The shared contract explicitly still needs generation-scoped callback invalidation, cancellation/decisions, and backend/profile selection. Existing downloads/native automation include WebKit-specific integration; a different `PageBackend` does not make them portable automatically.

### Proposed selection policy, not existing code or flags

Build-time inclusion and runtime selection are different layers:

1. A future macOS-targeted **optional CEF dependency/adapter/helper/package** must be absent from the default WKWebView build's dependency/build path. Feature-off must not resolve an SDK at build-script runtime, bundle CEF, or initialize it. Coordinate manifests, lockfile and any GPUI patch with the parent/core owner.
2. Runtime availability must distinguish **not compiled**, **missing/incompatible runtime**, **initialization failure**, and **ready**. Loading arbitrary engine frameworks from page-controlled or user-writable paths is not a supported selection mechanism.
3. A future factory must accept a stable backend ID and backend/profile context for **each page**, including hidden live-folder pages; do not hardcode selection inside every call site. New tabs inherit a default; existing tabs retain their backend. Diagnostics for a loaded tab come from `page.backend_info()`, not a global WebKit-only function.
4. CEF is initialized once per host process; each CEF tab is a `CefBrowser` with an appropriate `CefRequestContext`. This is not one `CefInitialize`/`CefShutdown` pair per tab, and not one CEF version per tab. WKWebView and CEF can conceptually coexist without the two-Cocoa-build identity conflict, but native coexistence is unproved here.
5. “Reopen using” must create a replacement page, load a validated URL anew and preserve the original until replacement creation succeeds. Confirm possible unsaved work; never replay POST automatically. Reject stale events/results from replaced generations. Downloads retain their original engine/context independently of the initiating document.
6. Persist backend/profile IDs only when implemented, with old-file defaults, unknown/unavailable-ID recovery and atomic persistence tests. Do not silently change engine, cookies, authentication, or TLS policy on failure.

The [build catalog](engine-builds.md) now accepts explicit candidate requests such as `engine-cef` and `engine-gecko` only to reject them with adapter-not-implemented errors. They do not link or enable these runtimes. No working CEF/Gecko feature invocation is advertised here.

## 3. CEF: concrete API and binding choice

### Official API versus Rust integration

CEF provides maintained C/C++ embedding interfaces and binary distributions; it is not a Wry backend switch. Its third-party language bindings are maintained separately from CEF itself. For a Rust prototype, the strongest candidate inspected is **`tauri-apps/cef-rs`**, comprising `cef` and low-level **`cef-dll-sys`**. The repository declares macOS ARM64 support, has generated Darwin arm64 bindings, a macOS helper with sandbox-first startup, application protocols, child-view support, and external-pump examples. Those are source-level suitability signals, not this application's native validation.

Do not accidentally select old **`libcef-sys`**: its registry metadata still reports CEF 117.2.6, last published October 2023, with the older repository identity. Similar crate names are not evidence of current maintenance/security coverage.

The inspected `cef-rs` source pin uses CEF's versioned API (`api_hash(sys::CEF_API_VERSION_LAST, 0)`), and its build script selects the corresponding versioned wrapper API. Keep generated bindings, wrapper, sandbox library, helpers, resources and framework consistent; never treat a same-major runtime as arbitrarily interchangeable. The exact source pin is a candidate requiring owner approval and a native build, not a dependency change made by this report.

**Acquisition warning:** `cef-dll-sys`'s build script can download and extract CEF under `OUT_DIR`; `CEF_PATH` is not a universal “offline” switch. In this pin a nonexistent/invalid configured directory can also trigger a versioned download. A future approved build must prevalidate its expected archive metadata/layout and use an actual network-denied build environment if no downloads are allowed. Cargo `--offline` alone does not stop arbitrary build-script HTTP. `dox` is a documentation-build facility, not a runnable engine configuration.

### Exact entry points for the next implementation

Names below are verified in the pinned CEF headers / Rust source. This is an API map, **not a compilable or executed Felinaut adapter**.

| Responsibility                  | Official CEF API                                                                                                               | Inspected Rust API / integration                                                                                                                       |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Main-process framework lifetime | `CefScopedLibraryLoader::LoadInMain()`; lower-level `cef_load_library` / `cef_unload_library`                                  | `cef::library_loader::LibraryLoader::new(&current_exe, false)` then `.load()`; retain until after shutdown and release of CEF objects                  |
| Helper sandbox bootstrap        | `CefScopedSandboxContext::Initialize(argc, argv)`; `cef_sandbox_initialize` / `cef_sandbox_destroy`                            | `cef::sandbox::Sandbox::new()` then `.initialize(args.as_main_args())`, **before framework load**                                                      |
| Helper framework + execution    | `LoadInHelper()` then `CefExecuteProcess(main_args, app, nullptr)`                                                             | `LibraryLoader::new(&current_exe, true)`, `.load()`, API hash negotiation, `cef::execute_process(...)`; propagate its result as the helper exit status |
| Browser process startup         | `CefInitialize(args, settings, app, nullptr)`; `CefGetExitCode()`                                                              | `cef::args::Args::new()`, `cef::Settings`, `cef::initialize(...)`, `cef::get_exit_code()`; handle failure/early exit explicitly                        |
| Pump scheduling                 | `CefBrowserProcessHandler::OnScheduleMessagePumpWork(int64_t)`; `CefDoMessageLoopWork()`                                       | handler `on_schedule_message_pump_work(&self, delay_ms: i64)`; `cef::do_message_loop_work()` on the main thread                                        |
| Per-profile context             | `CefRequestContext::CreateContext(settings, handler)`                                                                          | `cef::request_context_create_context(...) -> Option<RequestContext>`; retained per backend/profile pair                                                |
| Native child presentation       | `CefWindowInfo::SetAsChild(parent_nsview, CefRect)`                                                                            | `WindowInfo::default().set_as_child(parent, &Rect { ... })`; `parent_view` is an `NSView*`, **not `NSWindow*`**                                        |
| Async page creation             | `CefBrowserHost::CreateBrowser(window_info, client, url, settings, extra_info, context)`                                       | `cef::browser_host_create_browser(...) -> c_int`; acceptance is not completion; wait for `CefLifeSpanHandler::OnAfterCreated`                          |
| Page operations                 | `CefFrame::LoadURL`, `CefBrowser::{GoBack, GoForward, Reload, CanGoBack, CanGoForward, IsLoading}`, `CefBrowserHost::SetFocus` | Map through adapter-owned browser/frame handles; native events update queued state                                                                     |
| Native view access              | `CefBrowserHost::GetWindowHandle()`                                                                                            | `ImplBrowserHost::window_handle()`; treat returned handle as borrowed/engine-owned; retain native parents through close                                |
| Script execution                | `CefFrame::ExecuteJavaScript`                                                                                                  | `ImplFrame::execute_java_script(...)` **does not return an evaluation result**; implement bounded renderer IPC/results separately                      |
| Close/shutdown                  | `TryCloseBrowser`, `CloseBrowser(false)`, `IsReadyToBeClosed`, `CefLifeSpanHandler::{DoClose, OnBeforeClose}`, `CefShutdown`   | An asynchronous state machine; `cef::shutdown()` once, on the main thread, after all browsers finish closing                                           |

Useful Rust function signatures from `cef/src/bindings/aarch64_apple_darwin.rs`:

```rust
pub fn initialize(
    args: Option<&MainArgs>,
    settings: Option<&Settings>,
    application: Option<&mut App>,
    windows_sandbox_info: *mut u8,
) -> ::std::os::raw::c_int;

pub fn browser_host_create_browser(
    window_info: Option<&WindowInfo>,
    client: Option<&mut Client>,
    url: Option<&CefString>,
    settings: Option<&BrowserSettings>,
    extra_info: Option<&mut DictionaryValue>,
    request_context: Option<&mut RequestContext>,
) -> ::std::os::raw::c_int;

pub fn request_context_create_context(
    settings: Option<&RequestContextSettings>,
    handler: Option<&mut RequestContextHandler>,
) -> Option<RequestContext>;
```

These declarations describe upstream functions; they are not new declarations to paste into Felinaut. The fourth initialization parameter is Windows-only and null on macOS; the macOS sandbox is initialized separately in helpers.

The Rust loader/sandbox conveniences currently contain `unwrap`/assert paths. Preflight missing files, invalid architecture and layout and return actionable initialization errors; do not advertise graceful missing-runtime behavior merely because `.load()` returns a boolean. No panic may unwind across native callbacks.

## 4. AppKit + GPUI integration: the first real blocker

### Exactly one application singleton

CEF's [`cef_application_mac.h`][cef-app-mac] states that clients must subclass `NSApplication` and implement `CefAppProtocol`, which extends `CrAppControlProtocol` / `CrAppProtocol`:

```text
-isHandlingSendEvent
-setHandlingSendEvent:
-sendEvent:             scoped handling flag around superclass dispatch
-terminate:             orderly asynchronous browser shutdown, not immediate exit
```

The official sample uses `CefScopedSendingEvent` inside `sendEvent:` and explicitly checks that nobody created a plain `NSApplication` singleton first. Rust's sample implements the same protocols and restores the previous send-event flag, including nested dispatch.

**Published GPUI 0.2.2 source observation:** `src/platform/mac/platform.rs::build_classes()` registers **`GPUIApplication : NSApplication`** with a private `MAC_PLATFORM_IVAR`; `MacPlatform::run()` gets that singleton, installs **`GPUIApplicationDelegate`**, writes its private platform pointer into both objects, and calls `app.run()`. `quit()` asynchronously sends `terminate:`. The inspected application registration does not implement CEF's protocols/dispatch scope.

Consequences:

- Creating CEF's sample `SimpleApplication` first and then starting unmodified GPUI is not a safe integration: GPUI expects its own class/ivar layout and delegate. Creating it afterwards cannot replace the existing singleton.
- Do not copy the CEF sample delegate over GPUI's delegate; menus, launch callbacks, URL handoff and GPUI lifetime would be lost.
- `NSPrincipalClass` in a plist is not a complete solution to this programmatic singleton/ivar constraint. Neither is setting an Objective-C object's class after allocation or blanket method swizzling.
- **Next prerequisite:** agree a narrow GPUI macOS integration hook or reviewed patch that preserves GPUI's application state and delegate, implements CEF's protocols and scoped send-event behavior, and supports coordinated asynchronous quit. This is a separate, owner-coordinated change; no GPUI dependency upgrade/fork was made here. Stop if maintaining that boundary is unacceptable.

### One native main loop, externally scheduled CEF work

For a GPUI-owned AppKit loop, use these **candidate settings**: `multi_threaded_message_loop = 0`, `external_message_pump = 1`, `windowless_rendering_enabled = 0`, `no_sandbox = 0`. They are upstream CEF fields, not existing Felinaut configuration.

- `multi_threaded_message_loop = 1` is supported on Windows/Linux, **not macOS**. Do not move the browser UI thread to a worker to avoid AppKit integration.
- Do not call blocking `CefRunMessageLoop()` alongside GPUI's `NSApplication::run()`. Use the documented existing-loop route: `OnScheduleMessagePumpWork` schedules `CefDoMessageLoopWork` on the main thread.
- Scheduling notifications may arrive on **any thread**. Marshal a small wakeup/deadline record to the main thread; do not touch GPUI entities from the callback or run CEF work recursively inside it.
- For `delay_ms <= 0`, enqueue prompt main-thread work. For positive delay, replace/cancel the outstanding scheduled call as specified by CEF. Use a reentrancy guard, bounded/coalesced wakeups, saturating time conversion, a lifetime token, and cancellation on shutdown. Do not hold a callback mutex across a pump call that can call back into it.
- Upstream's macOS external-pump example uses `NSTimer`, `NSRunLoopCommonModes` and `NSEventTrackingRunLoopMode`; account for menu tracking, window resize and nested native dialogs. Study the pinned example, but do not copy its application-loop ownership into GPUI.
- Never pump from a GPUI render/prepaint callback. CEF notifications enqueue page events; the existing browser drain can consume those later. A fixed 100 ms polling timer is not the event-pump integration and would ignore requested deadlines. Check idle CPU and input latency rather than assuming any timer is efficient.

### Actual child-view attachment, bounds and input

1. On the AppKit main thread, obtain GPUI's live `RawWindowHandle::AppKit(handle).ns_view` using the existing raw-window-handle pattern. Create an adapter-owned container subview under that live content view. Keep raw pointers out of shared browser state.
2. Pass that **container `NSView*`** to `SetAsChild` / `WindowInfo::set_as_child`, initially with bounds `(0, 0, width, height)`. The pinned `cef_window_info_t` documents **Alloy style is used when `parent_view` is supplied**; explicitly selecting `RuntimeStyle::ALLOY` can make intent clear. This is not the old removed Alloy-runtime selection switch, and not Chrome's toolbar UI.
3. Call async `CreateBrowser`, retain client/context/parent, and publish a ready page only in `OnAfterCreated`. Report creation failure, close-during-create and parent-window loss. Do not block GPUI waiting for a callback.
4. On bounds updates, convert GPUI's window-relative top-left logical coordinates into the actual parent's AppKit coordinate system, respecting `isFlipped`/view conversion, then `setFrame:` on the container/native child. Native AppKit units are points, not blindly multiplied Retina pixels. In the pinned native implementation the supplied child rectangle is passed directly to `NSMakeRect` before `addSubview`; no automatic top-left conversion should be assumed. A zero-origin container avoids double-converting the CEF child's origin.
5. `GetWindowHandle()` provides the CEF browser view for windowed/native embedding. Use `setHidden:` for native visibility and `SetFocus` plus AppKit first-responder handoff for content/chrome focus. **`WasResized` / `WasHidden` are windowless APIs**, not the native-view resize/visibility solution; `NotifyMoveOrResizeStarted` is documented Windows/Linux-only.
6. Let the native view process real keys, marked-text composition, scrolling and pointer input. Route browser shortcuts and popup focus exactly once. Test Command-L/C/W, Tab traversal, selection/clipboard, IME candidate placement, dragging, Retina changes, occlusion, split panes and popovers above both CEF and WKWebView. Native child views still cover GPUI-painted overlays; preserve separate popup-window solutions where necessary.
7. Preserve page/native-object ownership until close completes. CEF views are not destroyed merely by dropping the last `Rc<dyn PageBackend>`; parent-window close and backend shutdown require explicit coordination.

Offscreen CEF (`CefRenderHandler::{GetViewRect, OnPaint, OnAcceleratedPaint}`) is a real alternative, not the recommended first slice. It adds buffer/IOSurface/Metal synchronization, input/IME, popups, accessibility and frame-lifetime work. `cef-rs`'s optional accelerated OSR facilities do not establish compatibility with GPUI's renderer or justify adding another GPU stack now.

### Teardown must be designed before the demo

CEF's official Cocoa sample overrides `terminate:` because the default terminates without returning for orderly Chromium cleanup. In GPUI, intercept the quit request through the reviewed lifecycle boundary, stop accepting new pages/decisions, request each CEF browser close, keep dispatch/pumping alive through unload confirmation and `OnBeforeClose`, then release browser/client/context state, shut down CEF on the main thread, and release the framework loader last. Preserve GPUI's own window-close/persistence callbacks.

Do not call `CefQuitMessageLoop` to stop a loop that was not started with `CefRunMessageLoop`; stop the host through its agreed lifecycle. No CEF API calls after `CefShutdown`. Pending scheduled pumps and renderer replies must be invalidated before state is freed. Test canceled unload, force-quit limitations, crashes, closing the last CEF tab while WKWebView remains, and quitting with both backends alive.

## 5. Sandbox, bundle, dependencies and deployment

### Current macOS sandbox recipe, not a legacy static-link recipe

Official [sandbox setup][cef-sandbox] requires **M138 and newer** macOS clients to distribute **`libcef_sandbox.dylib`**, dynamically loaded from the framework's `Libraries` directory. Helper startup must initialize sandbox context **at the beginning of `main`, before loading CEF**. The framework itself must be loaded at runtime, not linked eagerly. The scoped loader is part of that requirement, not merely an optional path convenience.

```text
main host: preflight trusted bundle -> load framework in main -> API negotiation
           -> coordinated NSApplication/GPUI startup -> CEF initialization

helper:    minimal argument setup -> load/init sandbox dylib
           -> load framework in helper -> API negotiation if using Rust binding
           -> CefExecuteProcess -> release loader -> release sandbox -> exit
```

Avoid adding GPUI, Wry, plugins, or unrelated global constructors to the minimal helper before sandbox setup. Preserve sandbox flags in every process; neither `--no-sandbox` nor `--single-process` is an acceptable success path. Separate PIDs and `no_sandbox = 0` alone do **not** prove effective confinement. Inspect actual process roles/startup and verify allowed/denied operations in the native prototype; not every CEF utility process has the same sandbox policy.

Chromium's subprocess sandbox, Apple's App Sandbox entitlement, Hardened Runtime, code signing and notarization are **different controls**. Enabling one does not establish the others. Mac App Store eligibility is not claimed.

### Bundle requirements

Use CEF's expected bundle-relative layout; preserve the selected distribution's entire required framework/resources and nested library layout, not just its main Mach-O:

```text
Felinaut.app/Contents/
  MacOS/felinaut
  Info.plist
  Frameworks/
    Chromium Embedded Framework.framework/
      Chromium Embedded Framework
      Libraries/libcef_sandbox.dylib
      ... all required libraries, resources, locales, ICU/V8 data ...
    felinaut Helper.app/Contents/MacOS/felinaut Helper
    ... role-specific helper app bundles and Info.plists ...
```

The inspected Rust bundler creates `Helper`, `Helper (GPU)`, `Helper (Renderer)`, `Helper (Plugin)` and `Helper (Alerts)` variants, copying the helper executable into their own bundles. Role-specific helper names/identifiers and launch metadata are part of Chromium integration, not extra application tabs. Audit the exact selected CEF distribution/sample and preserve required variants. The existing Felinaut packager does not become CEF-aware just because this upstream bundler exists.

The Rust loader resolves `../Frameworks` from the main executable and `../../..` from a helper executable's parent; its sandbox helper uses the corresponding relative `Libraries/libcef_sandbox.dylib`. A loose `target/debug/felinaut` is not an equivalent packaged launch. Framework/main/helper paths can be configured through CEF settings (`framework_dir_path`, `main_bundle_path`, `browser_subprocess_path`), but a custom topology still needs validation and must not defeat sandbox setup. Do not rely on `DYLD_*` overrides for a distributable app.

### Minimum source-known prerequisites versus unverified compatibility

- Native `aarch64-apple-darwin` Rust target/toolchain; Xcode's clang/clang++, Objective-C/Objective-C++, linker and macOS SDK. Existing full Xcode/Metal remains necessary for the current GPUI release path; CEF does not replace it.
- The pinned CEF CMake template requires **CMake 3.21+**. The inspected Rust sys build selects **Ninja**, builds `libcef_dll_wrapper`, and uses pre-generated platform bindings. Python 3 is needed by CEF distribution/build tooling; a full Chromium/depot_tools checkout is **not** required to consume binary distributions. Do not run bindgen regeneration or download another SDK by default.
- Pinned CEF macOS CMake flags and official `cefsimple` plist target **macOS 13.0**. The CMake SDK search includes **27.0**. These are source defaults, not tests of older macOS or of this Mac's prerelease OS/toolchain.
- The inspected Rust bundler still writes **`LSMinimumSystemVersion = 11.0`**. That is not evidence CEF 156 runs on macOS 11 and must not be copied into Felinaut as a compatibility claim. Reconcile the host, CEF Mach-O deployment commands, helper binaries, dependencies and plist to the actual highest minimum after acquisition.
- An exact oldest supported Xcode version for this complete Rust + GPUI + CEF combination was **not established**. Xcode 27 availability and an SDK-search entry do not prove it builds/links/runs cleanly. First native target is only the observed macOS 27 arm64 machine.
- Signing deployment needs an explicit identity/entitlement plan, Developer ID and notarization credentials for public distribution, and an update owner. A clean-profile desktop session is required for actual child-view/input tests. No credentials were inspected or requested here.

For Hardened Runtime, audit the selected runtime's JIT requirements (notably renderer `com.apple.security.cs.allow-jit`) and nested code signatures/Team IDs. Do not indiscriminately grant unsigned executable memory or disable library validation on every process; only use exceptions demonstrably required by the selected runtime and signing arrangement. Exact final entitlement sets and notarization are **not validated** in this report. Sign nested components in dependency order, then the outer app; verify every architecture, helper and dylib plus Gatekeeper/notarization behavior on a clean machine. An ad-hoc local signature is not notarized distribution.

### Security and privacy responsibilities added by CEF

- Felinaut would own Chromium security updates and redistribution; macOS updates do not update a bundled CEF. Track CEF stable/Chromium security releases, replace the entire matched runtime set, test rollback and avoid leaving an old pinned engine exposed indefinitely. The CEF README explicitly notes Chromium CVEs generally affect matching CEF versions and build availability can lag releases.
- Preserve current navigation scheme restrictions in `CefRequestHandler` / `CefRequestHandler::GetResourceRequestHandler` and `CefResourceRequestHandler` as applicable; handle `OnBeforeBrowse`, redirects, popups (`OnBeforePopup`) and external protocols explicitly. CEF internal `chrome:`/`about:` pages are not interchangeable with the current WebKit allowlist.
- Deny unsupported permissions, downloads and certificate exceptions rather than accepting silently. Integrate `CefPermissionHandler`, `CefDownloadHandler` and certificate handling deliberately, with deadlines/cancellation for deferrals. Preserve hidden-page download denial. Do not enable remote debugging on public interfaces or ship developer automation tokens to renderer content.
- `ExecuteJavaScript` is not WKWebView-style evaluated-result delivery. For results use a reviewed renderer handler/V8 context + `CefFrame::SendProcessMessage`/`CefClient::OnProcessMessageReceived` protocol, or a tightly configured message router. Correlate page/document/request IDs; bound data/time; cancel on context release/navigation/close; check engine-provided frame/origin attribution. Never authorize host actions based on the page's JSON claims.
- Use separate backend/profile storage. For CEF, set an application-specific absolute `Settings.root_cache_path`; `RequestContextSettings.cache_path` must be equal to or beneath that root. Keep contexts separate unless intentionally sharing the same CEF profile. CEF's root process-singleton lock matters across app instances. Do not reuse WKWebView databases or silently export cookies. Empty cache paths have documented in-memory/profile behavior but **are not a whole-application private-mode guarantee**; installation data may still persist at the root.

### Licenses and compatibility

CEF is BSD-style licensed; retain its notices and distribution credits. Chromium and bundled third-party components have additional license/notice obligations. `cef-rs` declares **Apache-2.0 OR MIT**; this does not relicense CEF/Chromium binaries. Audit the exact selected artifacts, provide notices and review codec/patent and proprietary component redistribution separately. Do not promise Chrome services, extension parity, Widevine/DRM, H.264/AAC availability or passkeys from the engine name alone. Their support depends on build configuration, licenses, entitlements and product integration.

## 6. Gecko: desktop options verified and not established

### What Mozilla actually provides

Mozilla's [GeckoView documentation][geckoview] describes a bundled web engine with a WebView-like API in the **Android** context; the source lives under `mobile/android/geckoview`, with Gradle and Java/Kotlin API surfaces. It is not an AppKit framework and does not supply a macOS `NSView` or GPUI lifecycle contract.

Current [Gecko architecture][gecko-overview] distinguishes the HTML/DOM/layout/network/JS/IPC engine, XPCOM, Fission/process isolation, principals and browser frontend/toolkit responsibilities. None is a promise that Firefox's packaged desktop binary exports a supported third-party native webview API.

Inspected current source shows:

- `widget/cocoa/nsAppShell.mm` owns Cocoa run-loop integration and implements `GeckoNSApplication`; `widget/cocoa/nsChildView.h` is engine widget machinery, not a public desktop embedding kit.
- `toolkit/xre/nsEmbedFunctions.cpp` includes **`XRE_InitChildProcess`** and process/AppShell lifecycle code. Finding “Embed” in a filename does not establish a public `XRE_InitEmbedding2` browser-control API. Legacy XULRunner/`nsIWebBrowser`/GtkMozEmbed/Camino recipes must not be advertised as current maintained macOS APIs without proving their present replacement and support.
- Rust `xpcom` implements/calls XPCOM interfaces inside Mozilla's source/build ecosystem; `toolkit/library/rust` assembles Gecko's Rust components. These are not an independently maintained Cargo SDK that creates a desktop Gecko page in a GPUI window. SpiderMonkey embedding is **JavaScript only**, not DOM/layout/network/browser embedding.
- Mozilla [remote protocols][gecko-remote] (Marionette, WebDriver BiDi, geckodriver's WebDriver route) control a separately running Firefox. They do not transfer a live Cocoa view into another process, supply a general texture/input bridge, or implement a per-tab native backend. Screenshots of Firefox are not embedding.

### Realistic alternatives, with honest boundaries

| Option                                                          | What it could accomplish                                                                       | Why it is not a selectable backend today                                                                                                                                                                         |
| --------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| External Firefox comparison                                     | Independent engine compatibility testing using loopback pages and a disposable Firefox profile | Separate application/window and automation; does not meet same-window per-tab selection                                                                                                                          |
| Custom Gecko application/fork with a new desktop embedding seam | Potentially full Gecko with retained process/security infrastructure                           | Requires a supported owner, build products, stable boundary, Cocoa/graphics/input bridge, permissions/profiles, sandbox/signing, update pipeline and native proof; substantial product work, not binding a dylib |
| Community embedder                                              | Worth evaluating if an actively maintained macOS arm64 project supplies all the above          | None meeting those gates was established here; do not assert that none can exist anywhere                                                                                                                        |
| GeckoView                                                       | A maintained Android embedding product                                                         | Wrong host platform/API; an Android runtime/VM does not validate native macOS embedding                                                                                                                          |

**Exact next Gecko prerequisite is not a Cargo flag:** obtain a maintained desktop embedding proposal with documented macOS arm64 entry points, distributable runtime, callback/thread/lifetime model, native presentation/input, and sandbox initialization. Review it with its maintainer/Mozilla before implementing against internal C++ interfaces. If pursuing a fork, isolate that feasibility project from Felinaut's default build; report unsupported until a native GPUI prototype passes the same gates as CEF. No exact public macOS Gecko `create_view` API is supplied here because one was not verified.

Mozilla's [macOS build guide][gecko-build] requires **at least 30 GB free disk**, third-party bootstrap tools, Python and Xcode command-line tools; distributable packages and Apple Silicon signing are separate steps. This exceeds the stated available budget. Artifact-mode Firefox is not a native embedding SDK and does not remove the custom-engine integration work. No `mach bootstrap`, `mach build`, artifact download or Firefox install was run.

Gecko's MPL-2.0 is file-level copyleft: distributing covered binaries/libraries requires appropriate source availability/notices, including modifications to covered files. New independent host files need not all become MPL solely by being part of a larger work. Audit bundled third-party licenses and Mozilla trademark/branding rules separately; MPL is not permission to brand a fork Firefox. A custom build must preserve Fission, content sandboxing, principal checks, NSS/TLS behavior and ongoing security updates rather than disabling them to obtain a render. Mozilla's [MPL FAQ][mpl-faq] is guidance, not a substitute for legal review.

## 7. Recommended next prototype and acceptance gates

**Next executable slice: an opt-in, disposable-profile, loopback-only macOS arm64 GPUI + WKWebView + CEF native-child-view harness, sandbox enabled. Not a settings picker and not Gecko.** Prerequisites in order:

1. **Owner agreement first:** coordinate GPUI application/lifecycle hooks and factory ownership; approve the exact Rust/runtime pair, acquisition budget/path and signing posture. Preserve default backend/data paths and keep research separate from production adapter changes.
2. **Resolve the application singleton/quit integration in a separately reviewable change.** Prove GPUI menus/URL handoff, send-event scope and host-loop shutdown without replacing the delegate or violating its private layout. If this cannot be maintained, stop before acquiring a large SDK or claiming per-tab feasibility.
3. **After explicit acquisition approval**, obtain one matched arm64 binary distribution, without symbols. Validate provenance/digests, Mach-O slices/deployment commands, required framework files and sandbox library. Preflight all paths. Keep the SDK/cache and generated bundles out of Git; measure peak disk use before a second copy/build. Do not perform a full source build.
4. Build the minimal sandbox-first helper and properly laid-out temporary app; initialize CEF with the external pump on the AppKit main thread. Fail closed on missing runtime, invalid version, missing sandbox dylib or initialization failure. Test no-CEF launch separately.
5. Create one CEF child view and one existing WKWebView in the **actual GPUI window**, using only a self-contained loopback fixture and distinct temporary engine data. Exercise text input, JS/DOM, independent storage, resize/hide/show/focus and simultaneous lifetime. A separate `cefsimple` window alone is only a prerequisite smoke test.
6. Prove canceled close/unload, async create/close races, renderer crash/recovery, stale message suppression, live WKWebView after CEF-tab close, and full orderly application shutdown. Record native process roles, sandbox evidence and security limitations. Preserve parent objects until callbacks terminate.
7. Only then implement the `PageBackend` adapter and per-tab/default selection model. Required `evaluate` results need an actual bounded IPC implementation; optional snapshots/audio/trusted input/certificate exceptions start unsupported unless implemented and tested. Downloads/permissions/hidden-live-page behavior are explicit follow-up integration gates, not free parity.
8. Add **behavioral unit tests** for availability/default selection, invalid/unknown IDs, persistence round trips/older files, missing runtime, creation failure, generation replacement, canceled/late script results, profile separation, decisions, downloads outliving navigation and teardown. Native tests must actually opt in, be loopback-only and independent of personal logins.
9. Validate real AppKit input/IME/accessibility, popup layering, multiple scales and idle/active CPU/memory across the process tree. Capture real clean-profile screenshots/video for the future UI PR using the browser-control skill; a mockup or this research document is not UI evidence. Re-run relevant project formatter, Clippy, unit/packaging tests and opt-in native regressions after code changes. Native macOS 27 results alone do not validate macOS 13 or Intel/universal deployment.

### What can and cannot be enabled now

| Capability                                                                  | Today                                                                                              |
| --------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Normal system WKWebView browsing                                            | Implemented existing backend; not retested in this research                                        |
| Trusted upstream Cocoa WebKit comparison                                    | Existing separate-process developer tool; documented smoke evidence, not rerun by this research    |
| Compile-time optional CEF backend in Felinaut                               | **Not implemented**; concrete upstream APIs and matching arm64 source/runtime candidate identified |
| Runtime/per-tab WKWebView + CEF selection                                   | **Not implemented**; application-class/event-loop/close/profiles/factory work required             |
| Multiple CEF versions or system + upstream Cocoa per tab in one host        | **Not provided** by the proposed in-process topology                                               |
| Native macOS Gecko tab                                                      | **Not available**; maintained desktop embedding path not established                               |
| Current sandbox/signing/security-update validation of CEF/Gecko in Felinaut | **Not executed**                                                                                   |

## 8. Validation of this report

**Checked:** local source/contracts and published GPUI 0.2.2 registry source; read-only Git status/HEAD; native OS/architecture/Xcode/SDK/Rust/tool availability/free space; primary-source CEF headers/examples/sandbox/build defaults; official arm64 archive metadata; maintained Rust repository/pin/generated APIs and registry metadata; current Mozilla source, GeckoView/remote protocols/build docs and licensing guidance.

**Not executed:** SDK/runtime archive downloads, clones, installers, dependency additions/resolution, engine builds, `cef-rs` examples, native CEF/Gecko app launch, sandbox denial tests, signing/notarization, mixed-engine pages, screenshots, benchmarks, compatibility tests or per-tab selection. Existing documents' native-WebKit passes are historical evidence belonging to those runs, not tests executed for this report.

Only this Markdown file is authored by the research task. No Rust/Python/package behavior changed, so Cargo/Clippy/unit/native suites were deliberately not run; they would neither verify researched upstream APIs nor prove a nonexistent backend. Documentation whitespace/local-link checks are the relevant local validation. Upstream APIs were source-inspected, not compiler-checked here.

## Primary-source references

All revision links below are immutable source pins. Registry/download index and rendered Mozilla documentation are dated lookup evidence and may change; recheck stable security versions before acquisition.

- [Official CEF download index][cef-index] — arm64 versions, channels, archive sizes/hashes and sandbox compatibility metadata.
- [CEF README/security-update responsibilities][cef-readme] and [general usage / macOS layout][cef-usage].
- [CEF macOS application protocols][cef-app-mac], [scoped library loader][cef-loader], [sandbox setup][cef-sandbox], [sandbox API][cef-sandbox-api], [minimal helper startup][cef-helper], [Cocoa main application example][cef-main].
- [CEF initialization/message-loop APIs][cef-app], [pump callback][cef-pump], [macOS external pump implementation][cef-pump-mac], [settings/profile paths][cef-types].
- [Native window-info C++ helper][cef-window], [macOS window-info types/runtime style][cef-window-types], [native macOS view implementation][cef-native], [browser lifetime/creation/focus APIs][cef-browser].
- [CEF CMake minimum][cef-cmake], [macOS compiler/SDK/deployment defaults][cef-cmake-vars], [official sample plist][cef-plist].
- [Maintained Rust binding README][rs-readme], [workspace version/license][rs-manifest], [sys build/download behavior][rs-build], [arm64 API bindings][rs-bindings], [native child-view helper][rs-window], [Rust loader][rs-loader], [Rust sandbox wrapper][rs-sandbox], [helper example][rs-helper], [application-class example][rs-app], [bundle construction/deployment metadata][rs-bundle].
- [`cef` registry metadata][rs-registry] and [older `libcef-sys` registry metadata][old-sys].
- [Published GPUI 0.2.2 macOS platform source][gpui-platform] — also inspected locally from the existing registry cache.
- [GeckoView Android embedding][geckoview], [current Gecko overview][gecko-overview], [Gecko Cocoa lifecycle][gecko-app], [current XRE process lifecycle][gecko-xre], [Rust XPCOM][gecko-rust], [remote protocols][gecko-remote], [macOS build prerequisites][gecko-build], [MPL FAQ][mpl-faq].

[cef-index]: https://cef-builds.spotifycdn.com/index.json
[cef-readme]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/README.md
[cef-usage]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/docs/general_usage.md
[cef-app-mac]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/cef_application_mac.h
[cef-loader]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/wrapper/cef_library_loader.h
[cef-sandbox]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/docs/sandbox_setup.md
[cef-sandbox-api]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/cef_sandbox_mac.h
[cef-helper]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/tests/cefsimple/process_helper_mac.cc
[cef-main]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/tests/cefsimple/cefsimple_mac.mm
[cef-app]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/cef_app.h
[cef-pump]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/cef_browser_process_handler.h
[cef-pump-mac]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/tests/shared/browser/main_message_loop_external_pump_mac.mm
[cef-types]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/internal/cef_types.h
[cef-window]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/internal/cef_mac.h
[cef-window-types]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/internal/cef_types_mac.h
[cef-native]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/libcef/browser/native/browser_platform_delegate_native_mac.mm
[cef-browser]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/include/cef_browser.h
[cef-cmake]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/CMakeLists.txt.in
[cef-cmake-vars]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/cmake/cef_variables.cmake.in
[cef-plist]: https://github.com/chromiumembedded/cef/blob/57389f04a61ab0e9603d9f96602843d0e08bb11c/tests/cefsimple/mac/Info.plist.in
[rs-readme]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/README.md
[rs-manifest]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/Cargo.toml
[rs-build]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/sys/build.rs
[rs-bindings]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/cef/src/bindings/aarch64_apple_darwin.rs
[rs-window]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/cef/src/window_info.rs
[rs-loader]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/cef/src/library_loader.rs
[rs-sandbox]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/cef/src/sandbox.rs
[rs-helper]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/examples/cefsimple/src/bin/cefsimple_helper.rs
[rs-app]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/examples/cefsimple/src/mac/mod.rs
[rs-bundle]: https://github.com/tauri-apps/cef-rs/blob/3ec2d7cb59b577adb1f0205f92d91e1b1925c8fd/cef/src/build_util/mac.rs
[rs-registry]: https://crates.io/api/v1/crates/cef
[old-sys]: https://crates.io/api/v1/crates/libcef-sys
[gpui-platform]: https://docs.rs/crate/gpui/0.2.2/source/src/platform/mac/platform.rs
[geckoview]: https://firefox-source-docs.mozilla.org/mobile/android/geckoview/index.html
[gecko-overview]: https://github.com/mozilla-firefox/firefox/blob/5fb56bef1f2d93bf32a9452c4442a6360bc6e3be/docs/overview/gecko.md
[gecko-app]: https://github.com/mozilla-firefox/firefox/blob/5fb56bef1f2d93bf32a9452c4442a6360bc6e3be/widget/cocoa/nsAppShell.mm
[gecko-xre]: https://github.com/mozilla-firefox/firefox/blob/5fb56bef1f2d93bf32a9452c4442a6360bc6e3be/toolkit/xre/nsEmbedFunctions.cpp
[gecko-rust]: https://github.com/mozilla-firefox/firefox/blob/5fb56bef1f2d93bf32a9452c4442a6360bc6e3be/xpcom/rust/xpcom/src/lib.rs
[gecko-remote]: https://firefox-source-docs.mozilla.org/remote/
[gecko-build]: https://firefox-source-docs.mozilla.org/setup/macos_build.html
[mpl-faq]: https://www.mozilla.org/en-US/MPL/2.0/FAQ/
