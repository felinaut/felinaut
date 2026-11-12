# Native CEF/GPUI lifecycle probe

Developer-only macOS investigation, separate from the browser's Cargo graph. **This first checkpoint links no CEF engine or SDK.** It proves a proposed application-lifecycle boundary before acquiring a runtime or changing the default browser. `engine-cef` in the browser remains explicitly unsupported.

## Build and test

From the repository root, with full Xcode/Metal and a macOS desktop session:

```sh
cargo fmt --manifest-path tools/cef-probe/Cargo.toml --all --check
cargo clippy --locked --manifest-path tools/cef-probe/Cargo.toml --target-dir target --all-targets -- -D warnings
cargo test --locked --manifest-path tools/cef-probe/Cargo.toml --target-dir target
cargo build --locked --manifest-path tools/cef-probe/Cargo.toml --target-dir target

# Separate fresh processes; the first creates no window.
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

## Not proved yet

No actual `CefAppProtocol` conformance, framework load, CEF initialization, message pumping, helper sandbox, page rendering, input/IME, resize, WebKit coexistence, per-tab selection, packaging or distribution support is established by this SDK-free checkpoint. A separately built official CEF example would not by itself prove GPUI coexistence either.

Next: use a matching pinned macOS arm64 CEF runtime/binding pair, implement real protocol conformance and bounded external-pump scheduling, initialize sandboxed helpers correctly, and render a trusted loopback page in the GPUI content area. Do not enable the browser's CEF flag or bypass sandbox checks merely because the application subclass works. See [the CEF feasibility report](../../docs/macos-cef-gecko.md).
