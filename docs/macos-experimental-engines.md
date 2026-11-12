# Experimental engines on macOS: Servo, Ladybird, Ultralight

**Research date: 12 November 2026 (UTC). Status: primary-source/API feasibility investigation, not an implemented adapter or native engine test.**

This document adds three candidates to the evaluation alongside the agreed [per-tab engine-family target](engine-backends.md), [porting plan](porting-plan.md), and [build-request gates](engine-builds.md). This source/API report adds no dependencies or runnable adapter. The primary target remains WebKit / CEF / feasibility-gated Gecko, with these additional candidates under evaluation. These additional candidates are feasibility-gated too; naming them here does not reserve an enabled picker option.

## Decision in brief

- **Servo: pursue the first open-source embedding spike**, initially trusted loopback content only. A concrete Rust embedding API and a newer, narrower C API exist. The immediate security blocker on this **Apple Silicon** host is not hypothetical: the inspected source excludes `aarch64` from sandbox activation and uses an unsandboxed child-spawn path. An arm64 browser download is not evidence of arm64 sandbox support.
- **Ladybird: investigate a new frontend boundary, not a drop-in webview.** Current source uses a Qt frontend, a separate Hub, and a Compositor with macOS IOSurface/CoreAnimation presentation. There is useful implementation code, but no maintained, independently packaged Rust/C desktop webview SDK was established in this review. Replacing Qt's frontend while retaining Hub/process isolation is substantial work. Older AppKit-port descriptions do not describe this pin.
- **Ultralight: technically the shortest SDK-to-pixels path, but only a trusted-content experiment.** There is a current macOS arm64 Free SDK and official C/C++ interfaces. It is a proprietary, single-process WebKit fork, not WKWebView, not a new independent engine lineage, and not a safe arbitrary-site replacement for the current backend. Missing screen-reader exposure is an explicit upstream limitation. Security isolation, restricted web APIs, edition gates, and licensing are independent blockers.
- **What can actually be provided on this Mac today:** the already functional system WKWebView and the separately prepared upstream **Cocoa** WebKit comparison, with isolated temporary homes. See the [existing comparison report](macos-webkit-comparison.md). That report records functional smoke for both runtimes; it is not WPE, a security audit, or a performance result. **No Servo, Ladybird, or Ultralight page can currently be opened through a Felinaut adapter.**

## Evidence and execution boundary

The native environment was checked with `sw_vers`, `uname -m`, `xcodebuild -version`, and `df -h .`:

| Item                       | Observed                                                          |
| -------------------------- | ----------------------------------------------------------------- |
| OS                         | macOS 27.0, build `26A428`                                        |
| Architecture               | `arm64`                                                           |
| Xcode                      | 27.0, build `27A266a`                                             |
| Available filesystem space | Approximately **16 GiB** at the start of this review              |
| Existing application       | Rust edition 2024; published GPUI **0.2.2**, Wry/native WKWebView |

**Checked:** official documentation, individual upstream source files, repository listings, release metadata and Ultralight's public package metadata. Only small text/JSON research responses were fetched; a repository _listing_ is not a source-tree checkout. Servo's recursive listing was truncated by GitHub, so the C API was checked separately through its directory listing and files. Negative API findings below are scoped to the sources reviewed, not a proof that no third-party project exists.

**Not done:** SDK/archive downloads, source clones, dependency installation, engine builds, C/Rust linkage, browser launches, account access, license acceptance, adapter implementation, GPU/input tests, sandbox probes, performance measurements, signing or redistribution. The host having Xcode does not establish that all engine prerequisites are installed. About 16 GiB free is **not an approved full-build budget**; no reliable total build-space estimate was established for either open-source engine.

### Pins and freshness

| Evidence set           | Inspected pin / version                                                      | Meaning                                                                                                                                                 |
| ---------------------- | ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Servo source           | [`772a8ec259a71a13600409db861e1a9ef848db89`][S-pin], committed 2026-11-12    | API/security source pin; workspace version says 0.8.0. Not a claim that the released 0.8.0 artifacts contain every API at this later commit.            |
| Servo release metadata | [`v0.8.0`][S-release], published 2026-11-09                                  | Official standalone servoshell artifacts; not a GPUI adapter or macOS C SDK.                                                                            |
| Servo Book source      | [`a0b844736cee945ee7832fbad02bb7c2ff60d69c`][S-book], committed 2026-11-10   | Pin for build/embedding/LTS documentation; source code wins where prose lags.                                                                           |
| Ladybird source        | [`093b084d9b575d4509a2e676e5cf021ffbc22675`][L-pin], committed 2026-11-12    | Current frontend/process/build evidence, not an SDK release.                                                                                            |
| Ultralight             | **`2.0.0-beta.3`, Free, release build**, package metadata checked 2026-11-12 | Public API reference is labelled 2.0.0; documentation is versioned `/docs/2.0/`, but mutable. Package bytes and headers were **not** downloaded.        |
| Ultralight legal       | [SDK License Agreement, version **2026-08**][U-terms]                        | Version-addressed commercial terms; Free eligibility/terms and applicability to an open-source browser still need legal review. No terms accepted here. |

Servo and Ladybird source links below are commit-pinned. Ultralight API/doc version URLs are better than unversioned marketing but are **not immutable source commits or proof of beta.3 ABI correctness**. Old `docs.ultralig.ht` guides and the public `ultralight-ux/Ultralight` sample repository are not the authority for 2.0: the latter's inspected HEAD was a 2023 commit. In particular, its old $100K licensing threshold, AppCore licensing, and claims that arm64 is still forthcoming are stale relative to current sources.

## Comparison matrix

“Available” means upstream evidence, not successful execution here.

| Criterion                   | Servo                                                                                               | Ladybird                                                                                                                       | Ultralight 2.0                                                                                             |
| --------------------------- | --------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------- |
| Engine lineage / JS         | Independent Servo engine; SpiderMonkey, not Gecko browser embedding                                 | Independent LibWeb + LibJS; Rust migration does not create a Rust embedder API                                                 | Fork of WebCore + JavaScriptCore; upstream identifies baseline as WebKit **615.1.18 / Safari 16.4 (2023)** |
| Concrete entry surface      | Rust `ServoBuilder`, `WebViewBuilder`, `RenderingContext`, delegates; `servo-capi` C wrapper        | C++ internal frontend/IPC: `Ladybird::RemoteView`, `WebContentView`, `HubConnection`; Hub's `WebView::ViewImplementation`      | Official C++ `Renderer::Create`, `CreateView`; stable-ABI C interface `ulCreateRenderer`, `ulCreateView`   |
| macOS architecture evidence | Release DMGs for **aarch64 and x86_64**                                                             | Current distribution workflow explicitly includes **macOS arm64**; macOS x86_64 artifact/support not established from this pin | Public release metadata lists separate **macos-arm64 and macos-x64** packages                              |
| Presentation                | Rust window/offscreen/software contexts; WebRender/OpenGL-oriented interface, not a supplied NSView | Compositor → shared images / IOSurface → native CALayer, plus optional private remote-layer hosting                            | Offscreen CPU surface or custom GPUDriver; no automatic GPUI surface or NSView                             |
| Loop/thread ownership       | Embedder-owned thread-affine handles; waker → `spin_event_loop`; explicit paint/present             | Qt UI/AppKit main thread + Core/IPC scheduling; separate Hub and helpers                                                       | Renderer-owned thread; `Update` → `RefreshDisplay` → `Render`; most View calls on that thread              |
| Input                       | Rust input/focus/IME delegates; embedder supplies native routing                                    | Qt event translation, `RemoteView::enqueue_input_event`, composition IPC; must port when removing Qt                           | Explicit key/mouse/scroll, Editor composition/caret APIs; host supplies native input                       |
| Isolation                   | Optional multiprocess feature **and** runtime options; inspected arm64 sandbox path unsupported     | Multi-process, default iframe site isolation; macOS Seatbelt helpers; UI and Hub unsandboxed                                   | **Single process; every View/frame shares a VM. No WKWebView-style renderer sandbox boundary.**            |
| Accessibility               | AccessKit-facing web tree APIs; requires embedding and native validation                            | NSAccessibility wrappers and tree IPC exist, with Qt-specific glue                                                             | Official feature table: **pages not exposed to screen readers**                                            |
| License                     | MPL-2.0 plus dependency licenses                                                                    | BSD-2-Clause plus dependencies, including Qt if retained                                                                       | Proprietary core/renderer + WebKit LGPL/BSD components; 2.0 platform reference code described as Zlib      |
| Obtaining usable bits       | Standalone DMG available; custom embedder requires building/linking Servo                           | Source build/CI artifacts investigation; no published desktop embedding SDK identified                                         | Prebuilt proprietary SDK; Free publicly downloadable without account in metadata flow                      |
| GPUI 0.2.2 effort           | Medium/high: first software frame plausible; GPU/thread/input/profile integration remains           | High: frontend extraction, IPC/runtime packaging, event-loop and presentation work                                             | Lower for first copied CPU frame; high for input/accessibility/security and ship readiness                 |
| Recommended scope now       | First open-source trusted-local embedding feasibility spike                                         | Boundary design / upstream engagement before large build                                                                       | Optional trusted-UI CPU-surface spike after legal/edition gate; **not arbitrary browsing**                 |

All three can execute JavaScript in some configuration. That is not equivalent to implementing browser web APIs, handling arbitrary websites correctly, being safe against hostile documents, or meeting this app's controls/download/storage contract.

## 1. Servo: actual Rust and C entry points

### Rust API: the useful starting point for this app

The pinned [crate exports][S-lib], [minimal winit example][S-example], [Servo implementation][S-servo], and [WebView implementation][S-view] establish this sequence:

1. Initialize required providers/resources. The example installs a Rustls crypto provider before creating Servo. Use baked-in default resources or a complete `ResourceReader`; do not assume a working directory happens to contain them.
2. Build `ServoBuilder::default().event_loop_waker(...).build()`.
3. Construct a rendering context (`SoftwareRenderingContext`, `OffscreenRenderingContext`, `WindowRenderingContext`, or a reviewed implementation of `RenderingContext`).
4. Build a page with `WebViewBuilder::new(&servo, context).url(...).delegate(...).build()`.
5. On wake, schedule `Servo::spin_event_loop()` on the owning thread. On `WebViewDelegate::notify_new_frame_ready`, request a host repaint; call `WebView::paint()` and, for a double-buffered context, `RenderingContext::present()` in the proper drawing phase.
6. Use `WebView::load`, `reload`, `can_go_back`/`go_back`, `can_go_forward`/`go_forward`, `resize`, `set_hidpi_scale_factor`, `show`/`hide`, `set_focused`, `notify_input_event`, and asynchronous `evaluate_javascript` / `take_screenshot` as the starting mapping to Felinaut's page seam.

This is **not** a winit requirement for Felinaut: the example is an API demonstration. Do not run a second `winit::EventLoop::run_app` over GPUI's AppKit loop. The initial experiment should keep engine ownership explicit, enqueue waker/delegate work, and prove one copied software frame in a GPUI content rectangle before attempting shared GPU surfaces.

`Servo` and `WebView` use `Rc`-based state, not thread-safe interchangeable handles. Wakers can originate on internal threads; only enqueue from them. Keep engine calls and disposal on the owner. A worker-owned engine with owned frame buffers is a possible future topology, not something the current UI-thread-owned `PageBackend` already supplies. GPUI's Metal renderer does not make a Servo GL texture directly usable; native context setup, copies/readback, synchronization, color/alpha, clipping and buffer retention need a measured prototype.

A context is the full viewport of its WebView. Resizing a context also affects WebViews sharing that context. For splits, start with distinct contexts and explicit viewport coordinates rather than accidentally resizing every tab. Notifications must carry app-owned page/document generations and drain outside GPUI rendering.

### C API exists, but is narrower and not yet a proven stable SDK

The upstream [`ffi/capi` README][S-capi] explicitly says stable ABI is an **eventual goal**. The [manifest][S-cargo-c] builds a `cdylib`; [builder exports][S-cbuilder], [C WebView exports][S-cview], and [rendering-context exports][S-ccontext] include:

- `servo_builder_create`, `servo_builder_set_options`, `servo_builder_set_preferences`, `servo_builder_set_event_loop_waker`, `servo_builder_build`;
- `servo_spin_event_loop`, `servo_free`;
- `servo_rendering_context_create_software(width, height)`;
- `servo_webview_builder_create`, `servo_webview_builder_set_url`, `servo_webview_builder_set_delegate`, `servo_webview_builder_build`;
- `servo_webview_load`, `servo_webview_resize`, `servo_webview_set_hidpi_scale_factor`, `servo_webview_paint`, `servo_webview_take_screenshot`, `servo_webview_free`.

At this pin, the C rendering-context module exposes software-context creation, not a ready macOS native-child-view/GPU host API. The inspected C WebView module does not expose the Rust API's full input, script-evaluation and history surface. Prefer Rust for the first interactive Felinaut spike; C is worth a separate headless/image feasibility check, not assumed feature parity.

Ownership is explicitly consuming in several C builder calls (for example the rendering-context handle passed to a WebView builder). The waker must be safe from any thread and must not unwind. Several invalid/null/UTF-8 inputs assert or panic rather than becoming recoverable errors. Do not pass untrusted pointers/strings through FFI, infer lifetime rules from names, or build a general page-to-native bridge around these exports. The [upstream C test instructions][S-ctests] use `cargo-c` via a build script; they are not a dependency-free C sample.

### macOS security blocker: arm64 sandbox support

The engine has real multiprocess and sandbox implementation work, not just a future diagram. Nevertheless, feature compilation is not activation. The [manifest][S-cargo] says `multiprocess` needs runtime flags too; [C options][S-copts] expose separate multiprocess and sandbox switches.

More importantly, inspect these exact sources before approving an Apple Silicon trial:

- [`components/constellation/sandboxing.rs`][S-sandbox]: the `aarch64` branch of `spawn_multiprocess` uses `std::process::Command` and returns `Process::Unsandboxed`. A macOS sandbox profile elsewhere in the file does **not** override that branch.
- [`components/servo/servo.rs`][S-servo]: `create_sandbox` excludes `target_arch = "aarch64"` from its working implementation; the other implementation panics that sandboxing is unsupported on ARM and other targets.
- Multiprocess launches the current executable with child startup arguments. A custom embedder must dispatch child mode through `run_content_process` before ordinary application/GPUI initialization; merely enabling a feature will not provide the executable dispatch or package helpers.

**Decision:** no arbitrary-site enablement on this arm64 pin. Do not “fix” the gate by silently turning sandbox off. A trusted-loopback software-frame spike can be explicitly unsandboxed and labelled as such, with no personal profiles or remote navigation. Production work needs an upstream-supported arm64 sandbox path, architecture-specific native tests and a security review. An x86_64 sandbox code path is not validated Intel support on this Mac and is not a reason to rely on Rosetta for security.

### Build, distribution, compatibility and license gates

- [macOS build instructions][S-mac] require Xcode, Homebrew, `uv`, Rustup, `./mach bootstrap`, then `./mach build`. The inspected [Brewfile][S-brew] lists CMake and pkg-config. Bootstrap can also install official GStreamer framework packages with elevated privileges; **do not run it as a harmless preflight**.
- The source [`rust-toolchain.toml`][S-toolchain] pins **1.99.1**; the workspace manifest declares MSRV **1.88.0** and explicitly warns against Rust 1.90 miscompilation. These are different facts. Do not upgrade Felinaut's Rust/dependencies incidentally to match Servo's development toolchain.
- [Embedding instructions][S-embed] explain that direct Cargo builds may need `mach` environment setup, baked-in resources and explicit `media-gstreamer`; media is not implied by a basic library build. Native graphics, SpiderMonkey and transitive toolchain/dependency costs remain even for a “small” embedder.
- [Official 0.8.0 release metadata][S-release] lists `servo-aarch64-apple-darwin.dmg` (**114,300,318 bytes**) and `servo-x86_64-apple-darwin.dmg` (**117,863,286 bytes**) plus checksums. These are standalone demo-browser distributions, not `libservo` SDK artifacts. A later, separately approved DMG smoke could establish shell behavior, not GPUI integration. Nothing downloaded here.
- The pinned workspace license is **MPL-2.0**. Keep modified covered files/source availability and dependency notices under review; this does not automatically relicense the entire host. No legal compatibility conclusion is made solely from Rust linkage.
- The [LTS policy][S-lts] describes best-effort six-month branch cadence / nine-month security-only support and explicitly distinguishes trusted UI from arbitrary browsing risk. It gives no security guarantee. Choose a supported release/branch and security-update owner before production pinning; the researched HEAD is not an instruction to freeze it indefinitely.
- Upstream calls servoshell a test/demo browser, not a fully fledged browser. Neither a DOM+JS fixture nor SpiderMonkey execution establishes media, login, downloads, workers, accessibility or arbitrary-site compatibility. Test each enabled web feature and each optional Felinaut capability explicitly.

**First future deliverable:** isolated engine crate/probe outside shared build changes, one loopback page rendered using the Rust software context, title/load/error/script/snapshot callbacks, clean shutdown, then an actual GPUI rectangle and native input. The arm64 sandbox issue remains a separate hard gate even if every functional probe passes.

## 2. Ladybird: frontend feasibility, not “link LibJS”

### What the current source actually offers

The pinned [build instructions][L-build] list **Qt** as the desktop frontend, Android separately. The reviewed `UI/` listing contains Qt and Android, not the former AppKit frontend. An old article showing an AppKit `WebView` is not a currently supported Cocoa embedding contract.

Useful concrete boundaries in current source:

- [`UI/Qt/RemoteView.h`][L-remote]: `Ladybird::RemoteView(Optional<u64> view_id, Hub::View::InitialState)`; `load(URL::URL const&)`, `load_html`, `reload`, `stop_loading`, `set_has_system_focus`, and `enqueue_input_event(Web::InputEvent)`.
- That header forwards commands through `HubConnection::the().async_view_*`. Composition uses `set_marked_text_from_input_method` and `commit_text_from_input_method`. These are internal C++ classes/protocols, **not stable exported C functions**.
- [`Services/Hub/HubServer.ipc`][L-hub] and `HubClient.ipc` define the UI/Hub protocol. `WebView::ViewImplementation` lives on the Hub side in the present architecture; embedding that class directly in GPUI is not the current frontend model.
- [`UI/Qt/WebContentViewMac.mm`][L-present] demonstrates IOSurface-backed `CALayer.contents`, crop/scale handling, frame ownership and layer stacking. It also optionally looks up private **`CALayerHost`** and uses private `CATransaction` commit handlers for synchronized resizing, including bounded waits for Hub messages.

**Actionable route:** scope a small C++/Objective-C++ shim that owns the UI-side Hub connection and exposes only reviewed, opaque C handles, events and retained frame descriptions to Rust. Preserve Hub and the engine helper architecture. First prove public IOSurface/CALayer presentation without Qt/remote private layers; then determine whether that is a viable supported upstream boundary. This shim does not exist here, and stable protocol/version negotiation must not be assumed.

Do not embed a QWidget into GPUI as the default design or create a competing Qt application loop in its process. That would add another toolkit, plugin/dependency packaging and native event ownership problem. Keeping the upstream standalone Qt app in a separate process is a useful future comparison, but is **not an embedded Felinaut tab**.

### Process and sandbox architecture to preserve

The pinned [process architecture][L-process] documents a substantially more developed topology than “one WebContent child per tab”:

- **UI → Hub → Compositor, WebContent, per-site RequestServer and ImageDecoder**, with additional MediaServer, AudioServer, VideoDecoderServer and WebRTCClient services.
- WebContent hosts LibWeb/LibJS; the UI never owns page DOM/JS heaps. Hub owns shared browsing/session state, launches helpers and brokers privileged operations.
- Default `--site-isolation=iframe` separates cross-site documents. Other modes weaken this; do not use them as a shortcut in acceptance tests.
- Helpers apply macOS **Seatbelt** sandboxes unless explicitly disabled. **UI and Hub are not sandboxed yet.** This is upstream source/documentation evidence, not a tested sandbox on macOS 27.
- macOS helper connections use Mach ports; a ProcessReaper helper handles parent-death cleanup. Packaging only LibWeb/LibJS omits essential runtime and security pieces.

A GPUI adapter must preserve helper identity, broker/IPC restrictions, site isolation, termination/reaping and profile partitioning, and must not copy synchronous Hub waits into GPUI rendering. The current Cocoa comparison's separate-process launcher has no general cross-process page-presentation bridge to reuse.

### Build and support evidence

The pinned [build guide][L-build] requires **CMake 3.30+**, a **C++23** compiler, **Qt 6.10+** development packages, Rust, nasm and other build tools. Its macOS section mentions Xcode 15 or Homebrew Clang and lists autoconf/autoconf-archive, automake, ccache, CMake, libtool, make, nasm, Ninja and pkg-config. CI currently uses GCC 16 / Clang 22; the documented macOS Xcode minimum is not proof that Apple Clang 27 successfully builds this pin. Dependencies are managed through vcpkg and include substantial native libraries such as Skia. Rust usage inside LibWeb is not an externally consumable Rust API.

`./Meta/ladybird.py run` **configures, prepares dependencies and builds**; it is not a safe read-only launch command. The documented `--no-build` mode avoids that only after suitable products already exist. No such Ladybird products were established here. The guide also says CMake's generated Xcode project is not a supported way to build Ladybird.

The pinned [distribution workflow][L-release-ci] explicitly builds **macOS arm64** on a macOS 26 runner. This is useful Apple Silicon evidence, not a run on this macOS 27 host. No current macOS Intel release artifact was established in this review; investigate before making an x86_64 or universal claim. The official [site][L-site] still describes the first macOS/Linux alpha as forthcoming; GitHub's latest-release API returned **404** on the research date. Build workflows and CI artifacts are not a released embedding SDK, and no download-ready artifact was authenticated or fetched here.

The [BSD-2-Clause license][L-license] is permissive with attribution requirements; dependencies need their own audit. Keeping Qt entails Qt licensing and its platform plugins. Removing Qt does not remove every third-party license or reduce the engine to a single small library.

### JS and native completeness

LibJS execution alone proves no HTML loading, networking, layout, compositor presentation or sandbox. The upstream site even distinguishes Windows JS CI from full browser-engine support; apply the same distinction to any macOS REPL experiment. Prefer a complete loopback page through the actual process architecture.

Ladybird has [macOS screen-reader infrastructure][L-ax]: NSAccessibility wrapper objects, tree IPC and a native overlay, currently wired through Qt-specific glue/swizzles. Reusing a frame texture does not transplant that accessibility implementation into GPUI. Port/review native input, IME, clipboard, focus, accessibility and overlays alongside rendering, not after declaring the frontend done.

**First future deliverable:** a boundary/ownership proposal with upstream confirmation, then an isolated UI/Hub/frame probe preserving default sandbox/site isolation and all helper products. Stop if it requires private-API-only presentation, GUI-thread blocking, or transplanting the whole Qt application into GPUI. Obtain build-space funding or prepared trusted products before scheduling a full build on this nearly full disk.

## 3. Ultralight: real SDK, different threat model

### Current downloadable macOS packages

The [public download page][U-download] documents a metadata-only query (`format=json`) and a [release listing][U-releases]. Both were checked without logging in or downloading the SDK. The listing reports:

| Version / edition             | Platform      | Filename                                | Publisher-reported SHA-256                                         |
| ----------------------------- | ------------- | --------------------------------------- | ------------------------------------------------------------------ |
| `2.0.0-beta.3`, Free, release | `macos-arm64` | `ul-sdk-2.0.0-beta.3-mac-arm64-free.7z` | `48be6b4788e1f6df8dcdf748aa0c2f67ff0fd84608e32d1e97549124b115feb8` |
| `2.0.0-beta.3`, Free, release | `macos-x64`   | `ul-sdk-2.0.0-beta.3-mac-x64-free.7z`   | `afa54a71e807be6a9588fe70cc7a2af59e544a599d2421c6c19eb6efe505ea0b` |

The service labels this beta package's channel `stable`; **that is the service's channel name, not a claim of production readiness**. A metadata lookup returns a short-lived signed object URL; do not commit or treat it as a permanent evidence link. Record the versioned metadata endpoint, filename and digest instead. Digests were read, **not verified against downloaded bytes**. Compressed/extracted size, signing, Mach-O slices and actual minimum OS load commands remain unchecked.

Current [2.0 prerequisites][U-prereq] say macOS **11.3+**, Xcode or CLT, CMake **3.15+**. Manual integration requires C17 or C++20. Those published prerequisites are not a native macOS 27 compatibility result. Do not repeat the older guide's “Sierra or later” for the 2.0 SDK.

### Concrete C/C++ APIs and GPUI route

The official [C API][U-c] calls itself a portable stable-ABI interface. The [2.0 architecture][U-arch], [C renderer guide][U-crender], [C View guide][U-cview] and [C++ View API][U-view] support these concrete entry points:

| Operation               | C                                                                                          | C++                                                                          |
| ----------------------- | ------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| Runtime/config          | `ulCreateConfig`, `ulCreateRenderer`, `ulDestroyRenderer`                                  | Configure `Platform::instance()`, then `Renderer::Create()`                  |
| Isolated website data   | `ulCreateSession(renderer, false, name)`                                                   | `Renderer::CreateSession(false, name)`                                       |
| View                    | `ulCreateView`, `ulDestroyView`                                                            | `Renderer::CreateView`                                                       |
| Page/navigation         | `ulViewLoadURL`, `ulViewLoadHTML`                                                          | `View::LoadURL`, `LoadHTML`, history/reload/stop methods                     |
| Pump/render             | `ulUpdate`, `ulRefreshDisplay`, `ulRender`                                                 | `Renderer::Update`, `RefreshDisplay`, `Render`                               |
| CPU output              | `ulViewGetSurface`, `ulSurfaceLockPixels`, `ulSurfaceGetRowBytes`, `ulSurfaceUnlockPixels` | `View::surface`, `BitmapSurface` / `Bitmap`                                  |
| Input                   | `ulViewFireMouseEvent`, `ulViewFireKeyEvent`, `ulViewFireScrollEvent`, `ulViewGetEditor`   | `FireMouseEvent`, `FireKeyEvent`, `FireScrollEvent`, `editor()`              |
| Callbacks               | `ulViewSetDOMReadyCallback`, `ulViewSetChangeTitleCallback`, other individual setters      | `LoadListener`, `ViewListener`, `EditorListener`, download/network listeners |
| Cross-thread submission | `ulRendererPostTask`                                                                       | `Renderer::PostTask`                                                         |

Use the core renderer, **not `App::Run()` / AppCore-owned windows**, for an existing GPUI host. Rust can call the C ABI through a minimal audited FFI layer without adopting a community wrapper of unknown SDK-version compatibility. Public C names are not permission to assume the old 1.4 callback signatures: 2.0 C callbacks include ownership/cleanup conventions that need exact matching to downloaded headers.

**First presentation path:** CPU View (`is_accelerated=false`) → lock changed surface → copy rows respecting stride and premultiplied pixel format → unlock → publish an owned image to GPUI. Do not retain a borrowed surface pointer after resize/destruction or while GPUI still uses it. CPU copying is an honest first feasibility step, not a low-memory or low-latency claim.

The GPU path requires a `GPUDriver`/C GPU driver implementation and correct texture/command lifetime. The engine's texture IDs are not automatically GPUI Metal textures. Reference platform implementations may help, but neither an SDK GPU driver nor the word “Metal” proves zero-copy integration.

Follow the [frame-loop guide][U-loop]: `Update` dispatches timers/tasks/JS; `RefreshDisplay` advances animation; `Render` paints. Most calls stay on the Renderer creation thread; documented posting/data-context exceptions do not make general View/DOM use thread-safe. A single owner worker plus bounded command/frame queues is a reasonable spike design, not an implemented architecture. **Do not run page JavaScript or synchronous script evaluation on the GPUI UI thread**: one busy script in the in-process engine can otherwise stall browser chrome. Hidden/inactive rendering and timer throttling must be deliberately configured; hiding a View alone does not stop every timer.

The [IME guide][U-ime] exposes real composition operations (`SetComposition`, `CommitComposition`, `CancelComposition`), UTF-16 offsets and caret bounds. GPUI still has to supply native marked-text queries, candidate positioning, command mapping, clipboard/focus routing and ownership. `Char` key events are not a complete macOS IME bridge. The official [web-features table][U-features] says screen-reader exposure is **not available**; this is an accessibility gate, not merely an untested capability.

### Single-process security gap: not acceptable for arbitrary tabs

The [official product description][U-home] explicitly says everything runs in the application's process. The [2.0 View API][U-view] says every frame and View shares one VM. Sessions isolate website data, **not exploit address spaces**. A renderer memory-safety vulnerability or blocking execution can affect the host; WebKit ancestry does not inherit WKWebView's WebContent/XPC separation or Apple-maintained update path.

A helper process could keep engine code out of the GPUI host, but Felinaut would then own IPC validation, bounded messaging, input/presentation transport, process teardown, an effective OS sandbox, privileges for network/files and cross-site isolation. One unsandboxed helper hosting every site is not a secure browser architecture. Temporary HOME, storage sessions, JIT-disabled builds, CORS and an App Sandbox entitlement are each **different from renderer/site isolation**. Do not claim any of them closes this gap.

For trusted-local work, register restrictive filesystem/platform handlers, prohibit privileged JS/DOM bindings on remote documents and keep all engine stores/logs inside the temporary home. Explicitly disable universal access from file URLs and never ignore TLS errors. A loopback-only address bar is not a subresource/network firewall.

Important additional [network-control limitation][U-network]: Free does **not** deliver network-filter callbacks. Even the paid HTTP(S) listener omits `file:`, `data:` and synchronous XHR. Do not rely on a registered callback as the sole containment mechanism; do not treat an unavailable callback as successful denial. A fixture with no external resources is a trusted workload, not an adversarial sandbox test.

### Web compatibility is materially below general browser expectations

The [official 2.0 feature table][U-features] is more useful than “modern WebKit” marketing. It reports, among other things:

- JavaScript/ES modules, DOM, flex/grid, fetch/XHR, WebSocket and basic storage support; **Free still runs JS**, without JIT. Native DOM operations can also run with JS disabled.
- **No WebAssembly, WebGL, WebGPU, service workers, SharedArrayBuffer or WebCrypto `subtle`**. Those omissions alone break many otherwise modern websites and packaged web apps.
- No WebRTC/getUserMedia, Web Audio, DRM/EME, Media Source Extensions, passkeys/Web Authentication or general permission APIs.
- Media is limited by edition and format; the feature guide describes VP9/Opus WebM rather than Safari's general media stack. No MSE means a JavaScript HLS/DASH player is not made functional by JavaScript execution.
- No `beforeunload` prompt, file-picker UI for file inputs, printing or screen-reader exposure; several form/control/window APIs are missing or partial.

A login form rendering, a React component mounting or `1 + 1` returning `2` cannot justify “arbitrary websites supported.” The 2023 WebKit baseline is a reason to demand a documented security-backport/update policy, **not proof that every historical WebKit CVE is unpatched**.

### License, edition and packaging decisions

The core is **proprietary**, with WebKit open-source components governed separately. [2.0 architecture][U-arch] describes SDK platform reference sources as **Zlib**; do not carry forward an old AppCore LGPL assertion without auditing the actual package. Review the delivered `NOTICES.md` and all open-source source/relinking obligations before redistribution.

Current [pricing][U-pricing] says Free requires both turnover and investment funding below **$200K**, has a 60 FPS cap, and omits JIT/media. It advertises Indie/Pro per-title licenses with renewal fees for updates. Paid [2026-08 terms][U-terms] include dynamic-linking restrictions, notices/end-user terms, no standalone SDK redistribution, and a **no developer-platform** clause (§4.9) with an exception for wrappers that distribute no SDK and require users' own licenses. They also contain confidentiality provisions. Obtain explicit review for an open-source browser, CI artifacts, screenshots/benchmark publication where relevant, and future extensibility before acquiring/redistributing proprietary materials. Public API research is not license acceptance or permission to redistribute the SDK.

**Edition evidence conflict:** pricing says all paid tiers include media, while the 2.0 [feature-macro guide][U-editions] says media is Pro-or-higher and uses `PLUS` where pricing says Indie; the C View guide also describes a higher floor for network callbacks than some other guides. Resolve with the vendor/exact package **before purchase or capability claims**. Check `UL_HAS(...)`, edition macros, `VERSION.txt` and runtime `ulVersionString` / `ulWebKitVersionString`; do not trust an API declaration alone to establish a working feature. Beta package suffixes are not fully represented by numeric API version macros.

The [linking guide][U-link] requires the engine's runtime libraries (`Ultralight`, `UltralightCore`, **WebCore even if not directly linked**) and resources. AppCore is optional when custom handlers are supplied. The [shipping guide][U-ship] includes ICU data, certificates and optional inspector resources, macOS `@rpath`/bundle paths and explicit storage configuration. Audit architecture slices, deployment target, install names, SDK signatures, hardened runtime/JIT entitlements, Developer ID/notarization and clean-machine startup in separate packaging work. Do not replace system WebKit or weaken signing checks to make co-loading work. Coexistence of this private WebCore/JSC fork with system WKWebView must be proven, not assumed from different filenames.

The renderer's default Session is persistent. [Session documentation][U-sessions] warns that an empty cache path creates a `default` directory relative to the working directory. Always set an explicit private cache root and explicit nonpersistent sessions for experiments; do not let a null session silently choose the persistent default. Separate sessions and backend/profile pairs must never open each other's databases or copy cookies.

**First future deliverable:** after legal/edition approval, obtain and verify only the selected arm64 package, then a C ABI CPU-surface probe with temporary stores, no native page bindings, and bounded worker ownership. Keep the adapter unavailable for untrusted browsing. If general browsing is the requirement, stop until there is a credible sandbox/site-isolation and accessibility plan; a quick texture demo is not that plan.

## Integration acceptance and stop decisions

The existing [`src/page.rs`](../src/page.rs) seam is UI-thread-owned, drains notifications, and deliberately returns unsupported errors for optional operations. Its current runtime-source classification is Cocoa-specific, and generation/cancellation/profile-selection work is still incomplete. This research does not expand that seam or propose faking a `WKWebView` image UUID for other engines.

Before parallel adapter work, agree on ownership of that contract with the parent/core workstream. Engine-specific handles, IPC objects, GPU pointers and profiles remain adapter-owned. Report backend identity from actual linked/runtime artifacts, not user-agent text or a requested engine name. Ultralight's family metadata should acknowledge WebKit derivation while its backend identity remains distinct from Cocoa WebKit.

| Gate                    | Small deliverable / acceptance                                                                                                                    | Stop condition                                                                                            |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| A — Acquisition         | Pin source/package, license, architecture, version/digest, prerequisites and a bounded disk budget; metadata-only first                           | Requires an unapproved huge download/install, unauthenticated artifact, or unresolved license             |
| B — Engine-only page    | Trusted loopback HTML/CSS/JS, committed URL/title, load error, screenshot, close/reopen; explicit runtime identity                                | JS REPL only, silent fallback, no clean teardown, or unsupported capability reported as success           |
| C — GPUI coexistence    | One real page inside a GPUI 0.2.2 content rectangle **with working WKWebView in another tab**, resize/scale/split/hide; window capture            | Separate engine window mistaken for embedding, second owning GUI loop, stale pixels, main-thread stalls   |
| D — Input/accessibility | Native trusted click/type, Cmd-L/Tab/Escape focus return, Japanese/Chinese IME commit/cancel and caret placement, clipboard, VoiceOver            | DOM-synthesized input counted as native, duplicate text, missing content accessibility                    |
| E — State/policy        | Backend/profile isolation, navigation restrictions, TLS failure, download cancel/hidden-page denial, popups, stale callbacks and page replacement | Data leaks, weakened TLS, replayed POST, erased cache on refresh failure, late event resurrects page      |
| F — Security/lifecycle  | Validate actual helper identity/sandbox, cross-site boundaries, renderer crash/restart, root death and helper reaping                             | Servo arm64 unsupported sandbox; Ultralight unisolated hostile content; Ladybird helpers omitted/disabled |
| G — Shipping            | Reproducible complete bundle, notices/security updates, native architecture tests and clean-machine startup                                       | Developer tool success mistaken for distributable/notarized engine                                        |

A first research spike may deliberately stop at B/C with documented unsupported capabilities. **A selectable arbitrary-web backend must pass all gates.** Do not weaken those gates to make every engine appear equally ready. Keep system WKWebView as the default; no silent fallback, no real account use, no migration of live DOM/cookies, and no new engine picker in this task.

### Small executable checks for the future

These are explicit future commands/checks, **not results of an engine build**. They require no invented `felinaut --engine servo` switch: no such adapter switch was added. Run prototype code only after agreeing on its actual interface. Use the browser-control skill when later validating real Felinaut; a JavaScript console check alone is not native UI evidence.

#### A. Metadata-only acquisition check (safe to run before SDK approval)

This queries one small JSON response, validates the exact selected package metadata, and intentionally never follows its SDK URL or prints the temporary signed URL. It contacts Ultralight's public service; it installs/writes nothing. The expected digest is the publisher value observed above, not an independently authenticated checksum.

```sh
python3 - <<'PY'
import json
import urllib.request
url = ('https://ultralig.ht/api/v1/sdk/download?platform=macos-arm64'
       '&version=2.0.0-beta.3&format=json')
with urllib.request.urlopen(url, timeout=15) as response:
    package = json.loads(response.read(65537))
assert package['version'] == '2.0.0-beta.3'
assert package['platform'] == 'macos-arm64'
assert package['filename'] == 'ul-sdk-2.0.0-beta.3-mac-arm64-free.7z'
assert package['sha256'] == (
    '48be6b4788e1f6df8dcdf748aa0c2f67ff0fd84608e32d1e97549124b115feb8')
print('Package metadata matches; SDK NOT downloaded or executed.')
PY
```

A matching response proves neither architecture contents, signature, license eligibility nor successful linking. When a later approved archive exists, verify its bytes against the recorded digest **before extraction**, then inspect Mach-O deployment targets/slices and signature status. Never download unversioned `latest` as the reproducibility pin.

#### B. Bounded, self-contained loopback page fixture

This future fixture serves only generated content, not repository files. It writes no profiles/files and opens no browser. It exits after approximately **120 seconds**, plus socket timeouts. It is a trusted workload, not a network sandbox. In a second terminal/prototype, load the printed literal-loopback URL in an explicitly isolated engine context; do not use a personal browser profile to evaluate experimental engines.

```sh
python3 - <<'PY'
from http.server import BaseHTTPRequestHandler, HTTPServer
import time

PAGE = b'''<!doctype html><meta charset="utf-8">
<title>engine-fixture-loading</title>
<style>body{font:20px system-ui} #box{width:80px;height:40px;background:green}</style>
<h1>Engine fixture</h1><div id="box"></div>
<label>Text <input id="text" autocomplete="off"></label>
<button id="button">Confirm native input</button><output id="result"></output>
<a href="/next">Next</a><a href="/missing">HTTP 404</a>
<script>
window.fixture = {js: 6 * 7, trusted: false, input: '', storageError: null};
try {
  fixture.previous = localStorage.getItem('engine-probe');
} catch (error) { fixture.storageError = String(error); }
document.getElementById('button').addEventListener('click', function(event) {
  fixture.trusted = event.isTrusted;
  fixture.input = document.getElementById('text').value;
  document.getElementById('result').textContent = JSON.stringify(fixture);
});
document.title = 'engine-fixture-ready';
</script>'''

class Handler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(2)

    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path == '/redirect':
            self.send_response(302)
            self.send_header('Location', '/')
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        status, body = (200, PAGE) if self.path == '/' else (
            (200, b'<title>engine-fixture-next</title><p>Next page</p>')
            if self.path == '/next' else (404, b'<title>fixture-404</title>Missing'))
        self.send_response(status)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(body)

with HTTPServer(('127.0.0.1', 0), Handler) as server:
    server.timeout = 0.25
    print('http://127.0.0.1:%d/' % server.server_port, flush=True)
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        server.handle_request()
PY
```

For each actual future engine/prototype, record independent outcomes:

1. Load `/redirect`; committed URL becomes `/`, title becomes `engine-fixture-ready`, green box and controls visibly render. Script/DOM check: `JSON.stringify(window.fixture)` has `js:42`. A screenshot must show page pixels, not just a native chrome window. Timeout each load at 10 seconds; a script result without pixels is **JS-only**, not presentation PASS.
2. Use **native input**, type `probe-123` into Text, click the button, then evaluate the fixture. Require `trusted:true` and `input:"probe-123"`. Dispatching DOM events or setting `input.value` directly does not pass. Repeat with a real IME and navigation mid-composition; this fixture's ordinary text input is not an IME automation harness.
3. In clean context A, require `previous:null` and no storage error; write `localStorage.setItem('engine-probe','A')`, reload and check it reads `A`. Open the **same origin** in distinct context B and require `previous:null`, write `B`, then recheck A remains `A`. If storage is unsupported, record unsupported/FAIL for the storage gate rather than claiming isolation. A persistent-context round trip is a separate test; nonpersistent close/recreate must discard the sentinel.
4. Follow `/next`, back, forward and reload; assert URL/title/history transitions and eventual screenshot. `/missing` is an HTTP 404 **document**, not a transport-error callback. Close the server and separately require a recoverable connection failure; invalid TLS is a separate local HTTPS fixture, never a global ignore-errors flag.
5. Close during an in-flight load/script/frame, create a replacement, and require no old-generation completion to mutate it. Repeat create/close 20 times and record host/helper counts and settled memory. Killing an owned test renderer must leave GPUI chrome responsive or produce an explicit recoverable backend failure, not take down unrelated user processes.

Do not publish account URLs, profiles, cookies, private logs or whole-desktop captures. A later UI PR must include a real sanitized screenshot/video and identify any still-pending native input/accessibility/security gates.

### Next work items, in order

1. **Core/macOS owner:** agree on an experimental probe boundary, page/document generation semantics and presentation ownership before adapter implementation or shared architecture changes.
2. **Servo owner:** ask upstream about the inspected arm64 sandbox exclusions; choose a maintained pin; budget a separate build location/prepared products. First functional spike uses the Rust software context and trusted loopback fixture. Keep full browsing blocked pending sandbox resolution.
3. **Ladybird owner:** confirm the current UI/Hub protocol is an acceptable downstream boundary and whether public IOSurface presentation can remain supported without Qt/private remote layers. Establish a build/artifact and disk budget before attempting compilation. Do not mistake LibJS or an older AppKit port for the solution.
4. **Ultralight owner/legal:** resolve Free/commercial terms, edition discrepancies, security-update policy and browser-use implications before SDK acquisition. If approved, prototype only the C CPU-surface path. General-web selection stays blocked by isolation/accessibility, even if the prototype is quick.
5. Only after native evidence, propose separately reviewed adapter dependencies/implementation and per-tab availability handling. No calendar, lower-memory claim, compatibility promise or release commitment follows from this research.

## Validation of this document

This task checked sources/host metadata and performed the following documentation-only checks on 2026-11-12:

- **PASS:** local file links and Markdown reference definitions resolve.
- **PASS:** both shell snippets pass `sh -n`; both embedded Python programs parse successfully.
- **PASS:** the metadata-only snippet matches the pinned Ultralight package metadata without downloading the SDK.
- **PASS:** a briefly launched fixture passed HTTP checks for the page, redirect, next page, missing path and refusal to serve a repository-file path. The owned fixture process was then terminated/reaped. No browser or JavaScript engine participated; the 120-second natural-expiry path was not waited out.
- **PASS:** `git diff --check` for tracked working-tree changes; the new document was separately checked for trailing whitespace/conflict markers. Working-tree status showed this document as the only additional authored path relative to the initial status; pre-existing modifications/untracked paths were left alone.

The existing Cocoa comparison and native browser tests were **not re-run**. Cargo formatting, Clippy, Rust tests and distribution/Python project suites were not run for this new Markdown-only file. All candidate-engine build, linkage, UI, sandbox and packaging boxes remain **not built / not run**. Published minimum-OS prerequisites and architecture labels are not substitutes for inspecting actual Mach-O deployment targets or running on older macOS/Intel hardware; no such compatibility result is claimed.

## Primary-source references

All online references were consulted on **2026-11-12**. Commit URLs preserve inspected source; versioned vendor docs remain mutable.

[S-pin]: https://github.com/servo/servo/commit/772a8ec259a71a13600409db861e1a9ef848db89
[S-release]: https://github.com/servo/servo/releases/tag/v0.8.0
[S-book]: https://github.com/servo/book/tree/a0b844736cee945ee7832fbad02bb7c2ff60d69c
[S-lib]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/components/servo/lib.rs
[S-example]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/components/servo/examples/winit_minimal.rs
[S-servo]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/components/servo/servo.rs
[S-view]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/components/servo/webview.rs
[S-capi]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/ffi/capi/README.md
[S-cargo-c]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/ffi/capi/Cargo.toml
[S-cbuilder]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/ffi/capi/lib.rs
[S-cview]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/ffi/capi/webview.rs
[S-ccontext]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/ffi/capi/rendering_context.rs
[S-copts]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/ffi/capi/options.rs
[S-ctests]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/tests/capi/README.md
[S-sandbox]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/components/constellation/sandboxing.rs
[S-cargo]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/components/servo/Cargo.toml
[S-toolchain]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/rust-toolchain.toml
[S-brew]: https://github.com/servo/servo/blob/772a8ec259a71a13600409db861e1a9ef848db89/python/servo/platform/Brewfile
[S-mac]: https://github.com/servo/book/blob/a0b844736cee945ee7832fbad02bb7c2ff60d69c/src/building/macos.md
[S-embed]: https://github.com/servo/book/blob/a0b844736cee945ee7832fbad02bb7c2ff60d69c/src/embedding/overview.md
[S-lts]: https://github.com/servo/book/blob/a0b844736cee945ee7832fbad02bb7c2ff60d69c/src/embedding/lts-release.md
[L-pin]: https://github.com/LadybirdBrowser/ladybird/commit/093b084d9b575d4509a2e676e5cf021ffbc22675
[L-build]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/Documentation/BuildInstructionsLadybird.md
[L-remote]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/UI/Qt/RemoteView.h
[L-hub]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/Services/Hub/HubServer.ipc
[L-present]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/UI/Qt/WebContentViewMac.mm
[L-process]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/Documentation/ProcessArchitecture.md
[L-release-ci]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/.github/workflows/build-release.yml
[L-license]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/LICENSE
[L-ax]: https://github.com/LadybirdBrowser/ladybird/blob/093b084d9b575d4509a2e676e5cf021ffbc22675/Documentation/ScreenReaderSupport.md
[L-site]: https://ladybird.org/
[U-home]: https://ultralig.ht/
[U-download]: https://ultralig.ht/download/
[U-releases]: https://ultralig.ht/api/v1/sdk/releases
[U-prereq]: https://ultralig.ht/docs/2.0/installing-prerequisites
[U-c]: https://ultralig.ht/api/c/2_0_0/
[U-arch]: https://ultralig.ht/docs/2.0/architecture
[U-crender]: https://ultralig.ht/docs/2.0/c-renderer
[U-cview]: https://ultralig.ht/docs/2.0/c-views
[U-view]: https://ultralig.ht/api/cpp/2_0_0/classultralight_1_1_view.html
[U-loop]: https://ultralig.ht/docs/2.0/updating-and-rendering
[U-ime]: https://ultralig.ht/docs/2.0/input-method-editors
[U-features]: https://ultralig.ht/docs/2.0/supported-web-features
[U-network]: https://ultralig.ht/docs/2.0/downloads-and-network-control
[U-pricing]: https://ultralig.ht/pricing/
[U-terms]: https://ultralig.ht/terms/2026-08
[U-editions]: https://ultralig.ht/docs/2.0/editions-and-feature-macros
[U-link]: https://ultralig.ht/docs/2.0/linking-to-the-library
[U-ship]: https://ultralig.ht/docs/2.0/shipping-your-app
[U-sessions]: https://ultralig.ht/docs/2.0/sessions-and-site-data
