# Optional engine builds and selection

**Status, 13 November 2026:** the `multiplatform` working tree now implements optional **system WKWebView + Chromium (CEF) per-tab selection in the root Felinaut application**. A fresh native macOS arm64 smoke/capture passed with WebKit `22625.1.29.11.27` and CEF `156.0.26` / Chromium `156.0.8078.88`, then graceful root exit 0. See the [test guide, switching semantics and actual root UI](macos-per-tab-engines.md). Default shipping builds remain system-WebKit-only. Upstream Cocoa comparison remains separate-process; CEF + upstream Cocoa coexistence is untested. No SDK is acquired merely by selecting a feature or invoking the developer launcher.

## Build inclusion is not runtime availability

There are four separate gates:

1. **Requested at build time:** Cargo features choose adapters. Defaults can be disabled.
2. **Implemented:** a native adapter actually exists. Requests for unimplemented adapters fail the build, including combinations that also request system WebKit; there is no silent fallback.
3. **Runtime available:** compatible libraries/helpers/resources and startup policy pass validation. A compiled upstream WebKit adapter does not mean its external products are installed or distributable.
4. **Selected:** the runtime actually used by a page/process. Per-tab diagnostics report the actual page backend and runtime. The separate Cocoa comparison verifies the mapped `WKWebView` source and UUID, not just a requested feature or framework filename.

The default stays small and uses Apple's system engine. Normal startup now requires verified system-image provenance before loading a profile; an unidentified runtime cannot bypass upstream isolation. Because shared-cache classification currently uses an optional exported dyld query, an unavailable/unusable query causes an explicit startup refusal, including on an unvalidated older macOS version. This is a compatibility limitation, not a claim of support for every macOS release. Wry and the direct Objective-C WebKit dependency are optional, macOS-targeted dependencies activated by the Cocoa features and currently also by `engine-cef` for platform integration. CEF uses the existing pinned optional `cef` / `cef-dll-sys` pair and internal host/protocol crates; this integration adds no other external dependency. No Servo, Gecko, Ladybird, Ultralight or WPE dependency has been added as a placeholder.

## Current feature matrix

| Cargo feature            | Status on this Mac                                                     | Selection scope                                                                     |
| ------------------------ | ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `engine-webkit-system`   | Implemented; **default**                                               | Normal browser launch, or isolated system comparison                                |
| `engine-webkit-upstream` | Implemented experimental Cocoa comparison adapter                      | Isolated launcher and trusted local products required; one runtime per host process |
| `engine-cef`             | Implemented experimental root adapter; native arm64 mixed build passed | Per-tab WebKit / Chromium (CEF); prepared SDK and developer bundle required         |
| `engine-gecko`           | Build rejected: adapter not implemented                                | Maintained desktop embedding path not established                                   |
| `engine-servo`           | Build rejected: adapter not implemented                                | Concrete embedding APIs found; arm64 sandbox and GPUI integration unresolved        |
| `engine-ladybird`        | Build rejected: adapter not implemented                                | Frontend/Hub embedding boundary and supported presentation path required            |
| `engine-ultralight`      | Build rejected: adapter not implemented                                | Proprietary license/edition, single-process security and accessibility gates        |
| `engine-wpe`             | Build rejected: adapter not implemented                                | Linux investigation; no validated native macOS or Windows port                      |

Rejected features are explicit requests/guardrails, **not working SDK integrations**. `--all-features` therefore intentionally fails today. So do an empty `--no-default-features` build and the internal `_cocoa-webkit` dependency feature on its own. At least one implemented adapter must be selected. Native platform validation also rejects unsupported OS targets rather than advertising portable support through a Cargo flag.

## Build only system WebKit

```sh
cargo build --locked --bins
# Equivalent explicit selection:
cargo build --locked --bins --no-default-features --features engine-webkit-system
./target/debug/felinaut
```

The upstream comparison mode is excluded. Requesting it is rejected before opening its temporary profile or browser window. The comparison picker disables its upstream and Both choices.

## Build only upstream Cocoa WebKit

```sh
cargo build --locked --bins --no-default-features --features engine-webkit-upstream
python3 scripts/compare_macos_webkit.py --engine upstream
```

This does **not** install, download, link a production bundle, or enable unrestricted upstream browsing. Follow the [Cocoa comparison setup and security limits](macos-webkit-comparison.md). The launcher supplies the loader environment, expected UUID and isolated temporary home; pages use nonpersistent stores and trusted local fixtures. An ordinary unconfigured launch is refused rather than silently starting system WebKit. The system comparison choice is excluded.

## Build both Cocoa runtime choices

```sh
cargo build --locked --bins --no-default-features --features engine-webkit-system,engine-webkit-upstream
python3 scripts/compare_macos_webkit.py
# Or bounded functional verification of both:
python3 scripts/compare_macos_webkit.py --engine both --smoke --capture
```

Keeping the default and adding `--features engine-webkit-upstream` is equivalent. The launcher first reads the binary's manifest with a sanitized loader environment, then enables only built choices. Runtime products and loaded identity are validated independently. It refuses an excluded choice, even when an older products tree happens to exist on disk.

The Cocoa comparison launcher requires a build without `engine-cef`; comparison-mode startup explicitly refuses that combination while upstream/CEF coexistence remains unvalidated.

**Both Cocoa versions cannot be loaded into the same application process.** The current selector starts separate isolated processes/windows. This does not implement two Cocoa versions in adjacent tabs. A cross-process page-presentation bridge would be separate work, not a feature flag.

Add `--release` to the Cargo commands to build optimized executables. The comparison launcher currently uses `target/debug/felinaut` and `felinaut-ctl`; it does not automatically select a release executable. Existing `cargo dist` packaging remains the system-WebKit default; there is no distribution recipe for an upstream/alternate-engine app.

## Build and test system WebKit + CEF

```sh
# Explicit SDK acquisition, only if not already prepared:
python3 tools/cef-probe/prepare_runtime.py --download
python3 tools/cef-probe/prepare_runtime.py --verify
# No actions by default; inspect the plan first:
python3 scripts/run_macos_engines.py
# Both modes freshly build with --features engine-cef and assemble the developer bundle:
python3 scripts/run_macos_engines.py --run
python3 scripts/run_macos_engines.py --smoke --capture --smoke-timeout 120
```

The launcher requires native macOS arm64, verifies the already prepared `target/cef-runtime/156.0.26/macosarm64` SDK and original archive, and never auto-downloads them. Cargo may fetch locked Rust dependencies. `--run` leaves an interactive app open in a fresh private HOME, retaining bundle/logs/profile; smoke checks the loopback fixture and quits normally. Add `--runtime-shaders` without full Xcode/Metal. A loose CEF-enabled executable lacks the bundle-relative framework/helpers. The developer app is not a notarized or distributable signed package; `cargo dist` still ships system WebKit only.

`engine-cef` is optional and currently also activates Cocoa platform-integration dependencies. **CEF-only selection passes all-target Clippy and unit tests** with `--no-default-features --features engine-cef`; the native evidence here is specifically the mixed app, not a separately launched CEF-only app. Do not advertise a Cocoa-dependency-free CEF configuration. There are no new external dependencies beyond the existing pinned CEF pair; the host/protocol crates are internal.

Use the toolbar engine button or tab context menu. Loaded tabs require explicit native confirmation for a fresh GET, losing forms/document state/native history without cookie migration; New Tab selection does not reload. The old page survives until replacement `creation_ready`; creation failure keeps it. CEF tabs share an in-memory context distinct from WebKit, and their cookies/logins are lost on quit. CEF downloads, permissions, audio control, zoom, trusted automation and page snapshots are explicitly unsupported. Private-IPC evaluation respects CSP and may error. Interactive automation is disabled; comprehensive real-Keychain validation remains pending; smoke uses only a marked private fresh HOME, mock test keychain and local fixture with DOM-only input. See [the full test guide and limitations](macos-per-tab-engines.md).

## Inspect a binary's build

```sh
./target/debug/felinaut --list-engines
```

This outputs JSON (`schema_version: 1`) before profile/window initialization. Each descriptor includes `id`, `feature`, `family`, `built`, `adapter_implemented`, `requires_isolated_launcher` and a `limitation`. It reports no profile paths, account information or browsing contents. It lists researched candidates honestly as unimplemented, not as ready engines. `built` alone is **not a runtime availability or sandbox claim**.

## macOS evaluation and next implementation gates

- [CEF and Gecko source/API investigation](macos-cef-gecko.md): historical lifecycle/embedding research and standalone checkpoints, now followed by the root CEF integration above. Real-Keychain behavior, complete input/IME/accessibility, optional capabilities, recovery and distribution remain gates. Gecko lacks an established maintained desktop SDK for this app.
- [Servo, Ladybird and Ultralight source/API investigation](macos-experimental-engines.md): concrete entry points, pins, licenses, input/presentation requirements and security blockers. These reports do not claim that an SDK was downloaded or an alternate page ran here.
- Servo remains a possible independent open-source embedding experiment, scoped to trusted loopback content until the inspected arm64 sandbox gap is resolved. Ladybird needs a frontend/Hub boundary investigation. Ultralight is a trusted-UI experiment only after license review, not a safe arbitrary-site backend by default.

For each implemented adapter, replace its explicit unsupported gate only with real adapter/context construction, target-specific optional dependencies, runtime checks, behavioral tests and native loopback evidence. Do not turn `adapter_implemented` on to make an empty feature build green.

Current per-tab selection uses stable backend IDs and explicit runtime readiness. It recreates a loaded page only after confirmation; it does not migrate documents or cookies. CEF's shared in-memory context is not durable per-profile support or download parity. The broader profile, recovery, independent-download and generation-safety requirements in the [backend contract](engine-backends.md) remain authoritative; do not treat the local smoke as completion of all of them.

## Validation evidence

**Current root CEF checkpoint, 13 November 2026:** `python3 scripts/run_macos_engines.py --smoke --capture --smoke-timeout 120` passed actual mixed-tab runtime identity, WebKit/CEF same-origin storage separation, shared context across two CEF tabs, independent close, rejected invalid/unconfirmed switches, fresh confirmed replacement documents, window capture and graceful root exit 0. This is loopback/DOM-only evidence, not production Keychain, complete IME/accessibility, CEF trusted-input/snapshot parity or a helper-exit audit. See [the recorded run](macos-per-tab-engines.md#recorded-native-evidence).

**Historical Cocoa-only build checkpoint, 12 November 2026:** on macOS 27 arm64, default system-only, upstream-only and combined builds each passed Clippy with warnings denied, browser/CLI unit tests, and actual rebuilt-browser loopback smoke for their included runtime(s). The combined run captured both real windows. Actual Cargo checks also rejected no-engine builds and each unimplemented candidate. The native picker was displayed/captured with system-only choices; selection itself was not automated. Exact suite counts and current native harness results are recorded in [macOS backend status](macos-backend-status.md).

The pure Rust selection model tests success, exclusion, invalid IDs, duplicate/empty configurations, unsupported platforms and unimplemented candidates even alongside a working fallback. Python tests execute the real build script with explicit Rust feature configurations, exercise manifest parsing/picker gating and preserve isolation/cleanup/capture regression coverage. These older model/build-gate tests did not establish alternate-engine native support; the separate root CEF run above supplies the newer, explicitly bounded native evidence.

![Actual native picker for a system-only build; upstream and Both are disabled.](screenshots/macos-engine-picker-system-only.png)
