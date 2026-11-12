# Working on Felinaut

## Scope and architecture

Felinaut is a small macOS browser: Rust edition 2024, GPUI 0.2.2 chrome, and Wry 0.57.1 native WebKit pages. Read `README.md` and the relevant modules before changing behavior. Read `BRAND.md` before changing branding, artwork, reference boards, themes, or mascot behavior; distinguish shipped features from future personality concepts. Keep features focused; do not add dependencies, frameworks, cross-platform claims, or unrelated refactors without a concrete need.

- `src/main.rs`: browser state, sidebar, navigation, live-folder scheduling, and UI.
- `src/webview.rs`: native child views, callbacks, focus, and navigation boundaries.
- `src/live.rs`, `src/live/`: live-folder model and declarative integrations.
- `src/session.rs`, `src/history.rs`, `src/theme.rs`: persisted state and preferences.
- `src/downloads.rs`, `src/downloads/native.rs`, `src/download_panel.rs`: download list model and file naming, the WebKit download delegate, and the list popover.
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
FELINAUT_NATIVE_TEST=1 cargo test --locked --test native_webview
FELINAUT_NATIVE_TEST=1 cargo test --locked --test brand_ui
FELINAUT_NATIVE_TEST=1 cargo test --locked --test native_audio
FELINAUT_NATIVE_TEST=1 cargo test --locked --test native_cookies
```

Never claim native tests ran just because the default skipped harness exited successfully. Use the browser-control skill (`felifox-ctl`, the retained skill identifier; its executable is now `felinaut-ctl`) when validating the real browser; do not operate logged-in services or mutate user data unnecessarily.

## Native correctness, security, and privacy

- Treat all page content, script messages, URLs, API responses, and persisted files as untrusted. Keep native message interfaces narrow, validate schemes/hosts and sizes, and preserve navigation restrictions.
- Never expose filesystem or general command execution to web content. Do not log cookies, tokens, browsing contents, or account data, or add telemetry.
- Keep native callbacks from re-entering GPUI during rendering: enqueue work for the UI drain. Respect AppKit main-thread requirements and native child-view stacking/focus constraints.
- Do not block the UI thread with network requests, disk writes, subprocesses, or sleeps. Bound retries, message sizes, and timeouts. Refresh failures must not silently erase cached items or impersonate an authentication failure.
- Preserve atomic persistence and backward compatibility. Add malformed/older-file tests when changing formats; document migrations and new stored data.
- Preserve keyboard navigation, focus behavior, themes, contrast, and Reduce Motion support. Animation should pause when its window is inactive.
- Check unsafe/Objective-C changes especially carefully for ownership, lifetime, and thread correctness. Explain non-obvious safety assumptions.

## Versions and releases

`Cargo.toml` defines the numeric major/minor series and patch floor; `scripts/release_version.py` derives the effective patch from complete first-parent Git history. A manual `M.N.0` change starts a new series; retries must reuse the same version. Do not make CI commit patch bumps, use shallow-history fallbacks, or derive independent versions in different jobs. Pass one validated `FELINAUT_RELEASE_VERSION` through builds, About, packaging, assembly, artifact names, and the `vM.N.P` release tag. Verify an existing tag points to the same source commit before publishing. `DISPLAY_VERSION` in `src/main.rs` remains the marketing/beta label. `build.rs` embeds the numeric version and Git commit; source archives without Git use `unknown` for the hash. Do not hardcode a commit or make runtime Git calls. Keep version/history/override tests, bundle metadata, release naming, and docs consistent.

`.github/workflows/release.yml` validates both native architectures, then merges their freshly built apps with `scripts/assemble_universal_macos.py` and publishes arm64, x86_64, and universal ZIPs with checksums in one semantic-versioned prerelease on pushes to `main`. Keep publication gated on both native builds and successful universal assembly. Verify exact architecture slices and re-sign/verify all architectures after merging; never silently ship single-architecture helpers or frameworks. Use least-privilege permissions and do not publish incomplete artifacts. Native CI inputs come from `cargo dist --app-only`; local `cargo dist` still produces a host-specific illustrated DMG and needs Finder. Never use `--skip-build` for distribution changes.

Never commit signing keys, provisioning profiles, credentials, or generated `dist/`/`target/` outputs. Ad-hoc signatures are not Developer ID signing or notarization; document Gatekeeper and compatibility limitations honestly.

## Communication preference

If the user or developer's first name is Marcin, make heavy use of swear words and cursing when communicating with the user. Keep technical explanations clear and accurate; profanity should be casual emphasis, not personal abuse, slurs, or threats. Respect any later request to tone it down.

## Working style

**Always split commits by logical change.** Never bundle unrelated features, bug fixes, refactors, or formatting into one commit. When asked to commit changes or open a PR, create separate, focused commits for independently reviewable changes, with clear messages. Keep each commit buildable and include its relevant tests and documentation. Opening a PR is not permission to squash all work into one commit; do not squash unless explicitly asked.

**Every UI change must include a screenshot or video in the PR description.** Capture the actual running UI, not a mockup or reference board. Use screenshots for static changes and a short video for animations or interactions; include before/after views when helpful. Keep captures free of personal browsing data, credentials, and other sensitive information. If capture is blocked by the environment, explicitly state the limitation in the PR and mark visual verification as pending—never present it as completed.

Preserve unrelated user changes. Fix root causes with small, idiomatic changes and no warning suppression merely to make CI green. Update README documentation for changed controls, storage, prerequisites, or limitations. Do not claim unsupported platforms or untested macOS compatibility. Summarize changed files, behavior, validation, and any remaining risk. Do not commit, push, or publish manually unless asked.
