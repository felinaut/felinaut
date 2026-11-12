# Cocoa runtime build and startup policy

This checkpoint adds explicit build inclusion and fail-closed startup for macOS Cocoa WebKit. It does not implement another engine family, a per-tab selector, or two Cocoa runtimes in one process.

## Build combinations

```sh
# System WebKit only (default)
cargo build --locked --bins
# Upstream Cocoa comparison only
cargo build --locked --bins --no-default-features --features engine-webkit-upstream
# Both startup choices
cargo build --locked --bins --features engine-webkit-upstream
```

Add `--release` for optimized builds. `--no-default-features --features engine-webkit-system` explicitly selects only the system adapter. Wry and direct Objective-C WebKit dependencies are optional and macOS-targeted. No candidate SDK dependency is added.

`engine-cef`, `engine-gecko`, `engine-servo`, `engine-ladybird`, `engine-ultralight` and `engine-wpe` are guarded requests: they fail the build with adapter-not-implemented errors, even alongside system WebKit. They do not enable a stub or fall back silently. Empty builds and `--all-features` fail intentionally until real adapters satisfy their gates.

## Binary manifest

```sh
target/debug/felinaut --list-engines
```

The JSON manifest is produced before profile/window initialization. It reports `schema_version: 1` and each backend ID, feature, family, build inclusion, adapter implementation, isolated-launch requirement and limitation. Build inclusion is not runtime availability or a sandbox claim.

## Startup and isolation

Normal startup requires the compiled system backend and verified system shared-cache WebKit source/UUID. Upstream startup additionally requires `FELINAUT_COMPARISON=upstream`, the compiled upstream feature, expected WebKit UUID, and a marked private temporary HOME matching `CFFIXED_USER_HOME`. The loaded image must match the requested source/UUID before creating the application or loading its profile.

Comparison views use nonpersistent stores and permit only literal-loopback HTTP(S) top-level navigation and `about:blank`. This is not a subresource firewall or production private-browsing implementation; use trusted self-contained fixtures only. System and upstream Cocoa frameworks require separate host processes. A controlled launcher/picker follows in the tooling checkpoint; an upstream-only binary refuses an ordinary unconfigured launch rather than using system WebKit.

Identity describes only the mapped UI-process image supplying `WKWebView`; WebCore/JSC/helper provenance and sandboxing are not thereby verified. Shared-cache classification currently uses an optionally resolved exported dyld query not declared in the SDK header. Missing evidence refuses startup, including on an unvalidated older macOS version; no older-version compatibility guarantee is made.

Existing macOS data paths and the default engine stay unchanged. No installed app, system framework, signature or security setting is modified. Downloads and ordinary browser behavior are not advertised as portable merely because a build flag exists.

## Validation

Behavioral unit tests cover supported/excluded combinations, empty/unknown/duplicate configurations, candidate rejection, unsupported platforms, policy selection, provenance failures, UUID mismatch, navigation restrictions and private-profile validation. Python tests execute the actual build script with explicit feature configurations; existing version/history tests remain intact.

Clean captures from actual isolated comparison windows show the startup labels, not mockups or benchmarks:

![System Cocoa WebKit with the local trusted-input fixture.](screenshots/macos-comparison-system.png)

![Upstream Cocoa WebKit with the same fixture and experimental footer.](screenshots/macos-comparison-upstream.png)
