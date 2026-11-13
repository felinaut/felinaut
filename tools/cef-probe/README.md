# Native CEF/GPUI lifecycle probe

Developer-only macOS arm64 investigation, separate from the browser's Cargo graph. The default build is SDK-free; the explicit `runtime` feature now loads a pinned CEF distribution and exercises a trusted loopback document inside an AppKit child view hosted by GPUI. **This is not the browser adapter or arbitrary-site support.** `engine-cef` in the browser remains explicitly unsupported. Native functional checks and the window-only visual check pass on the host below. The recorded capture contains the actual GPUI chrome and CEF document, not a mockup.

## Build and test

From the repository root, with full Xcode/Metal and a macOS desktop session:

```sh
cargo fmt --manifest-path tools/cef-probe/Cargo.toml --all --check
cargo clippy --locked --manifest-path tools/cef-probe/Cargo.toml --target-dir target --all-targets -- -D warnings
cargo test --locked --manifest-path tools/cef-probe/Cargo.toml --target-dir target
cargo build --locked --manifest-path tools/cef-probe/Cargo.toml --target-dir target

# Separate fresh processes; the first two create no window.
target/debug/felinaut-cef-probe --check-pump
target/debug/felinaut-cef-probe --check-existing-app
target/debug/felinaut-cef-probe --run-lifecycle
```

Without an explicit mode the executable returns usage/error, not a skipped native success. Lifecycle mode opens its own labelled GPUI window, checks state, and quits after roughly 1–2 seconds; a 12-second watchdog bounds the native run. Neither mode opens websites, loads a user browser profile, installs software, alters system protections or changes the installed Felinaut app. Native capture, if requested separately, must select only this process's window.

## Boundary under test

GPUI 0.2.2 registers `GPUIApplication` during its load-time constructor, but its normal explicit singleton acquisition occurs in `MacPlatform::run`. At the start of this executable, a small Objective-C++ shim:

1. Checks the actual `NSApp` global without calling `sharedApplication`.
2. Refuses an existing singleton, missing GPUI class, changed inherited `platform` ivar ABI, or duplicate probe class.
3. Registers a **new subclass** of GPUI's class. It does not modify GPUI's superclass methods, shadow the inherited ivar, mutate an existing object's class, or replace the delegate.
4. Adds scoped send-event state and a deferred termination barrier, then creates that subclass's singleton before `Application::new()`.
5. Allows GPUI to install its original delegate and initialize its inherited platform state normally.

The native checks require actual class ancestry because AppKit installs an `NSKVONotifying_…` subclass after launch on this host. Exact `isa` equality would reject the valid application. Inherited state is only observed, never written/dereferenced by the probe. Scoped dispatch restores the previous state through nested calls and an intentional Objective-C exception using `@try/@finally`; a native application-defined event also traverses superclass dispatch.

GPUI's quit request is held at the subclass's `terminate:` barrier while the host remains alive. Explicit release schedules ordinary termination asynchronously, outside GPUI borrows. The original GPUI termination callback must still run. This is a lifecycle proof, not yet CEF's asynchronous browser-close state machine.

The hook relies on a **private, version-pinned GPUI class/ivar**. Runtime checks reduce accidental ABI misuse but do not turn it into a stable GPUI API. Correct Objective-C method encodings use the SDK's `BOOL` ABI. No Rust panic crosses a native IMP; Objective-C dispatch/exception scope remains in Objective-C++.

## Recorded evidence

On **12 November 2026**, macOS **27.0 (26A428), arm64**, Xcode **27.0 (27A266a)**, baseline `f5c763c` plus this probe:

- Tool formatting and Clippy with warnings denied: passed.
- **3 behavioral unit tests**: explicit mode/argument validation, native error propagation, off-main-thread refusal.
- Fresh-process conflicting-singleton check: **PASS**, no replacement.
- Native GPUI lifecycle check: **PASS**, inherited state/delegate, nested/exception send-event restoration, termination barrier and original GPUI shutdown callback.
- Window-only capture reviewed; no personal browser data or web content was involved.

![Actual GPUI-only lifecycle-probe window, explicitly labelled as having no web engine loaded.](../../docs/screenshots/macos-cef-lifecycle-probe.png)

## External-pump scheduling checkpoint

`--check-pump` runs bounded native tests before creating an application singleton. The scheduler accepts CEF-style deadline notifications from any thread, serializes/coalesces them, and invokes a process-lifetime callback only from deferred main-run-loop work. The latest notification replaces the previous deadline; nonpositive delays are prompt/asynchronous and positive delays saturate at 24 hours. Common and event-tracking modes are supported without a periodic idle timer.

For the pinned CEF runtime, `pump::start_cef` explicitly opts into the reference embedder's liveness policy: positive waits cap at 33 ms, and each callback arms a 33 ms fallback only if no newer deadline is pending. Notification-only scheduling stalled actual CEF page loading on this host; this separate mode follows [CEF's pinned reference pump](https://github.com/chromiumembedded/cef/blob/8ea0c73b5dbed1389c8dafb911a98c41246bb8cb/tests/shared/browser/main_message_loop_external_pump.cc#L21-L83). Starting still waits for the first notification. While active, CEF mode wakes roughly 30 times per second even without notifications; callback work is additional, and no power/performance improvement is claimed. Stop cancels the fallback, and restarting generic mode restores no-idle-poll behavior. Extended native checks cover these transitions, capped/fallback deadlines, nested scheduling precedence and non-reentrancy.

The callback runs outside the scheduler mutex and outside GPUI rendering; nested loops cannot recursively invoke it. Stop invalidates timers, sources, callback ownership and stale queued generations before engine shutdown. A stopped callback may finish, so its owner must wait for that call to return before destroying state or unloading an engine. Old producers must be quiesced before a restart because the upstream scheduling callback carries no producer token.

The integrated native check passed on this host: coalescing/replacement, immediate/positive/extreme delays, worker wakeup, tracking mode, nested-loop non-reentrancy, stop during callback, stale work and restart. Development validation also passed 10 fresh native runs and AddressSanitizer/UndefinedBehaviorSanitizer. This proves scheduler behavior, **not actual CEF initialization or page rendering**. The future Rust trampoline must catch panics; no callback may unwind/throw through the native ABI.

## Pinned SDK preparation

Acquisition is explicit and separate from Cargo/the default browser. The matching published binding is `cef 156.4.0+156.0.26`; its runtime is CEF `156.0.26+g8ea0c73+chromium-156.0.8078.88`, macOS arm64 minimal distribution.

```sh
python3 -m unittest discover -s tools/cef-probe -p 'test_*.py'
python3 tools/cef-probe/prepare_runtime.py --download
python3 tools/cef-probe/prepare_runtime.py --verify
```

The downloader prints the absolute `CEF_PATH` for later runtime builds. No argument performs no acquisition; `--verify` is offline. Output stays under ignored `target/cef-runtime/`. The script bounds bytes/time/free space, checks publisher size/SHA-1 and records SHA-256, rejects unsafe tar paths/link traversal/special files/duplicates, preserves legitimate contained framework symlinks, and atomically publishes a private SDK directory. Existing trees require the preparation marker and full manifest/hash verification; unknown destinations are not overwritten. Exact `cef_version.h` and arm64 Mach-O headers are checked. The upstream downloader's version check permits older builds; this preparer instead pins the exact version.

Acquired/verified on this host: **134,979,837 compressed bytes**, **353,085,527 extracted file bytes**, SHA-256 **`6d7936cdd55ef30cf351a6358b2d922becefe87b25a8a8870a2b9ff5af2d39f6`**. Publisher SHA-1 **`ca4fafe7a9c52d9f28200bb8713971a9662b891c`** matched. These checks record integrity, not independent publisher authentication or an audit of binary contents. **32 offline behavioral tests passed**, including hostile archives and destination preservation.

Read-only signature inspection found linker-generated ad-hoc signatures without Team IDs. Sandbox-dylib strict verification passed; framework-bundle strict verification failed with “code has no resources but signature indicates they must be present.” No binaries/signatures/xattrs or system protections were changed. This is not a signed distribution package or permission to bypass a runtime failure. CEF has not been loaded or launched by the acquisition step.

## Isolated helper and developer bundle

The standalone workspace includes `felinaut-cef-probe-helper`, with no GPUI or Wry dependency. Its pinned CEF/sys pair uses sandbox support. It validates its child-process arguments and bundle layout, initializes the CEF sandbox before dynamically loading the framework, negotiates the API hash and executes the child role. Sandbox-bypass switches are refused; initialization requests alone do not establish effective confinement.

```sh
# Set CEF_PATH to the exact absolute path printed by prepare_runtime.py.
cargo build --locked --manifest-path tools/cef-probe/Cargo.toml --target-dir target -p felinaut-cef-probe-helper
python3 tools/cef-probe/bundle_probe.py --help
```

`bundle_probe.py` performs explicit, offline assembly only beneath ignored `target/`, refusing existing destinations and redirected inputs. It verifies the prepared SDK, thin arm64 executable headers, copied hashes, framework links, all five CEF helper layouts and bundled license/credits files. It preserves framework bytes/signatures and performs no signing, installation, security changes or launch. Preparation/bundling behavioral tests cover invalid and hostile inputs as well as success; helper tests cover role arguments and sandbox-bypass rejection.

## Owned-helper observation

Every native run now integrates `helper_observer.py`. It accepts bounded native helper-start/post-sandbox records, verifies only those PIDs using `libproc` (exact executable, parent, UID and start time), registers `kqueue` exit notifications and queries `sandbox_check` only after sandbox initialization. PID reuse, API errors, unmatched launch counts, malformed records and surviving helpers remain explicit gaps. It neither scans the process table, signals helpers nor deletes profiles. **41 offline observer tests pass**, complemented by the native evidence below. Helper records are emitted as one bounded OS write; a concurrent-process unit test checks record integrity.

## Coexistence transition model

`src/coexistence_policy.rs` and `fixtures/coexistence.html` define the native experiment's fixed sequence: both engines ready, same-origin cookie/localStorage separation, native input WebKit → CEF → WebKit, retained CEF input and both viewports resized to 600 CSS pixels. The pure model rejects invalid, untrusted, wrong-engine and out-of-order reports, tolerates accepted duplicates without advancing, and issues each action once. **26 behavioral unit tests pass**, including latest-width tracking for repeated resize reports and failure on viewport regression during the completion grace period. The native implementation below now passes; a browser engine picker remains unimplemented.

## Same-window system WebKit + CEF probe

**Native functional and window-only visual checks passed on 13 November 2026**, on the macOS 27 arm64/Xcode 27 host documented here. This is a standalone developer fixture, not two separately launched browsers and not yet the main Felinaut adapter.

```sh
# Uses the same explicitly prepared CEF SDK; never acquires an SDK here.
python3 tools/cef-probe/run_probe.py --coexistence              # plan only
python3 tools/cef-probe/run_probe.py --run --build --coexistence
python3 tools/cef-probe/run_probe.py --run --build --coexistence --capture
```

The runner builds this tool with the optional **`coexistence`** feature, which includes `runtime` and the existing project baseline **Wry 0.57.1**. It invokes `--run-coexistence`; a binary built without the feature refuses that mode explicitly, before opening a profile. The default tool remains SDK-free; `runtime` alone does not include Wry. No browser-root manifest, lockfile, engine gate, installed application or user profile is changed. Wry's non-macOS dependency metadata appears in this standalone lockfile, not as Linux/Windows validation or a new runtime dependency on those platforms.

`src/coexistence_webkit.rs` creates an initially hidden/unfocused WKWebView child in the same GPUI window as CEF, with an explicitly nonpersistent store. The live `WKWebView` class and `stopLoading` implementation must resolve through `dladdr` to system WebKit, not a user agent or fixture label. The native check reports `/System/Library/Frameworks/WebKit.framework/Versions/A/WebKit`. CEF retains its own in-memory request context. Both engines use the runner's same literal-loopback origin; only `/cef` and `/webkit` serve the coexistence fixture.

The bounded native sequence verifies:

1. Both real documents load and run JavaScript in one GPUI host window.
2. The same cookie name/root path and localStorage key initially have no value, then remain independently assigned to each engine after both write them.
3. Scoped AppKit native clicks/key events type `Hello from Felinaut` into WebKit, then CEF, then return to WebKit and append ` Meow`. Both buttons report trusted pointer events. CEF's original input and both engines' storage values remain intact. This proves the view-scoped native input/focus sequence, **not full hardware-event dispatch, IME or accessibility**.
4. Resizing the window to 1200×760 produces two 600-CSS-pixel viewports. Repeated/nonfinal resize reports cannot satisfy the gate from historical evidence; a regression after completion is armed fails the run.
5. WebKit callbacks are disabled, loading stops and its native child is removed/dropped on the main thread before CEF close/shutdown and GPUI termination. Layout callbacks skip teardown. This is not a claim that all system-managed WebKit subprocesses immediately terminate.
6. The unchanged CEF canary, helper policy, notification-broker exclusion and independent helper-exit audit also pass. Coexistence has its own ordered completion markers; single-engine markers cannot substitute for them.

The final fresh `--run --build --coexistence --capture` equivalent passed with root exit 0, every native marker and `helper_audit_passed: true`. The reviewed window-only capture contains only generated test content:

![Actual CEF and system WKWebView documents side by side in one GPUI window, after native input handoff and resize.](../../docs/screenshots/macos-webkit-cef-coexistence.png)

Validation passed: tool/default/coexistence formatting and Clippy with warnings denied; **43 coexistence-enabled host tests** (26 sequence, 7 WebKit adapter, 7 runtime-policy, 3 lifecycle), **8 helper tests**, **157 Python tests** (32 preparation, 21 bundle, 41 observer, 63 runner), the SDK-free native lifecycle/pump/negative-singleton checks, the original single-engine native runner and the coexistence native/capture runs. No test result from this Mac establishes another OS or macOS version.

Remaining: production CEF page/event/capability adapter, generation/cancellation and permission handling, normal chrome/content keyboard routing, IME/accessibility, media/downloads, renderer crash/recovery, durable profiles/real Keychain, production packaging/security updates and per-tab engine selection/reload policy. The browser's `engine-cef` gate remains blocked. The system/upstream Cocoa WebKit process-exclusivity constraint is unchanged: this probe pairs **system WebKit with Chromium/CEF**, not two Cocoa WebKit builds.

## Shared PageBackend checkpoint

`src/page/cef.rs` at the repository root now implements the **same `PageBackend` API source as the browser**, compiled by this opt-in harness. Native CEF navigation/history/reload, committed URL/title events, loading state, logical bounds, visibility and focus run through an erased `Rc<dyn PageBackend>`. Native load-start callbacks invalidate script scopes before the UI drain; queues and text payloads are bounded, overflow fails closed, and close rejects further page operations. Native parents/browser handles remain retained by the runtime through asynchronous close, and the adapter is dropped before engine shutdown.

On 13 November 2026 the real single-engine run passed navigation to a second fixture document, back, forward, same-URL reload, rejected privileged URL without document invalidation, hide/show, parent/page focus, and document-scope invalidation. History restoration reports CEF HTTP status 0, accepted only in the restoration phases; fresh network loads still require HTTP 200. Existing native input/resize/capture and helper policy/canary/exit checks also passed. The same-window WebKit/CEF regression passed again. Validation: **49 library tests, 41 binary tests, 157 Python tests**, all-target Clippy and the root browser's Clippy/unit tests.

![Actual running CEF shared-contract harness after native input and resize, before navigation checks.](../../docs/screenshots/macos-cef-page-contract.png)

This is an initial adapter exercised in a standalone runtime, **not root-browser integration or an enabled picker**. `evaluate` currently rejects dispatch explicitly; browser-side renderer-result transport is next. Optional capabilities remain false and fail explicitly. The harness still owns one native CEF container, uses disposable profiles/mock Keychain and restricts requests to its fixture origin. Production multi-page ownership, profiles, permissions, errors/recovery, complete input/IME/accessibility and packaging remain gates. No browser-root CEF dependency or feature-gate relaxation is included.

## Renderer evaluation transport

The helper now implements a native-only, main-frame evaluation transport in `helper/src/evaluation.rs`, with the dependency-free wire contract in `src/evaluation_protocol.rs`. Bindings refer to retained engine contexts and actual native frame/browser identities, not URLs or page JSON. Source/results are limited to 1 MiB, errors to 4 KiB, and retained contexts to 64. Context release removes V8 references. No JavaScript global, query handler, filesystem or host-command API is installed.

The helper captures the context's JSON serializer before page scripts run, returns explicit evaluation/serialization errors and rejects duplicate request IDs. Evaluation and serialization cannot be interrupted by this CEF API; deadlines cancel delivery, not execution or side effects. **20 helper unit tests and all-target Clippy pass.** The ordinary native input/resize/shutdown probe also passes with this helper, preserving the canary and helper-exit audit. Browser-side result delivery and live evaluation conformance are a separate checkpoint; these unit tests do not prove that integration.

## Real CEF runtime probe

After explicit SDK preparation:

```sh
python3 tools/cef-probe/run_probe.py                     # plan only; no launch/build
python3 tools/cef-probe/run_probe.py --run --build       # bounded builds and native checks
python3 tools/cef-probe/run_probe.py --run --capture     # additionally require a window-only screenshot
```

`--build` prevalidates the exact prepared SDK and passes its absolute `CEF_PATH` to each bounded Cargo invocation. It builds the runtime-enabled host and the separate helper. There is no SDK acquisition in the runner. The browser's own Cargo manifest/lockfile and engine gates are unchanged.

The runner creates a new private bundle beneath `target/cef-probe-run-*`, serves only its self-contained loopback fixture with restrictive CSP, and launches with a fresh marked temporary HOME. The native executable rejects non-loopback HTTP URLs, credentials and unmarked/non-private homes. Native launch inherits only locale/text-encoding values, not shell credentials, proxy settings, loader overrides or arbitrary engine switches. Navigation and resource policies restrict the fixture origin, popups/downloads are refused, and redirect destinations are checked. This is not evidence that Chromium makes no background network requests.

### Native boundary and test sequence

- Uses the exact SDK's Objective-C `CefAppProtocol`, with protocol conformance checked at runtime. A small Objective-C translation unit avoids importing third-party C++ templates into the application bridge.
- Initializes CEF before GPUI's run loop, with `no_sandbox = 0`, the external message pump and explicit bundle/helper/cache paths. Helpers initialize the sandbox before framework loading. Initialization alone is not treated as confinement evidence: the runner requires the scoped policy, renderer-syscall and helper-exit checks below.
- Asynchronously creates an Alloy-style native child in a retained container, with a separate in-memory request context. CEF callbacks enqueue bounded events only; they never call GPUI directly.
- Waits for document load/JavaScript, confirms editable-field focus, types the fixture phrase through AppKit key-down/key-up pairs, and clicks its HTML button. The fixture verifies the actual value and a trusted pointer event. Keyboard injection is intentionally limited to US-layout ASCII alphabet/space—not Unicode/IME coverage.
- Resizes the GPUI/native window and requires the document to report the new 1000-pixel CSS viewport width.
- Holds application quit, dispatches browser close exactly once (including quit before asynchronous creation completes), removes only the CEF container after `DoClose`, awaits `OnBeforeClose`, cancels pumping and calls `cef::shutdown` on the main native loop before releasing GPUI termination.
- A 32-second fixture deadline, 45-second host watchdog and 60-second runner budget bound failure paths. Runner success requires all ordered markers, root exit 0 and a passing helper audit, including all observed helpers and at least renderer/GPU/utility roles. Capture failure remains a failed capture run, never a skipped success.

### Disposable Keychain policy

**This trusted-fixture-only probe enables Chromium's `use-mock-keychain` test switch.** Chromium's fake Keychain uses a public test key; it is not secure persistent secret storage and must never be copied into a real browser profile. The private test HOME has no login Keychain. A sampled earlier shutdown hung in `SecItemAdd → defaultKeychainUI → AuthorizationCopyRights`, with other Keychain tasks contending, until CEF's teardown watchdog fired. The scoped test switch prevents those authorization operations; real Keychain integration remains a production gate.

Pinned source: [test switch](https://github.com/chromium/chromium/blob/156.0.8078.88/components/os_crypt/common/os_crypt_switches.h) and [key provider](https://github.com/chromium/chromium/blob/156.0.8078.88/components/os_crypt/async/browser/keychain_key_provider.mm). No real login Keychain, signature, xattr, TLS protection or sandbox setting was modified to obtain the passing run.

### Scoped sandbox and helper-exit audit

On **13 November 2026**, native runs on this macOS 27 arm64 host passed the following checks with the pinned runtime:

- Browser child-launch counts reconcile with distinct helper-start records. Only announced, positively identified helpers are queried; the observer does not search other processes. Both renderer processes, GPU, network and storage utilities passed. An earlier passing run additionally observed sandboxed unzip utilities; counts are not hardcoded.
- At post-initialization sampling, each helper reports Seatbelt presence, allows the framework-file read policy control and denies the generated canary's data-read policy control. Runtime symbol/query errors are failures, never interpreted as denials.
- Each renderer starts a native worker after WebKit initialization and attempts `open(O_RDONLY | O_NOFOLLOW | O_NONBLOCK)` on the fixed generated canary outside the app/profile. **Both renderer opens returned `EPERM`.** The controller can read the same regular file before/after, and its content/identity must remain unchanged. No canary descriptor or sandbox extension is supplied to helpers. This is a native test callback, not a page-accessible filesystem bridge.
- Every observed helper exit is independently confirmed using a live-registered `kqueue NOTE_EXIT` or protected process-start identity. Two final fresh runs each observed five helpers, reconciled all launches, received exit evidence for every helper and returned root exit 0.

The private `report.json` contains raw per-helper query outcomes, role summaries, canary controls and exit evidence. Success reports `helper_audit_passed: true`, `renderer_canary_denied: true`, `notification_broker_absent: true` and `helper_lifecycle: verified-announced-helper-exit`. Missing/unobserved roles, unsafe switches, unknown identities, query errors, allowed canary opens, malformed/duplicate evidence or incomplete exits fail the audit. Log-limit/read failures still write a failed report and retain HOME.

**Unused OS notifications are disabled in this fixture.** The first audit discovered Chromium's `mac_notifications.mojom.MacNotificationProvider` with no Seatbelt policy; its [pinned interface](https://github.com/chromium/chromium/blob/156.0.8078.88/chrome/services/mac_notifications/public/mojom/mac_notifications.mojom) explicitly declares `kNoSandbox`. The probe now adds `SystemNotifications` to `disable-features` before initialization, preserving CEF's existing feature overrides, and disables web notification APIs. This [supported notification-bridge gate](https://github.com/chromium/chromium/blob/156.0.8078.88/chrome/browser/notifications/notification_platform_bridge_delegator.cc) avoids launching the unneeded broker. The audit requires the broker to be absent and still rejects any unsandboxed helper; no exception or forced sandbox-type rewrite was added. Production notification support needs a separate privileged-service decision and review.

**Scope:** these are point-in-time policy predictions for selected reads, plus actual renderer denial of one controlled syscall and exit of announced CEF helpers. They do not audit every sandbox allowance, privileged IPC/broker, inherited resource, network access, unannounced descendant, utility type, production profile/signature or other macOS release. They are not a blanket claim that CEF is securely sandboxed. GPU/utility canary checks are policy queries, not injected syscalls. No user file, login Keychain, system protection or sandbox policy was modified for this audit.

### Evidence and limitations

On the same macOS 27 arm64/Xcode 27 host, actual native runs passed protocol conformance, initialization, child creation, loopback HTTP 200/JavaScript, native text and trusted pointer interaction, viewport resize, `OnBeforeClose`, CEF shutdown and original GPUI termination. Tool unit tests also cover loopback/resource policy, invalid/private profiles, mode handling and close-before-create/duplicate-close transitions. Validation passed: tool formatting, default/runtime/helper Clippy with warnings denied, **10 runtime-enabled host unit tests**, **8 helper unit tests**, **157 Python tests** (32 SDK preparation, 21 bundle, 41 observer, 63 runner), native pump/conflicting-singleton/lifecycle checks, and the runner's actual `--run --build` sequence with root exit 0. The default host also passed its 3 SDK-free unit tests. The scoped native helper audit is recorded above; wider security/compatibility gates remain below.

Private logs, reports, canaries and bundles stay under ignored `target/`; temporary homes are **retained even after a passing audit** for inspection (`home_cleanup: retained-audit-artifacts`). Failure/unknown evidence keeps HOME and is explicitly reported. The runner stops only roots it owns; it does not issue global/process-group kills or automatically delete profiles. Missing helper evidence is not permission to remove a retained profile.

**Window-only visual verification passed on 13 November 2026**, after the Mac was unlocked. Two complete capture runs returned root exit 0 with every ordered native marker; the second used the normal `python3 tools/cef-probe/run_probe.py --run --capture` command without diagnostic interception. The reviewed image shows the actual GPUI header, correctly presented Chromium document, entered fixture text and trusted-click confirmation after viewport resize. It contains no personal browsing data. This verifies this static fixture's presentation, not general website compatibility, animations, IME or accessibility.

![Actual CEF document inside the GPUI probe window, showing native text and trusted pointer verification after resize.](../../docs/screenshots/macos-cef-runtime-probe.png)

Earlier locked-session attempts failed through `screencapture` and ScreenCaptureKit despite permission preflight succeeding; the first unlocked attempt also failed window selection. Subsequent captures succeeded without changing engine or capture code. No display-wide fallback or personal-window capture was attempted. The image in the earlier lifecycle section remains SDK-free evidence only.

Not yet proved: complete sandbox/broker coverage beyond the scoped audit, real persistent Keychain integration, Unicode/IME, accessibility, media, renderer-crash recovery, arbitrary-site behavior, production WebKit/CEF feature parity, per-tab selection, engine switching, signing/distribution or compatibility beyond this host. Resource/redirect negative cases currently have policy unit coverage, not an end-to-end hostile-response test. Do not enable the browser's CEF flag based on this prototype. Next: implement the production adapter and per-tab replacement policy against the [backend contract](../../docs/engine-backends.md). See [the CEF feasibility report](../../docs/macos-cef-gecko.md).
