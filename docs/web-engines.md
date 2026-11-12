# Web engines: native WebKit and WPE WebKit

**Status:** feasibility notes, not an implemented feature or platform-support promise. Reviewed against the sources below on 12 November 2026. Felinaut currently ships **only native macOS WebKit**, embedded with Wry 0.57.1.

## Short answer

A browser can support multiple rendering backends, but **WPE WebKit is not a drop-in second backend for this macOS app**. WPE's official FAQ currently lists Linux-based operating systems, with Android support in progress; it does not document a supported macOS embedder. Wry 0.57.1 provides macOS WebKit and Linux WebKitGTK integrations, not a WPE engine-selection option.

Keep native WebKit as Felinaut's supported backend. Treat WPE on macOS as a separate research/porting project, with a working prototype and security review required before adding a user-facing selector. A Linux experiment could establish WPE embedding experience, but would not prove macOS support or make Felinaut a supported Linux browser.

For a staged Windows/Linux investigation that keeps GPUI, explains Zed's native input paths, and evaluates GTK-free Linux embedding, see the [porting plan](porting-plan.md). It does not propose WPE as a supported Windows or macOS backend.

## Two ports of the same engine family

- **Native macOS WebKit:** Apple's WebKit framework, exposed through `WKWebView`. Wry embeds that native view inside Felinaut's GPUI window. macOS supplies the engine rather than Felinaut shipping its own copy.
- **WPE WebKit:** the upstream WebKit port intended for embedded systems and custom presentation environments. It provides WebKit without a Cocoa/GTK UI widget; the integrator connects rendering, input, and browser UI to the host platform.

Both use WebKit and JavaScriptCore. This is **not WebKit versus Chromium/Blink**, and WPE is not a way to get Chrome's engine. Different WebKit releases, build options, networking/media implementations, and platform integrations can produce different behavior, but switching ports does not guarantee that a broken website will work.

This comparison does not limit Felinaut's architecture to two backends. The [portable engine-backend contract](engine-backends.md) separates platform services, embedding APIs, and engine families so we can evaluate Chromium, Gecko (Firefox's engine), or other candidates independently. Mozilla's GeckoView targets Android; a desktop Gecko adapter remains an unproven feasibility project, not a dependency or supported option.

## Differences and advantages

| Area                      | Native macOS WebKit: what we have                                                                                                                                                           | WPE WebKit: what an integration would involve                                                                                                                                                                        |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Platform fit              | A native AppKit view on our current supported OS.                                                                                                                                           | Officially documented Linux support; macOS embedding would need a port/integration investigation.                                                                                                                    |
| Engine updates            | Apple distributes WebKit through system software updates; availability depends on the installed macOS and Apple's support policy. Felinaut cannot independently replace that system engine. | We can choose and ship an engine version independently of Apple's releases, but must rebuild and distribute security updates ourselves. A fixed version must not become a permanently frozen version.                |
| Installation              | No bundled web engine or its runtime dependency tree.                                                                                                                                       | A self-contained app would need to ship the engine, required libraries, resources, and subprocesses; local developer dependencies alone are not a distributable product.                                             |
| Rendering and input       | Existing native child-view presentation, focus, text input, and platform integration.                                                                                                       | More control over the presentation pipeline, but a platform integration must supply buffers, input, and application-level UI. No ready-made macOS view is documented.                                                |
| Browser features          | Felinaut already implements navigation, zoom, scripts, audio controls, snapshots, and downloads against Wry and native WebKit APIs.                                                         | Equivalent engine APIs and host behavior must be connected and tested; existing Objective-C integrations cannot simply be reused.                                                                                    |
| Media and system services | Uses the Apple WebKit platform stack, subject to the APIs and entitlements available to embedded apps. It is not the full Safari application.                                               | WPE's multimedia stack uses GStreamer when enabled. Codecs, devices, hardware acceleration, WebRTC, DRM, and system integration depend on the exact build and deployment.                                            |
| Security isolation        | Uses Apple's WebKit process architecture. Felinaut is still responsible for safe navigation policies and native message interfaces.                                                         | WebKit has a multiprocess architecture here too, but the deployed sandbox, IPC, permissions, dependency updates, and macOS isolation would need explicit validation. Linux sandbox machinery is not a macOS sandbox. |
| Performance and battery   | A sensible macOS baseline with existing platform integration. We have not benchmarked it against WPE.                                                                                       | WPE targets efficient embedded deployments, but that does not prove lower memory use, faster pages, or better battery life on a Mac. Buffer copies, compositing, and media integration matter.                       |
| Best reason to choose it  | A small macOS browser with the least additional engine-integration and distribution burden.                                                                                                 | Control of the WebKit version/build and a custom rendering environment, especially on supported embedded/Linux targets.                                                                                              |

Neither option automatically adds Safari extensions, Chrome extensions, password-manager integration, working passkeys, or streaming-service DRM. Our current passkey limitation is documented in the [README](../README.md#passkeys); WPE must not be advertised as a workaround without an independently implemented and tested authenticator integration.

## A separately built native WebKit on macOS

The supported upstream starting point for a macOS WebKit build is the **Cocoa/macOS port**, exposed through `WebKit.framework` and `WKWebView`, not WPE. Apple's system framework is what Felinaut currently uses. WebKit developers can also build that native port with Xcode and exercise it using the upstream MiniBrowser reference app.

From a suitable WebKit source checkout, the documented development commands include:

```sh
Tools/Scripts/build-webkit --release
Tools/Scripts/run-minibrowser --release
```

These commands have **not** been run for this investigation. No WebKit checkout or alternate engine runtime has been added to Felinaut. Upstream also documents `run-webkit-app` for launching an app against development framework builds; that is a development mechanism, not a validated production bundling solution.

A useful separate experiment would first prove a pinned Cocoa WebKit build in MiniBrowser, then investigate an isolated Felinaut launch against it, verifying the framework actually loaded through runtime diagnostics. Prefer separate launches/profiles for different framework builds, not loading two versions into one running process. Do not replace system frameworks or disable system protections. Helper processes, framework loading, sandboxing, signing, licenses and security updates still need review before redistribution.

**System WebKit versus a separately built Cocoa WebKit tests revisions/configurations of the native port. It is not native WebKit versus WPE**, and should not be labeled as such in a picker or benchmark. Preserve WPE-on-macOS as its own unresolved feasibility investigation.

Sources: [Apple's WKWebView API](https://developer.apple.com/documentation/webkit/wkwebview), [building WebKit](https://webkit.org/building-webkit/), and [running WebKit development builds](https://webkit.org/running-webkit/).

## What WPE would require on macOS

The current WPE architecture documentation recommends **WPEPlatform starting with 2.54**. It exchanges rendered buffers through `WPEDisplay`, `WPEView`, and related types, with built-in Wayland, DRM/KMS, and headless implementations. Older `libwpe` and external-backend instructions describe the legacy integration and should not be assumed to be the starting point for a new project.

A headless backend is useful for offscreen work, but **headless does not mean macOS support, an AppKit view, or a Metal texture bridge**. Custom presentation is an integration task, not a free consequence of receiving pixels.

Before promising a macOS WPE option, prove:

1. **A reproducible Darwin build:** select a supported upstream release and identify its actual dependencies. Current upstream build configuration includes GLib/GIO, libsoup, libepoxy, xkbcommon, and optional platform/media stacks. Validate which parts work on macOS instead of assuming Linux build instructions apply. Darwin conditionals in upstream source are not proof of a supported Mac runtime.
2. **Usable presentation:** render a real page inside a GPUI/AppKit window; establish buffer ownership, synchronization, resize/HiDPI behavior, and main-thread/event-loop coordination. Measure copying costs rather than claiming zero-copy rendering.
3. **Complete input and host UI:** keyboard navigation, IME/composition, selection, clipboard, pointer events, scrolling, focus, accessibility, file choosers, dialogs, and popup menus. WPE's embedder owns dialogs and popup rendering.
4. **Browser and media behavior:** navigation history, load errors, TLS handling, downloads, permissions, audio/video, and supported media devices. No silent weakening of scheme restrictions, certificate validation, or the native message boundary.
5. **Safe distribution and maintenance:** subprocess lifecycle and sandboxing, dependency/license obligations, vulnerability updates, and correctly signed arm64/x86_64/universal bundles. WPE and its dependencies have their own licenses; Felinaut's MIT license does not replace those obligations.

This document records an assessment, not a successful WPE macOS build, benchmark, or security audit. No such prototype has been built or tested here.

## Where Felinaut would need to change

These are concrete integration points, not a proposal to refactor them preemptively:

- [`src/webview.rs`](../src/webview.rs): `NativePage` owns a Wry `WebView`. It creates native child views and directly uses `WKWebView` for document/loading state, zoom, trusted input, snapshots, and native failure handling. Its event channel is a useful boundary to preserve.
- [`src/downloads/native.rs`](../src/downloads/native.rs): downloads depend on `WKDownloadDelegate`, native progress, and Wry's navigation delegate. WPE would need a separate download adapter with equivalent filename, cancellation, hidden-page, and filesystem safety behavior.
- [`src/main.rs`](../src/main.rs): tab lifetime, layout, focus, page events, and hidden live-folder pages currently assume the existing backend. Background integration pages must use the right engine/profile, not accidentally borrow another engine's logins.
- [`Cargo.toml`](../Cargo.toml): the locked dependencies expose Wry's `os-webview` integration, not a bundled WPE stack. Wry's Linux WebKitGTK backend is a different port again, not WPE.
- [`scripts/package_macos.py`](../scripts/package_macos.py) and [`scripts/assemble_universal_macos.py`](../scripts/assemble_universal_macos.py): today's flat app-bundle validation deliberately rejects unexpected helpers/frameworks and extra Mach-O files. Shipping WPE would require deliberate packaging changes, architecture checks for every bundled executable/library, and correct inside-out signing—not bypassing the existing checks.

If a prototype succeeds, introduce a narrow backend adapter for the operations we actually use. Report backend capabilities explicitly rather than pretending every feature works everywhere. Preserve queued native callbacks, bounded untrusted messages, and the existing navigation restrictions.

## What engine selection should mean for a user

A safe initial design would be **one engine per browser profile, selected at startup**, with native WebKit as the default. This is a proposed design, not an existing setting.

- **Separate website data:** cookies, local storage, IndexedDB, caches, service workers, and permissions belong to their engine/profile. The same account may need a fresh sign-in. Do not share database directories or silently copy session cookies. Our current native WebKit storage is app-managed; using Apple's engine does not mean sharing Safari's logged-in sessions.
- **No invisible engine fallback:** a switch should be explicit. Do not replay form submissions or quietly retry a failed TLS/authentication request in another engine.
- **No live-document transfer:** changing engines cannot preserve a running page's DOM, JavaScript heap, native history, or in-flight operations. Require a restart initially. A later per-tab “reopen with” feature would reload the page and need unsaved-work/download handling.
- **Backward-compatible persistence:** keep existing native profiles intact. Test old/malformed settings and session files, unavailable backends, rollback, and cross-engine isolation before saving an engine preference.
- **Honest feature labels:** expose the backend and engine version, document unsupported media/system features, and fail clearly if the requested backend cannot start.

Supporting multiple backends simultaneously would multiply lifecycle, storage, failure, and test cases. It should solve a demonstrated need, not be added only because a dropdown is easy to draw.

## Proposed experimental backend picker

A small picker is useful for controlled comparisons **after two backends actually work on the same OS**. It does not itself make WPE available on macOS. Keep native WKWebView as the macOS default and treat WPE-on-macOS as a separately scoped feasibility prototype, not a prerequisite for the Linux/Windows ports.

For the first implementation, use a simple experimental/developer setting with one backend per profile at startup:

- Offer **Native WebKit** and, only when a validated runtime is available on that machine, **WPE WebKit (Experimental)**. An unavailable candidate can be shown disabled with a clear reason, never as a working choice. Do not infer availability from a platform label or library filename alone.
- Require a restart or launch an isolated comparison profile. Do not hot-swap live pages, silently replay form submissions, transfer logins or share engine website-data directories.
- Show the active backend, actual engine/runtime version and relevant build configuration in diagnostics so screenshots and measurements identify what ran. A failed requested backend must produce an explicit error, not silently fall back and invalidate the comparison.
- Compare the same local pages on the same Mac, viewport/scale, browser build mode and artwork settings. Use separate equivalent clean profiles, distinguish cold/warm caches, repeat trials, and measure the whole process tree. Different engine versions/configurations remain a confounder to disclose.
- Test availability detection, unavailable/missing runtime, saved selection/older files, initialization failure, restart behavior and profile separation. Native integration tests must prove that the chosen adapter really rendered and received input. Any actual picker UI needs the usual screenshots/video and keyboard/accessibility validation.

This is planned comparison tooling, not an implemented preference or a promise that WPE can currently run inside Felinaut on macOS. It should follow a working, safely embedded WPE macOS prototype; initially a startup override can exercise the contract without designing a large settings UI.

## Recommended next step

**Continue shipping native WebKit.** If the goal is a pinned WebKit version or custom texture-based composition, first scope an isolated WPE feasibility prototype. Gate any product work on rendering, input/IME, media, sandboxing, and packaging working on both Mac architectures.

If the goal is instead “a different engine when WebKit cannot run a website,” evaluate a genuinely different engine family, such as Chromium through CEF, separately. That would also be a substantial integration and security-maintenance commitment, not a recommendation to add a dependency now.

Any future backend feature needs unit tests for selection, capabilities, persistence, failure handling, and profile isolation; offline extractor fixtures for each supported backend; and opt-in native tests using loopback pages and clean profiles. Compare identical builds/pages for memory, startup, rendering latency, media, and power before claiming a performance advantage.

## Primary sources

These describe upstream capabilities, not functionality shipped by Felinaut. Upstream `main` may differ from a released WPE tarball; pin and recheck a concrete release before implementation.

- [Wry 0.57.1: platform considerations and feature flags](https://docs.rs/wry/0.57.1/wry/).
- [Apple: WKWebView](https://developer.apple.com/documentation/webkit/wkwebview).
- [WPE FAQ: operating systems, media, dialogs, licensing, and platform integration](https://wpewebkit.org/about/faq.html).
- [WPE architecture: WPEPlatform and legacy backends](https://wpewebkit.org/about/architecture.html).
- [Upstream WPE build options and dependencies](https://github.com/WebKit/WebKit/blob/main/Source/cmake/OptionsWPE.cmake).
- [Upstream WPE platform build configuration, including Darwin conditionals](https://github.com/WebKit/WebKit/blob/main/Source/WebKit/PlatformWPE.cmake).
- [Upstream headless display implementation](https://github.com/WebKit/WebKit/blob/main/Source/WebKit/WPEPlatform/wpe/headless/WPEDisplayHeadless.cpp).
- [Upstream GStreamer dependency checks](https://github.com/WebKit/WebKit/blob/main/Source/cmake/GStreamerChecks.cmake).
