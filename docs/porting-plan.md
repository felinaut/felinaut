# Multiplatform Felinaut: beautiful, fast, and portable

**Status:** proposed investigation and implementation plan, not shipped Windows/Linux support. Felinaut remains macOS-only. No cross-platform build, input test, or WPE performance result is claimed here. Research checked on 12 November 2026; see also [Web engine options](web-engines.md). The first native adapter implementation and its actual validation are tracked in the [macOS backend status](macos-backend-status.md); WPE comparison is not ready.

## Execution roadmap

**Product goal:** one recognizable Felinaut experience on macOS, Windows, and Linux, with GPUI chrome, appropriate native integration, and replaceable web-engine adapters. Preserve the current macOS beta while proving each new platform. Beauty, accessibility, performance, and security are acceptance criteria throughout—not a final polish phase.

### Architecture we are building toward

```text
Shared Felinaut experience
  brand, theme tokens, controls, tabs, history, live-folder/download models
                         |
       engine-neutral page/event/capability contract
                         |
     macOS adapter / Windows adapter / Linux adapter
       WKWebView       WebView2       WPE investigation

Host services are separate: paths, dialogs, menus, clipboard, OS preferences.
Future Gecko/CEF/other adapters must satisfy the same reviewed contract.
```

This is the target separation, not a description of completed refactoring. Do not force all engines into native-child-view assumptions or reduce their features to a silent lowest common denominator.

### Milestones, owners, and completion criteria

| Milestone                                | Owner                                                                | Deliverable and completion gate                                                                                                                                                                                                                                                                        |
| ---------------------------------------- | -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **M0 — Capture the baseline**            | macOS/core, with platform agents recording their environments        | Clean-profile captures of all six themes and representative controls; repeatable macOS performance measurements; agreed control/capability matrix. Record hardware, OS, display scale/refresh, engine/build version, workloads and commands. No performance advantage is assumed.                      |
| **M1 — Prove the control foundation**    | Core/control work coordinated across platforms                       | Reviewed GPUI/accessibility baseline and a gallery of actual buttons, address input, choices, slider, lists and popovers. Keyboard, IME, focus and semantic accessibility gaps are known and have an implementation path. A dependency update is its own change, not mixed into an engine port.        |
| **M2 — Establish the portable seam**     | macOS/core                                                           | A small shared page/event/capability contract with behavioral tests; current WKWebView implementation isolated behind it without changing macOS behavior/data. Agree the contract revision before production platform adapters depend on it.                                                           |
| **M3 — Prove each native backend**       | Windows and Linux agents on their own machines; core maintains macOS | Windows: GPUI + WebView2 inside the actual browser window. Linux: GPUI + integrated WPE presentation/input on Wayland first, X11 separately verified. Prototypes demonstrate focus, composition, resize, media/runtime prerequisites, lifecycle and safe failure—not just a separate engine window.    |
| **M4 — Reach a coherent browser**        | Each platform owner with shared-model review                         | Navigation/errors, tabs/pins/splits, history, live folders, audio, downloads, persistence and available native services work against the agreed capability matrix. Unsupported operations are explicit; profiles remain isolated. Re-run shared tests and native conformance checks after integration. |
| **M5 — Pass the beauty and speed gates** | Core design/performance review plus each native owner                | The visual and performance checklist below passes on an integrated build; popup layering, input, accessibility and real web content are validated together. Review actual captures and measured data rather than approving from source alone.                                                          |
| **M6 — Package experimental builds**     | Platform owners; release changes coordinated by core                 | Native package/install/update-prerequisite checks, sandbox/security review, licensed dependencies, documented limitations, UI evidence and smoke CI. Publish an OS only after its own gates pass; continue the existing macOS release guarantees.                                                      |

Platform agents can explore M1/M3 in parallel while core establishes M2. The dependency is on an agreed contract and framework baseline, not on waiting for every other platform to finish. No calendar estimate is committed until the native prototypes expose their actual blockers.

### Keep it beautiful: one identity, native manners

Use [`BRAND.md`](../BRAND.md) as the visual source of truth. Preserve and improve Felinaut's visual identity across platforms: the happy astronaut cat, yarn planets, six themes, clear hierarchy, quiet content area and coherent rounded shapes. Reuse the existing GPUI interface where it works, and redesign or replace components where needed for usability, accessibility, performance or native integration. This does not freeze the current UI or promise unimplemented mascot behaviors.

- Preserve theme keys and existing macOS preferences. Centralize shared color/spacing/control-state decisions only where doing so removes real duplication.
- Build a portable typography policy: use appropriate installed platform fonts and reviewed wordmark fallbacks rather than hardcoding `.AppleSystemUIFont` everywhere or downloading a font. Check metrics, glyph coverage, baseline alignment, truncation and IME text.
- Respect native shortcut, menu, title-bar, window-control, scrolling, dialog and accessibility conventions. Shared identity does not mean identical OS chrome or pixel-identical font rasterization.
- Give controls deliberate hover/pressed/disabled/selected/focus states, usable hit targets, complete keyboard paths, and accessible names/roles/values/actions.
- Retain tested contrast targets from the brand guide: 4.5:1 for normal/muted text and 3:1 for primary control accents on their surfaces. Check focus and selection visibility across every theme.
- Test small/wide windows and representative 100%, 150% and 200% scaling where supported, plus display changes and popup placement near edges. No clipped controls, misplaced IME candidates or popovers behind the page.
- Keep decoration outside website content. Preserve animation-off and Reduce Motion behavior, pause decorative motion while inactive, and retain a complete static scene. Do not raise the existing decorative animation cadence just to claim a higher frame rate.
- Capture the same clean demo states on each OS: New Tab, sidebar/pins/split, suggestions, appearance, audio and downloads. Store screenshots/video in visual PR descriptions; review intended platform differences rather than requiring a brittle whole-window pixel match.

### Keep it fast: measure the browser, not the dependency count

The following are **engineering targets to validate**, not measured current results or promises for arbitrary hardware. Establish reference machines/workloads and numerical regression tolerances in M0 before turning these targets into automated gates.

| Area                         | Target and evidence                                                                                                                                                                                                                                                                                               |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **UI responsiveness**        | Keep chrome layout/paint within the display frame budget under normal reference workloads: approximately 16.7 ms at 60 Hz, 8.3 ms at 120 Hz where supported. Record frame-time percentiles and missed frames; measure input-to-visible-response as well, since paint time alone does not establish input latency. |
| **Startup / tab operations** | Measure cold/warm launch to usable chrome, time to first usable page, tab switching and popup opening separately. No unexplained sustained regression in the preserved macOS path relative to its repeatable baseline.                                                                                            |
| **Idle / power**             | Static or inactive chrome must not trigger unnecessary continuous redraws. Decorative timers pause correctly; background polling/work is bounded and measured. Measure CPU/GPU activity and power where tools permit, rather than equating low UI CPU with low total power.                                       |
| **Memory / lifecycle**       | Track the full engine process tree, not just Felinaut's host process. Repeated tab/popup open-close and renderer-crash cycles must settle without continuing leaks or orphaned helpers; do not silently discard user pages merely to lower the number.                                                            |
| **Page presentation**        | Record frame copies, transfers, buffer lifetimes, synchronization and scroll latency for the WPE integration. Start correct, then optimize the measured bottleneck. Zero-copy is a prototype result to prove, not an architecture slogan.                                                                         |
| **Background work**          | No blocking network/disk/subprocess waits on the UI thread; bounded queues and cancellation. Cache reusable assets and virtualize lists where workload measurements justify it; do not add premature caching or a second UI framework.                                                                            |

Use offline/loopback fixtures for empty New Tab, many tabs, a long sidebar, typing/IME, scrolling content, local media and downloads. Keep build mode, engine configuration and workload comparable, run repeated trials, and report measurement variance. Cross-OS/engine differences are not a valid controlled benchmark of GTK itself. GTK-free remains a Linux integration goal, not proof of greater speed.

### Experimental backend comparison, after feasibility

Plan a [small backend picker](web-engines.md#proposed-experimental-backend-picker) for comparing available adapters on the same machine. Native WebKit versus WPE on macOS is a useful research goal, but first requires a working, safely embedded WPE macOS prototype. This optional investigation must not block the shared contract or Linux/Windows work. Start with a startup/developer selection, isolate website data, identify the actual engine version, and compare equivalent local workloads; no live page migration or pretend WPE option.

### Readiness rule

A platform is not ready merely because it compiles, draws the mascot, or passes fake-adapter tests. Require native input/accessibility, safe engine integration, real UI evidence, compatible persistence, measured performance, usable packaging and an engine-security-update owner. Missing features must be visible in the capability matrix, never replaced with successful no-ops.

**First actionable task on this Mac:** capture the macOS visual/performance baseline, review the GPUI control/accessibility baseline, and turn the minimal engine-neutral contract into tested code while preserving the current WKWebView adapter. Linux/Windows agents report shell/backend prototype results against the same agreed revision. Do not start a Gecko integration or engine picker before this foundation is proven.

## Technical direction and supporting investigation

Keep **GPUI** for browser chrome and input. Port Felinaut's platform-specific integrations, not Zed's entire editor or a new GUI toolkit.

For a GTK-free Linux browser, investigate **GPUI + WPE WebKit** first. For Windows, investigate **GPUI + Wry/WebView2**. Keep **GPUI + Wry/WKWebView** on macOS. These are proposed platform defaults, not two interchangeable engine choices on every OS.

WPE's documented platform support does not include Windows or macOS. Choosing it for Linux would not replace the native engine on the other platforms.

Keep the architecture open to additional engine families, including a separately investigated Gecko/Firefox backend. Follow the [portable engine-backend contract](engine-backends.md): host platform, engine family, and embedding backend are separate concepts. Do not bake WebKit/Wry objects or a permanent OS-to-engine mapping into shared state.

## What Zed uses for input

Zed draws its own UI with GPUI and connects to native windowing and text-input services. Its text fields are not GTK entry widgets or native text-field controls.

| Platform        | Window, keyboard, and pointer input                                                              | Text composition / IME path in the inspected code                                                                 |
| --------------- | ------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| macOS           | AppKit events, including `NSEvent`.                                                              | `NSTextInputContext` / `NSTextInputClient` for committed text, marked text, selection, and candidate positioning. |
| Linux / Wayland | Wayland keyboard/pointer protocols, with xkbcommon keyboard interpretation and Compose handling. | Wayland `text-input-v3`; actual availability depends on the compositor and IME configuration.                     |
| Linux / X11     | x11rb/XCB, XInput2/XKB, and xkbcommon.                                                           | XIM integration through `zed-xim`, requiring a compatible input-method service.                                   |
| Windows         | Win32 keyboard, character, pointer, and wheel messages.                                          | The inspected implementation uses IMM32 composition messages; do not describe it as a TSF implementation.         |

Felinaut already uses this architecture for its address bar: [`src/input.rs`](../src/input.rs) is adapted from the GPUI input example and implements `EntityInputHandler`, including UTF-16 range conversions and marked text. Website fields are a separate input destination: today the native `WKWebView` handles them.

**Version caveat:** our frozen crates.io dependency is `gpui = "=0.2.2"`. That published crate already contains macOS, Linux, and Windows platform backends, but our Cargo configuration disables default features and enables only `font-kit`, not the Linux `wayland`/`x11` features. Current Zed source has moved platform implementations into separate crates. Its manifest version alone does not establish API compatibility with our published dependency. Do not copy a current `gpui_platform` example or upgrade GPUI without a specific compatibility check.

Backend presence is not a guarantee of complete input support. The inspected Wayland path has limitations around preedit cursor offsets and surrounding-text deletion; XIM handling also has preedit/caret limitations. Test real language/input-method combinations rather than treating “IME supported” as a single checkbox.

## Avoiding GTK: what that does and does not mean

**We do not need to replace our GUI toolkit to avoid GTK widgets.** GPUI already supplies the browser's tabs, sidebar, popovers, address bar, artwork, and other chrome.

The distinction is the web-content backend:

- **Wry on Linux uses WebKitGTK.** Keeping GPUI does not remove the GTK dependency that comes with that engine integration. In Wry 0.57.1, native child webviews on Linux are X11-only; the documented Wayland integration uses a GTK container and its event loop. A cross-platform raw window handle is not sufficient to make that work on Wayland.
- **WPE does not provide a GTK/Cocoa widget.** Its modern WPEPlatform API supports rendering/input integration without a GTK widget tree. That creates a possible GTK-free route, but Felinaut would need a separate Linux web backend rather than a Wry engine-selection flag.
- **GTK-free is not GLib-free or dependency-free.** WPE has a native dependency stack, potentially including GLib/GIO, networking, graphics, font, and GStreamer/media libraries. Audit the final dependency graph, including optional file dialogs and desktop integration, before advertising a GTK-free build.
- **GTK-free is not automatically faster.** Web content, engine processes, decoding, GPU synchronization, and frame copies can dominate. GPUI is GPU-accelerated already; retaining GPUI and eliminating a GTK embedding layer is an architectural choice, not a demonstrated speedup.

WPE's existing Wayland output creates its own presentation surfaces. Do not assume that displaying a separate WPE window proves it can be embedded in a GPUI browser window. Wayland does not offer arbitrary X11-style foreign-window reparenting.

The Linux prototype must prove that we can present the page inside GPUI's content area and route input correctly. Start with the simplest correct buffer path available, even if it copies pixels. Investigate GPU buffer import only after verifying the actual WPE/GPUI APIs, ownership, synchronization, supported formats/modifiers, scaling, and driver behavior. Headless output alone is not a GPUI texture integration, and zero-copy rendering must be measured rather than assumed.

## Proposed platform matrix

| Platform        | Chrome/input                    | Proposed web backend                               | Main investigation                                                                                                                  |
| --------------- | ------------------------------- | -------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| macOS           | Existing GPUI platform support. | Existing Wry + system `WKWebView`.                 | Preserve behavior and existing profiles while isolating native adapters.                                                            |
| Windows         | GPUI Win32 support.             | Wry + WebView2.                                    | Embed/focus the webview, handle runtime availability, and implement Windows browser services. This uses Chromium/Blink, not WebKit. |
| Linux / Wayland | GPUI Wayland support.           | Experimental WPE adapter.                          | Shared presentation and event-loop integration, page input/IME, media, and sandboxed distribution without GTK.                      |
| Linux / X11     | GPUI X11 support.               | Evaluate the same WPE adapter after Wayland proof. | Confirm the chosen buffer/presentation design works on X11; do not assume it follows automatically from GPUI support.               |

WebKitGTK through Wry remains an alternative Linux approach if integration simplicity matters more than avoiding GTK. It is not a hidden fallback in a build advertised as GTK-free. CEF would be a different investigation with its own substantial runtime/distribution burden; it should not be added merely to replace a widget toolkit.

## Shared branch and platform-agent handoff

`multiplatform` is the shared development/integration branch. It starts with planning documents only; production `main` and the macOS release workflow remain unchanged. Linux and Windows native work will be validated on the user's corresponding machines; a macOS-only result is not evidence that those ports work.

- Create focused work branches from the latest `origin/multiplatform`, for example `port-macos-backend-boundary`, `port-linux-wpe-spike`, or `port-windows-webview2-spike`. Open integration PRs **against `multiplatform`**, not `main`, unless explicitly agreed otherwise. Do not overwrite or force-push the shared branch.
- Coordinate a small shared page/event/capability contract first. Platform agents may explore isolated prototypes while it is reviewed, but must not independently introduce incompatible shared interfaces.
- The macOS/core coordinator owns the initial shared boundary, existing native adapter extraction, portable browser models, and contract tests. The Linux agent owns its GPUI/WPE presentation/input investigation; the Windows agent owns its GPUI/WebView2 investigation. New module layout is to be agreed before edits, not implied to exist by this plan.
- Coordinate edits to `Cargo.toml`, `Cargo.lock`, shared UI/state, the backend contract and CI before making them. The macOS/core coordinator approves shared-boundary/manifest/GPUI/CI changes after review with affected platform owners; record the responsible owner and agreed base commit in the handoff. If ownership is unclear, propose the shared change for review rather than landing it independently. GPUI baseline/accessibility upgrades are separate reviewed work; do not have each platform upgrade independently. Keep platform changes in disjoint adapters wherever possible.
- Every handoff/PR records the source commit, OS/architecture, prerequisites, actual commands/results, supported capabilities, unresolved failures and next step. Attach clean-profile screenshots/video for UI changes. Keep prototype commits separate from production adapter refactors; do not merge an unsafe or nonfunctional stub merely to unblock compilation. Before integrating a platform adapter, bring it onto the agreed integrated core revision and rerun relevant checks on its native machine; isolated spikes against different contracts do not prove they compose.
- Run relevant checks on each native machine. Existing release CI targets macOS and does not automatically validate these work branches; decide and add platform smoke CI in a focused follow-up rather than claiming coverage from the existing release job.

### First macOS/core slice

Preserve current behavior while establishing the seam other agents can target: review the control/accessibility baseline, specify the minimum page/event/capability contract, and then isolate the current WKWebView implementation behind it with model/conformance tests. Preserve macOS profile locations, queued callbacks, navigation policy, downloads, native input, and popup stacking. Do not implement WPE on macOS, change engines, or add a selector in this first slice. Publish the reviewed contract before Linux/Windows production adapters depend on it.

## Phased plan and acceptance gates

### 0. Audit control coverage and choose an accessibility baseline

Use the [GPUI control inventory](gpui-controls.md) to distinguish core primitives, Felinaut's custom controls, native services, and Zed's separate component libraries. GPUI can draw the required controls, but the published pin is not a complete widget library and lacks current upstream's semantic accessibility-tree interface for custom chrome.

- Inventory buttons, input/autocomplete, checkboxes/choices, sliders, lists, tabs/splits, menus/popovers, scrolling, progress, and native dialogs. Separate existing behavior from missing keyboard, accessibility, and platform integration.
- Establish an accessibility-capable framework strategy before making reusable control/API decisions. Evaluate any GPUI upgrade/backport separately; do not assume current Zed components work with the published crate or share its license.
- Plan a small gallery using real control implementations, with behavioral unit tests and native keyboard/IME/accessibility checks. Include popup layering over a real webview, not just a GPUI-only window.

**Gate:** documented control gaps, a reviewed framework/component compatibility and licensing decision, and a concrete plan to close focus/accessibility issues. Do not treat screenshots, successful mouse clicks, or an upstream component catalog as proof that all required controls are complete.

### 1. Establish baselines and prove the GPUI shell

- Inventory the macOS-only calls in [`src/webview.rs`](../src/webview.rs), [`src/downloads/native.rs`](../src/downloads/native.rs), menus, appearance preferences, automation, and persistence paths.
- Check the published GPUI dependency's required features, build tools, and window-handle compatibility on native Windows and Linux runners. Do not start by upgrading every dependency.
- Build a small GPUI shell with the real address input on Windows, Linux Wayland, and Linux X11. This isolates toolkit/input failures from engine-embedding failures; a shell is not yet a working browser port.
- Establish a repeatable macOS baseline using loopback fixtures: startup to usable window, input latency, idle CPU, UI frame times, and whole-process-tree memory.

**Gate:** native shell builds and basic input work on each candidate environment, with documented IME gaps. No change to macOS storage or behavior.

### 2. Prove backend integration before broad refactoring

Windows and Linux experiments can proceed independently once the shell works:

- **Windows:** display a loopback page in Wry/WebView2 inside the GPUI window. Exercise focus handoff, browser shortcuts, typing, resize/HiDPI, popovers, popup requests, and a clean shutdown. Detect a missing WebView2 runtime explicitly.
- **Linux:** pin a released WPE version and prove a page rendered inside GPUI, not in an unrelated toplevel. Coordinate the GLib/WPE and GPUI event loops without blocking rendering or re-entering GPUI callbacks. Exercise text input, scrolling, resize, tab visibility, and renderer crash/shutdown behavior. Keep engine subprocess isolation enabled.
- Record dependencies, actual graphics-buffer paths, copy costs, desktop prerequisites, and available sandboxing. Prove operation without GTK libraries for the intended GTK-free configuration.

**Gate:** an integrated page with correct focus/input, bounded frame-buffer lifetimes, and a viable security model. Stop and reassess if this requires an unmaintainable GPUI/WPE fork. Do not substitute JavaScript-generated input events for native engine input or disable a sandbox to declare success.

### 3. Extract only the platform boundaries the prototypes need

- Keep browser state and the existing queued page-event model platform-neutral. Implement the reviewed [engine-backend contract](engine-backends.md) for navigation, bounds/visibility, focus, history, scripts, load failures, zoom, and capabilities. Do not expose native WebKit, WPE, COM, CEF, or future Gecko objects through shared models.
- Put native view ownership, input injection, engine snapshots, downloads, and permission/TLS decisions in backend-specific adapters. Unsupported features must return explicit capabilities/errors rather than silently succeed.
- Make dependencies target-specific: a WPE Linux build must not pull Wry's Linux WebKitGTK stack just because Windows/macOS still use Wry.
- Add platform services for profile/download locations, open/reveal-file actions, menus, appearance/accessibility preferences, and automation transport. Existing macOS profile paths must remain unchanged; Linux XDG directories and Windows known folders require their own implementations and tests.
- Keep hidden live-folder pages in the correct engine/profile and prohibit hidden-page downloads. Do not copy cookies or open one engine's data directories with another engine.

**Gate:** macOS regression tests remain green, pure state transitions stay independently testable, and new adapters do not expose more native authority to web content.

### 4. Finish input, browser behavior, and accessibility

Maintain two focus/input domains: **GPUI chrome** and **web content**. One focused destination must receive each event; browser shortcuts must not also type characters into a page.

For a custom WPE content surface, prove key-down/up, layout interpretation, modifiers, committed text, preedit/commit/cancel, candidate placement, caret/selection queries, clipboard, and pointer/scroll coordinates. Composition is not just sending one character for each key press. Translate between engine/GPUI coordinate and string-index conventions explicitly and avoid duplicate text insertion when both key and composition events occur.

Test:

- Ctrl/Cmd platform shortcut conventions, AltGr, dead keys, Compose sequences, repeat, selection, and shortcuts while a page owns focus.
- Non-Latin text, surrogate pairs, combining marks, and emoji; Japanese/Chinese/Korean composition in both the address bar and website fields.
- Representative IBus/Fcitx and Wayland compositor configurations, XIM on X11, and Windows IMEs. Record unsupported combinations instead of promising all desktop setups.
- Touchpad scrolling, fractional scaling, popup layering, drag/drop, file choosers, fullscreen, screen-reader access, reduced motion, and focus after renderer crashes.
- Navigation/errors/TLS, downloads and cancellation, storage, session restoration, live folders, audio/media, automation, and capability-specific limitations. Do not grant broad certificate exceptions to paper over integration bugs.

**Gate:** a documented feature matrix, safe failures, and no critical input/accessibility/security regressions. A backend with no usable accessibility path is not ready for a general browser release.

### 5. Measure and package experimental builds

- Compare the same local workloads and comparable engine versions/build configurations where possible. Track UI input-to-frame latency, page scrolling, startup, idle CPU, all engine-process memory, frame copies, media playback, and power. Report backend/platform differences instead of attributing every change to GTK.
- Add native CI jobs and package checks only for targets we can build and validate. Preserve the current macOS architecture/signing gates. New engine libraries/helpers need exact architecture, runtime-dependency, license, and signature checks appropriate to their platform.
- Validate WPE subprocess sandboxing and patch/update distribution. Validate WebView2 runtime deployment/update expectations separately. No engine switch may weaken the native message interface or navigation rules.
- Publish clearly labeled experimental Windows/Linux artifacts only after the corresponding gates pass, with installation prerequisites and known limitations. Keep macOS-only claims until real builds have been validated.

**Gate:** reproducible packages, a security-update owner/process, native smoke tests, and real UI screenshots/videos for the PR/release documentation. Do not choose a new default based on an unmeasured claim that a toolkit is faster.

## Test strategy

Every implemented feature needs behavioral unit tests, including empty/error/invalid-input cases. Cover backend selection and capabilities, event routing, focus transitions, input index/coordinate conversions, cancellation, profile separation, and persistence compatibility. Keep unit tests independent of native browser startup.

Keep engine integration tests opt-in, loopback-only, and independent of personal accounts. Add native OS tests for real composition/focus, page input, renderer crashes, and downloads. A test skipped on macOS is not evidence that Windows/Linux works. Run existing formatter/linter/model and packaging tests for the code affected by each focused change.

## First implementation slice

Start the macOS/core slice above on this machine and publish the reviewed contract for the other agents. On the Linux box, start with **the control/accessibility audit and a native GPUI shell/control-gallery smoke test**, followed by a separate WPE-in-GPUI rendering prototype. On the Windows box, investigate GPUI/WebView2 once the control/framework baseline is established. Coordinate shared-file ownership and keep native platform claims tied to actual results.

Do not begin with a user-facing engine picker, a GUI toolkit rewrite, a WPE macOS port, or a large browser-state refactor. The first question is whether the proposed platform/backend pairs can render, accept input, and remain safe—not how the selector should look.

## Sources and version scope

- [Published GPUI 0.2.2 platform selection](https://docs.rs/crate/gpui/0.2.2/source/src/platform.rs), [Cargo features](https://docs.rs/crate/gpui/0.2.2/source/Cargo.toml), and [input example](https://docs.rs/crate/gpui/0.2.2/source/examples/input.rs).
- Published input backends: [macOS](https://docs.rs/crate/gpui/0.2.2/source/src/platform/mac/window.rs), [Wayland](https://docs.rs/crate/gpui/0.2.2/source/src/platform/linux/wayland/client.rs), [X11](https://docs.rs/crate/gpui/0.2.2/source/src/platform/linux/x11/client.rs), and [Windows](https://docs.rs/crate/gpui/0.2.2/source/src/platform/windows/events.rs).
- Current Zed platform organization, inspected at revision `7fab4c0527e10a466439b819d4e9e7fe34cd989c`: [platform selection](https://github.com/zed-industries/zed/blob/7fab4c0527e10a466439b819d4e9e7fe34cd989c/crates/gpui_platform/src/gpui_platform.rs), [Wayland input](https://github.com/zed-industries/zed/blob/7fab4c0527e10a466439b819d4e9e7fe34cd989c/crates/gpui_linux/src/linux/wayland/client.rs), [Windows input](https://github.com/zed-industries/zed/blob/7fab4c0527e10a466439b819d4e9e7fe34cd989c/crates/gpui_windows/src/events.rs). These sources are not interchangeable with the published crate.
- [Wry 0.57.1 platform considerations and child-view limitations](https://docs.rs/wry/0.57.1/wry/).
- [WPE architecture: toolkit-independent rendering, WPEPlatform, and input](https://wpewebkit.org/about/architecture.html) and [FAQ: platform support, dialogs, media, and dependencies](https://wpewebkit.org/about/faq.html).
