# Felinaut CEF subprocess helper

Standalone production child entry point for the root browser's CEF adapter in
`src/page/cef/runtime.rs`. Cargo produces **`felinaut-helper`**; packaging copies
that executable into each helper bundle under its required CEF name. It is not a
browser, probe launcher, SDK installer, or standalone runnable app. There is no
GPUI or Wry dependency, browser-process handler, canary, audit observer requirement,
filesystem bridge, mock-keychain override, or page-visible native global.

## Prerequisites and build

Currently **native macOS arm64 only**, matching the root CEF adapter. CEF is an
optional root feature; this separate helper always builds against CEF. This does
not change the root's defaults, feature selection, manifest, or lockfile.

Requirements:

- Rust 1.94+ and the macOS C/C++ toolchain, CMake and Ninja required by `cef-dll-sys`.
- The already prepared, verified macOS arm64 SDK/runtime at
  `target/cef-runtime/156.0.26/macosarm64`, including `archive.json`, headers,
  wrapper sources, framework resources and `Libraries/libcef_sandbox.dylib`.
- Exact `cef` and `cef-dll-sys` **156.4.0** bindings (published with the
  `+156.0.26` metadata), paired with CEF
  `156.0.26+g8ea0c73+chromium-156.0.8078.88`, as in the root/probe baseline.
- Cached Cargo dependencies for offline builds. This crate has its own lockfile.

From the repository root:

```sh
CEF_PATH=/Users/marco/felifox/target/cef-runtime/156.0.26/macosarm64 \
  cargo build --locked --offline --manifest-path tools/cef-helper/Cargo.toml --target-dir target
cargo fmt --manifest-path tools/cef-helper/Cargo.toml --all --check
CEF_PATH=/Users/marco/felifox/target/cef-runtime/156.0.26/macosarm64 \
  cargo clippy --locked --offline --manifest-path tools/cef-helper/Cargo.toml --target-dir target --all-targets -- -D warnings
CEF_PATH=/Users/marco/felifox/target/cef-runtime/156.0.26/macosarm64 \
  cargo test --locked --offline --manifest-path tools/cef-helper/Cargo.toml --target-dir target
```

The helper performs **no runtime acquisition**. The build script requires an
absolute `CEF_PATH` and the pinned version header. Supply the verified SDK before
invoking Cargo: the upstream `cef-dll-sys` build script can attempt SDK acquisition
if its path/archive metadata is absent or invalid. Cargo's `--offline` constrains
Cargo, not arbitrary dependency build-script networking. Neither this helper nor
its build script downloads or repairs an SDK; prepare it separately using the
existing repository tooling. Do not point `CEF_PATH` at an unverified directory.

## Packaging contract

The root executable is exactly `felinaut`; the outer `.app` name may vary:

```text
Felinaut.app/Contents/
  Info.plist
  MacOS/felinaut
  Frameworks/
    Chromium Embedded Framework.framework/
      Chromium Embedded Framework
      Libraries/libcef_sandbox.dylib
      ... matching CEF libraries and resources ...
    felinaut Helper.app/Contents/
      Info.plist
      MacOS/felinaut Helper
```

The other accepted helper app/executable pairs are exactly
`felinaut Helper (GPU)`, `felinaut Helper (Renderer)`,
`felinaut Helper (Plugin)`, and `felinaut Helper (Alerts)`, each with matching
`.app/Contents/MacOS/<name>` and `Contents/Info.plist`. These are bundle-name
variants, not additional accepted `--type` roles. Existing parent packaging owns
bundle metadata, resource copying, permissions, entitlements and signing. Do not
launch `target/debug/felinaut-helper` directly or rename the root executable.

Preflight rejects missing/mismatched bundle components and redirected executable,
metadata or framework-root paths. Versioned framework links are allowed only when
their resolved targets remain within the framework; broken/cyclic/escaping links
and special filesystem entries fail closed. The traversal is bounded to 16,384
entries. Both loaded dylibs must have nonempty, thin native arm64 Mach-O dylib
headers (not universal, Intel, arm64e, or executable images).

These checks are defense in depth, **not signature or provenance verification**,
and do not eliminate filesystem replacement races. The containing installed
bundle must remain trusted and immutable to untrusted writers. Packaging must
verify the SDK, complete resource set, executable architecture and code signatures.
Ad-hoc signing is not Developer ID signing or notarization; this helper adds no
Gatekeeper bypass or compatibility certification.

## Startup and renderer boundary

Startup order is deliberate:

1. Validate raw arguments and bundle/layout/Mach-O headers using Rust only.
2. Construct CEF argv without calling CEF command-line APIs; initialize and retain
   CEF's native sandbox object. Sandbox initialization failure aborts startup;
   there is no unsandboxed fallback.
3. Load the bundle-relative CEF framework using the helper loader.
4. Select/check the binding's last supported CEF API and verify the pinned CEF
   source commit, matching the root's runtime baseline.
5. Call `execute_process` and propagate its nonnegative child exit status. Drop
   app/renderer state before the loader and the sandbox. A negative result fails
   rather than entering browser initialization.

Exactly one `renderer`, `gpu-process`, or `utility` role is required. Single- and
double-dash forms are checked consistently, including duplicates. Early switch
termination, non-UTF-8/malformed roles and the probe baseline's sandbox-bypass
switches are refused, including `=false`/`=0` variants. Ordinary Chromium child
arguments and seatbelt descriptors pass through; the helper does not invent or
replace a sandbox policy.

The production app routes only renderer context-created/context-released,
browser-destroyed and process-message callbacks. It reuses the native-only
`evaluation::Bridge` and panic containment from
`tools/cef-probe/helper/src/evaluation.rs` via `#[path]`, and the root's
`src/page/cef/protocol.rs` as `crate::evaluation_protocol`. No old source file is
modified or duplicated. The full repository checkout is consequently required to
build this standalone Cargo crate; it is not a self-contained source package.

The bridge admits bounded browser-originated messages, binds requests to native
browser/frame/context identities, and releases V8 handles on renderer lifecycle
callbacks. It installs no native V8 extension/handler or host global. Evaluation
uses the captured JavaScript evaluator; site CSP can block it. JSON serialization
and script execution cannot be forcibly interrupted by this API. The browser must
continue enforcing document scopes, request deadlines and delivery cancellation.
A renderer hang is not solved by timing out the browser's request ledger.

## Validation scope

Unit tests cover malformed/duplicate/unknown roles, sandbox-bypass spellings,
exact helper names, missing/redirected bundle files, contained/escaping/broken
framework links, and invalid/truncated/wrong-architecture dylib headers. Fixtures
are synthetic and are never loaded. Shared evaluation/protocol tests also run,
including result bounds, replay protection, context invalidation and panic
containment. Tests do not initialize CEF or inspect real accounts.

Passing these checks does **not** establish native sandbox enforcement, successful
production launches, signed/notarized packaging, input/accessibility parity,
arbitrary-site safety, or Intel/other-platform support. Real root/helper integration
still needs the parent's opt-in loopback, clean-profile native validation on the
actual target machine. The production helper deliberately has no probe canary
requirement; the separate existing probe remains the sandbox-evidence harness.
