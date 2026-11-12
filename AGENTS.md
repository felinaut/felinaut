# Working on Felifox

## Scope and architecture

Felifox is a small macOS browser: Rust edition 2024, GPUI 0.2.2 chrome, and Wry 0.57.1 native WebKit pages. Read `README.md` and the relevant modules before changing behavior. Keep features focused; do not add dependencies, frameworks, cross-platform claims, or unrelated refactors without a concrete need.

- `src/main.rs`: browser state, sidebar, navigation, live-folder scheduling, and UI.
- `src/webview.rs`: native child views, callbacks, focus, and navigation boundaries.
- `src/live.rs`, `src/live/`: live-folder model and declarative integrations.
- `src/session.rs`, `src/history.rs`, `src/theme.rs`: persisted state and preferences.
- `src/agent.rs`, `tools/`: local browser automation and distribution tools.
- `scripts/package_macos.py`: app bundle, signing, and interactive DMG packaging.

Keep integration-specific extraction in its integration module rather than adding site-specific branches to the browser host. Prefer pure model functions for state transitions so they can be tested independently of GPUI and WebKit.

## Tests are part of every feature

**Add or update unit tests for every feature.** Bug fixes need a regression test that fails before the fix. Cover success, empty/error cases, invalid input, and relevant boundaries—not only the happy path. Do not replace behavioral tests with assertions that source code contains particular strings.

For live folders, cover refresh transitions, seen/dismissed state, missing timestamps, enable/disable combinations, duplicate URLs, and persistence round trips. Test API/DOM extractors with offline fixtures, including pagination, authentication failures, permission errors, rate limits, and malformed responses. Keep native browser tests opt-in, loopback-only, and independent of users' real accounts.

Before finishing, run the checks relevant to your changes and report exactly what passed or could not run:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --bins
cargo fmt --manifest-path tools/dist/Cargo.toml --all --check
cargo clippy --locked --manifest-path tools/dist/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path tools/dist/Cargo.toml
python3 -m unittest discover -s scripts -p 'test_*.py'
```

Use `-F gpui/runtime_shaders` for browser checks if full Xcode/Metal is unavailable. Native UI checks require a macOS desktop session:

```sh
FELIFOX_NATIVE_TEST=1 cargo test --locked --test native_webview
FELIFOX_NATIVE_TEST=1 cargo test --locked --test brand_ui
FELIFOX_NATIVE_TEST=1 cargo test --locked --test native_audio
```

Never claim native tests ran just because the default skipped harness exited successfully. Use the `felifox-ctl` skill when validating the real browser; do not operate logged-in services or mutate user data unnecessarily.

## Native correctness, security, and privacy

- Treat all page content, script messages, URLs, API responses, and persisted files as untrusted. Keep native message interfaces narrow, validate schemes/hosts and sizes, and preserve navigation restrictions.
- Never expose filesystem or general command execution to web content. Do not log cookies, tokens, browsing contents, or account data, or add telemetry.
- Keep native callbacks from re-entering GPUI during rendering: enqueue work for the UI drain. Respect AppKit main-thread requirements and native child-view stacking/focus constraints.
- Do not block the UI thread with network requests, disk writes, subprocesses, or sleeps. Bound retries, message sizes, and timeouts. Refresh failures must not silently erase cached items or impersonate an authentication failure.
- Preserve atomic persistence and backward compatibility. Add malformed/older-file tests when changing formats; document migrations and new stored data.
- Preserve keyboard navigation, focus behavior, themes, contrast, and Reduce Motion support. Animation should pause when its window is inactive.
- Check unsafe/Objective-C changes especially carefully for ownership, lifetime, and thread correctness. Explain non-obvious safety assumptions.

## Versions and releases

`Cargo.toml` is the numeric version source; `DISPLAY_VERSION` in `src/main.rs` is the public marketing/beta label. `build.rs` embeds the Git commit for About; source archives without Git use `unknown`. Do not hardcode a commit or make runtime Git calls. Keep About tests, packaging metadata, release naming, and docs consistent when changing version behavior.

`.github/workflows/release.yml` validates both native architectures and publishes a per-commit prerelease on pushes to `main`. Keep publication gated on all validation/builds, use least-privilege permissions, and do not publish incomplete artifacts. CI packages ZIPs via `cargo dist --app-only`; `cargo dist` produces the illustrated DMG locally and needs Finder. Never use `--skip-build` for distribution changes.

Never commit signing keys, provisioning profiles, credentials, or generated `dist/`/`target/` outputs. Ad-hoc signatures are not Developer ID signing or notarization; document Gatekeeper and compatibility limitations honestly.

## Working style

**Always split commits by logical change.** Never bundle unrelated features, bug fixes, refactors, or formatting into one commit. When asked to commit changes or open a PR, create separate, focused commits for independently reviewable changes, with clear messages. Keep each commit buildable and include its relevant tests and documentation. Opening a PR is not permission to squash all work into one commit; do not squash unless explicitly asked.

Preserve unrelated user changes. Fix root causes with small, idiomatic changes and no warning suppression merely to make CI green. Update README documentation for changed controls, storage, prerequisites, or limitations. Do not claim unsupported platforms or untested macOS compatibility. Summarize changed files, behavior, validation, and any remaining risk. Do not commit, push, or publish manually unless asked.
