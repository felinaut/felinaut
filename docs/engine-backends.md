# Portable engine-backend contract

**Status:** architecture constraints for the `multiplatform` development branch. These are target requirements, not proof of supported alternate engines or feature parity. The first native page/event/capability seam is implemented in `src/page.rs` and `src/page/macos.rs`; the [macOS status report](macos-backend-status.md) distinguishes its validated scope from still-unimplemented generation, cancellation and profile-selection work. The current application still uses native macOS WebKit. Read the [porting plan](porting-plan.md) and [control inventory](gpui-controls.md) first.

## Keep three decisions separate

1. **Host platform:** windowing, clipboard, paths, dialogs, file reveal/open, appearance/accessibility preferences, menus, and automation transport.
2. **Engine family:** WebKit, Chromium/Blink, Gecko, or a future candidate.
3. **Embedding backend:** the specific API and runtime that presents pages: WKWebView, WPE, WebView2, CEF, or a separately evaluated implementation.

An OS default is a policy choice, not a permanent one-to-one mapping between engine and OS. WKWebView and WPE are different integrations of the same engine family; WebView2 and CEF are different integrations of Chromium. Neither Wry nor GPUI is a web engine.

Do not design a shared interface named or typed specifically for WebKit. Do not assume an engine always supplies a native child window: some integrations may present native views, others engine-owned pixel/GPU buffers. Engine-specific objects, raw handles, ownership and synchronization remain inside reviewed adapters.

## Candidate backends, not commitments

| Candidate               | Scope of investigation                                                | Important constraint                                                                                                                                                                                                                    |
| ----------------------- | --------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| WKWebView through Wry   | Preserve the shipping macOS implementation.                           | System engine; keep AppKit and WebKit objects out of shared models.                                                                                                                                                                     |
| WPE WebKit              | GTK-free Linux investigation.                                         | Requires an integrated rendering/input bridge; not a supported macOS/Windows option in the current plan.                                                                                                                                |
| WebView2 through Wry    | Windows investigation.                                                | Separate runtime/deployment and browser API behavior; Chromium rather than WebKit.                                                                                                                                                      |
| CEF                     | Possible separately scoped Chromium integration.                      | A bundled runtime with its own process, packaging, sandbox and security-update obligations; not an automatic Wry backend switch.                                                                                                        |
| Gecko, Firefox's engine | Possible future desktop feasibility study.                            | Firefox running on a desktop OS does not establish a supported embedding API for this GPUI app. Mozilla's documented GeckoView product targets Android; we have not identified and validated a drop-in desktop equivalent for Felinaut. |
| Other engine families   | Consider only with a concrete use case and maintained embedding path. | Evaluate actual platform/feature/security support before adding dependencies, UI choices or release claims.                                                                                                                             |

GeckoView is not a desktop replacement for `WKWebView`. Do not promise Firefox extension compatibility, Firefox Sync, password integration, DRM, or Firefox's entire UI/security policy merely because an adapter might use Gecko. The same distinction applies to embedding Chromium versus shipping Chrome.

## Shared contract: requirements, not a giant abstraction

Implement only operations demanded by existing behavior and proven prototypes. A small adapter plus explicit optional capabilities is preferable to either scattered OS checks or a universal interface containing every engine's API.

- **Identity and availability:** distinguish a stable backend identifier from engine-family/version metadata. Report OS support, missing runtime and initialization failure explicitly. Do not infer capability from the OS, user-agent string, or a successful page load.
- **Page lifetime:** create, show/hide, resize/scale, focus and close a page. Define which thread owns native resources, how tasks are scheduled there, and when engine subprocesses/resources terminate. Native notifications enqueue events; they must not re-enter GPUI rendering. Teardown invalidates delivery and unregisters callbacks before freeing their targets, safely retains any in-flight native callback state, resolves pending decisions, and releases resources on their owning thread. Presented buffers remain alive until the consumer is finished, not merely until a tab closes.
- **Navigation:** load/reload, back/forward availability, committed URL/title, load completion/failure and popup/new-tab requests. Native error codes stay in adapters or typed diagnostic details; shared browser decisions use normalized meanings and must not invent states the engine cannot establish.
- **Event/request ownership:** identify page-lifetime, document/navigation-lifetime and independent request/download events separately. Reject stale document completions after navigation; do not discard a valid download's progress just because its initiating document changed. Define cancellation/closure rules for each scope. Bound callback payloads, retries, execution time and queued work; never block the UI thread waiting for another engine process.
- **Presentation and input:** backend-owned integration handles native child views or buffer presentation without exposing arbitrary native pointers to browser state. Route keyboard, IME, clipboard, pointer and scroll to exactly one focused domain, preserving scale/coordinate conventions. Accessibility is part of the integration, not a texture-rendering afterthought.
- **Script and message boundary:** preserve validated, bounded messages and asynchronous script completion. Do not expose filesystem access, a general native-command interface, arbitrary host objects or authenticated automation tokens to page content. Define permitted documents/frames for each bridge and audit injection timing/frame coverage rather than assuming Wry semantics apply everywhere. Use trusted engine-provided origin/frame/document attribution where authorization is required, revalidate it after navigation/redirects, and reject unauthorized or unattributable privileged requests. A payload's claimed origin or the tab's displayed URL is not proof of a sender's authority.
- **Optional capabilities:** zoom, trusted automation input, snapshots, media controls, download progress/cancellation, permission prompts and certificate exception handling must report supported/unsupported states accurately. Unsupported operations return explicit errors; a no-op is not success. Do not hide useful platform features merely to force every adapter to a lowest-common-denominator interface.
- **Downloads and decisions:** use application-owned IDs and models, not `WKDownload`/COM/WPE objects in the sidebar. Preserve validated file naming, cancellation, unknown totals, terminal states and hidden-page download denial. Preserve navigation/TLS restrictions; lack of an exception API must not become unconditional certificate acceptance.
- **Synchronous and deferred decisions:** distinguish notifications from native navigation, permission, TLS and download callbacks that require an answer. Use validated adapter-local policy for synchronous answers; do not wait for GPUI or allow first and authorize later. Native deferrals must be retained safely, have deadlines and cancellation/teardown handling, and fail closed when authorization cannot complete.
- **State ownership:** browser tabs/pins/history/UI preferences can remain engine-neutral where their meaning is genuinely portable. Live DOM, JavaScript heaps, back/forward histories and in-flight requests cannot be transferred between engines by serializing a tab URL.

The existing [`src/engine.rs`](../src/engine.rs) resolves address-bar text; despite its name it is **not** a multi-engine backend abstraction. Do not overload it or rename it as an unrelated cleanup. Agree on the new module boundary before platform agents implement against it.

## Profiles and engine selection

The first safe policy remains one backend per browser profile, selected at startup. This is an initial product constraint, not a design assumption that the adapter API can support only one backend implementation. A [small experimental backend picker](web-engines.md#proposed-experimental-backend-picker) can support same-machine comparisons once two adapters are validated on that OS; selection UI must report real runtime availability and must not imply WPE macOS support before a prototype proves it.

- Separate website-data directories and native contexts by both backend and profile, and never open one engine's databases with another. No silent cookie export/import or cross-engine authentication sharing. Require exclusive writable-profile access or an explicitly supported engine concurrency model across processes; finish shutdown before reusing a context/directory.
- Preserve existing macOS data paths and native-backend defaults. Define versioned, backward-compatible settings only when selection is implemented; validate unknown IDs, unavailable backends, malformed/older files, rollback and migrations.
- Resolve unsupported configurations explicitly. No invisible engine fallback, duplicate POST submission, or retry through a weaker TLS/authentication path.
- A future per-tab “reopen using” operation is an explicit reload, not a live migration. It must handle unsaved work and active downloads and use the selected profile's isolated website data.
- Hidden live-folder pages use the chosen backend/profile and its own authenticated sessions. They must not silently fall back to another profile or gain visible-page privileges.

## Contract tests and adding a backend

Shared, engine-independent tests must execute real state transitions against controllable fake adapters: events arriving late/out of order, creation failure, missing runtime, unsupported operations, cancellation, closure, profile separation, pending-decision timeout/teardown, navigation during a continuing download, and persistence compatibility. Fakes test browser policy; they do not prove a native implementation or sandbox works.

Each real adapter needs opt-in, loopback-only conformance tests for navigation/errors, message validation, focus/input, storage, download behavior and supported capabilities. Test engine crash/restart and shutdown. Report skipped capabilities honestly, with native screenshots/videos for actual UI changes.

Before accepting a new engine family, require:

1. A maintained, usable embedding API for each claimed OS and a prototype inside the actual GPUI window.
2. Native keyboard/IME/accessibility behavior, permissions and process isolation appropriate to untrusted websites.
3. Target-specific dependencies and reproducible packaging of all runtime libraries/resources/helpers, with architecture, licensing, signing and update requirements reviewed.
4. A security-update owner/process and tested failure/rollback behavior. Pinning an engine for reproducibility must not freeze security updates indefinitely.
5. Measured compatibility/performance on representative local workloads, plus documented differences. Do not promise complete browser compatibility, extension support, or lower memory solely from the engine's name.

No generic engine-picker UI, bundled engine download, dependency upgrade, or native adapter is implemented by this document.

## References

- [Mozilla GeckoView documentation: Android embedding](https://firefox-source-docs.mozilla.org/mobile/android/geckoview/index.html).
- [Wry 0.57.1 platform integrations](https://docs.rs/wry/0.57.1/wry/).
- [WPE platform integration](https://wpewebkit.org/about/architecture.html) and [supported-platform FAQ](https://wpewebkit.org/about/faq.html).
- [CEF project](https://github.com/chromiumembedded/cef).
