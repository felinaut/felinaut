# Felinaut · 0.1 beta

A simple Rust desktop browser: **GPUI for the interface, Wry / WKWebView for the web**. On macOS, WebKit handles HTML, CSS, JavaScript, images, forms, networking and web storage. There is no bundled browser engine and no custom page renderer.

## Felinaut identity and compatibility

Our mascot is **Felinaut**, a happy female astronaut cat and curious explorer who loves colorful things—especially wool balls and planets that look like yarn. She now has a little cardboard exploration box on New Tab. The design source of truth is [`BRAND.md`](BRAND.md); [`references/README.md`](references/README.md) links the four rebuilt brand/theme/artwork/icon boards and their reproducible SVG/PNG generator. Original reference images remain unchanged in a clearly labeled historical archive.

**Shipped now:** astronaut-cat artwork and icons, box composition, six selectable themes, gentle existing bob/scene motion, and updated application/distribution names. **Future personality concepts:** tab batting, zoomies, idle naps, easter eggs, contextual cat reactions and opt-in sounds are specified in the guide, not implemented or advertised as working features. No surprise audio, input changes, website transformations, tracker-blocking claims, or browsing telemetry were added.

**Existing data stays put.** The profile remains `~/Library/Application Support/Felifox/`, and the bundle identifier remains `io.github.casaroli.felifox` to avoid resetting existing app-managed state and WebKit identity or invalidating existing provisioning-profile associations. No profile migration or deletion occurs. The executable names are now `felinaut` and `felinaut-ctl`; reinstall the renamed app and update local launch scripts. The repository URL is unchanged.

Use `FELINAUT_AGENT`, `FELINAUT_NATIVE_TEST`, `FELINAUT_SIGN_IDENTITY`, `FELINAUT_NOTARY_PROFILE` and `FELINAUT_PROVISIONING_PROFILE` in new scripts. The corresponding `FELIFOX_*` names remain compatibility fallbacks; an explicitly supplied new variable takes precedence, including an empty/disabled value. Agent control remains off unless explicitly enabled with `1`. `felinaut://newtab` is the internal New Tab address; the old `felifox://newtab` is accepted as an alias, without a remote search.

## Run

Currently targets **macOS**. Requirements:

| Requirement                                | Why                                                             | Install                                                                                                           |
| ------------------------------------------ | --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| Rust 1.94+ (edition 2024)                  | Compiles Felinaut                                               | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh`                                                 |
| Xcode (full app, with its Metal toolchain) | GPUI compiles its Metal shaders at build time via `xcrun metal` | App Store, then `sudo xcode-select -s /Applications/Xcode.app` and `xcodebuild -downloadComponent MetalToolchain` |
| Python 3.11+                               | `cargo dist` packaging only                                     | `brew install python@3.13`                                                                                        |

```sh
cargo run
```

With only the Xcode Command Line Tools (no `metal` compiler), you can still build and test for local development by letting GPUI compile its shaders at runtime instead:

```sh
cargo run -F gpui/runtime_shaders
```

This flag changes nothing in `Cargo.toml`; use full Xcode for release and `cargo dist` builds.

For an optimized executable:

```sh
CARGO_PROFILE_RELEASE_STRIP=none cargo build --release
./target/release/felinaut
```

The first build downloads dependencies from crates.io. The new-tab page contains no remote assets or telemetry. Search queries go to your chosen search engine (DuckDuckGo by default). Websites control their own requests once loaded.

## Build a DMG for friends

From the project root, run:

```sh
cargo dist
```

This runs a locked, optimized release build from the current source, creates and verifies the app bundle, then builds and verifies the illustrated drag-to-install DMG. Previous local artifacts are replaced automatically, so you can use the same command after every change.

Requires **Python 3.11+**, the Rust/Xcode tools above, and a macOS desktop session with Finder. No pip packages or third-party packaging utilities are needed. If prompted, allow your terminal/editor to control Finder in **System Settings → Privacy & Security → Automation** so the installer layout can be saved.

Outputs are generated in `dist/` (excluded from Git):

- **`Felinaut.app`** — an optimized native app bundle with the custom astronaut-cat `.icns` icon, bundle metadata, version, and license.
- **`Felinaut-0.1-beta-macos-arm64.dmg`** on Apple Silicon, or **`Felinaut-0.1-beta-macos-x86_64.dmg`** on an Intel build host.
- **`SHA256SUMS`** — checksum for the DMG.

Double-click the DMG, then **drag Felinaut onto Applications**. The installer includes a high-resolution illustrated background, a directional arrow, correctly positioned icons, and a real `/Applications` shortcut. Its artwork is drawn at build time from the existing SVG mascot plus native AppKit gradients and paths.

**Send the `.dmg` file to your friends**, not the `.app` folder or the contents of `target/`. On this Apple Silicon Mac the file is `dist/Felinaut-0.1-beta-macos-arm64.dmg`; testers need an Apple Silicon Mac for this build. Intel builds must be produced on an Intel host. There is no updater: send a newly generated DMG for each test build.

`cargo dist` uses a tiny dependency-free Rust launcher to run the locked release build **before** invoking the Python packager. Packaging is skipped if the build fails, and the packager reuses that freshly built executable rather than compiling twice. `make dist` delegates to the same command. Use `cargo dist --app-only` to produce just the app. For artwork-only packaging iterations, the lower-level script also accepts `--skip-build`, but that intentionally reuses an older executable and should not be used when distributing code changes.

**Signing:** by default packages have a verified **local ad-hoc signature**, which Gatekeeper blocks when downloaded onto another Mac. To produce a DMG that opens anywhere, sign with a **Developer ID Application** certificate and notarize:

```sh
xcrun notarytool store-credentials felinaut-notary --apple-id <apple-id> --team-id <team-id>   # once; prompts for an app-specific password
FELINAUT_SIGN_IDENTITY="Developer ID Application: <name> (<team-id>)" FELINAUT_NOTARY_PROFILE=felinaut-notary cargo dist
```

This signs the app (hardened runtime, secure timestamp) and the DMG, submits the DMG to Apple, staples the tickets, and checks both with `spctl`. Local `cargo dist` packaging still targets the host architecture; the automated GitHub releases below instead combine two native builds into a universal app. A macOS 12.0 minimum deployment target is configured, but compatibility has not been tested across every macOS version.

The build preserves Cargo's procedural-macro metadata (`CARGO_PROFILE_RELEASE_STRIP=none`) to avoid a stripping issue on the current toolchain, then strips only the finished copied executable before signing. This leaves the normal Cargo profile unchanged. Embedded SVGs and Metal shaders require no separate runtime asset downloads.

Packaging checks:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
codesign --verify --deep --strict dist/Felinaut.app
hdiutil verify dist/Felinaut-0.1-beta-macos-arm64.dmg
```

## Automated GitHub prereleases

[`.github/workflows/release.yml`](.github/workflows/release.yml) runs on **every push to `main`**, including merge commits. It can also be started with **Actions → Main prerelease → Run workflow** for the selected ref. Only `main` publishes a GitHub release; manually selecting another ref validates and builds the artifacts without reserving a public version tag. There is no workflow-wide concurrency group: newer commits do not cancel older builds, and releases may finish out of commit order. Only publishing is locked per semantic release tag, so duplicate runs for the same version cannot publish concurrently or cancel the active publisher; different versions remain independent. This is not a FIFO queue: GitHub may replace redundant pending publishers for the same tag.

- **Both architectures must pass:** native Apple Silicon (`macos-15`, `arm64`) and Intel (`macos-15-intel`, `x86_64`) jobs check formatting, run Clippy with `-D warnings`, run Cargo binary unit tests and Python packaging tests, then build with the existing **`cargo dist --app-only`** launcher. The independent Rust packaging launcher is also formatted, linted and unit-tested. Dependency resolution is locked. Stable Rust, Python 3.13 (at least 3.11 required), full Xcode and its Metal compiler are used; the Metal component is downloaded if missing. Runtime shaders are not substituted for release builds.
- **One universal app:** after both native builds pass, a macOS assembly job downloads their ZIPs from the same workflow run, checks each signature, expected thin architecture, bundle layout and current-checkout metadata, then combines the executables with `lipo`. The canonical arm64 bundle supplies the resources, including its icon (AppKit rasterization can vary slightly between runners). Unexpected helpers, frameworks, symlinks or additional Mach-O resources are rejected rather than silently left single-architecture. The staged app is signed again **after merging**, and its exact arm64 + x86_64 slices and signature are verified across all architectures. Native inputs are never modified.
- **ZIP, not DMG:** hosted CI does not provide the interactive Finder/Automation session required by the illustrated DMG packager. The universal `Felinaut.app` is archived with macOS `ditto --sequesterRsrc --keepParent`, preserving bundle structure and macOS metadata. Local `cargo dist` still produces the host-specific Finder DMG described above.
- **Three downloads, one release:** `Felinaut-<M.N.P>-<12-character Git hash>-macos-arm64.zip`, `…-macos-x86_64.zip`, and `…-macos-universal.zip`, plus **`SHA256SUMS` covering all three**. The universal app works on both Apple Silicon and Intel; the thin ZIPs are smaller alternatives and leave room for architecture-specific updates later. The semantic prerelease tag is **`vM.N.P`**, targets the exact source commit, and is independent of the marketing label `0.1 beta`. GitHub's automatically generated source archives are not runnable app bundles.
- **Complete publication and safe retries:** universal assembly needs the entire native build matrix to succeed, and publishing in turn requires successful assembly. Only after universal assembly passes are the untouched thin ZIPs collected alongside it. Publishing accepts exactly those three versioned ZIPs and checksums each. An existing version tag must resolve to the same commit; a mismatched tag, failed lookup, or malformed response stops publication. The native `gh` CLI creates a draft prerelease, uploads all three ZIPs and the checksum, and only then publishes it. A failed upload leaves a draft, not a public partial release. Rerunning the same commit retains its version and resumes its draft only if the draft targets that exact full commit SHA; branch-name or mismatched draft targets are rejected before upload. An already published version is left unchanged. Older `main-<SHA>` releases are not rewritten. No rolling tag or latest stable release is overwritten. Build artifacts are retained in Actions for 14 days; published release assets have no workflow-imposed expiry.
- **Least privilege:** builds have only `contents: read`; only the publishing job has `contents: write`, through the built-in `GITHUB_TOKEN`. No personal access token, Apple certificate, provisioning profile or notarization credentials are used. Repository/organization policy must allow Actions to create releases, and both hosted runner labels must be available to the repository. Official JavaScript actions use Node 24 and require Actions Runner 2.327.1 or newer; the configured GitHub-hosted runners satisfy that requirement.

Download the universal ZIP on either Apple Silicon or Intel, or choose the smaller ZIP matching your Mac, and move the extracted **Felinaut.app** into Applications. Compare `shasum -a 256 <downloaded.zip>` with its entry in `SHA256SUMS`; if you downloaded all three ZIPs, use `shasum -a 256 -c SHA256SUMS`. macOS selects the universal app's native executable slice automatically; Apple Silicon does not need Rosetta to run this app. These are **ad-hoc signatures, not Developer ID signatures, and there is no Apple notarization**. Gatekeeper can block downloaded apps; only use a macOS security override if you trust the source and exact commit. Checksums detect corruption, not an independently authenticated publisher. The workflow does not bypass Gatekeeper.

These are per-commit universal testing prereleases, not an auto-updater or compatibility certification. The packager requests macOS 12.0 as the minimum deployment target; it does not test every supported macOS version. Interactive WebKit/About/audio integration tests remain opt-in and are not run by this workflow. Hosted runner images, stable Rust and Xcode can change, so locked dependencies do not make rebuilds byte-for-byte reproducible. Runner queue/capacity limits, unavailable Metal downloads, failed validation or packaging, and GitHub API failures can delay or prevent a release; a failed run can be rerun after its cause is resolved.

### Universal assembly validation

`scripts/assemble_universal_macos.py` is the macOS-only assembly step, not a build bypass: feed it the two freshly built apps from the same validated CI run. It verifies matching current-checkout metadata, but metadata alone cannot prove build provenance; do not mix manually sourced apps or different commits. It stages privately, re-signs ad-hoc regardless of local Developer ID environment settings, and creates its output ZIP only after verification. No existing output is overwritten.

```sh
python3 scripts/assemble_universal_macos.py \
  --arm64 native-apps/arm64/Felinaut.app \
  --x86-64 native-apps/x86_64/Felinaut.app \
  --output release-assets/Felinaut-VERSION-COMMIT-macos-universal.zip
```

Create the output directory first. The tests in `scripts/test_assemble_universal_macos.py` include offline, cross-compiled and signed Mach-O app fixtures, ZIP extraction, exact-slice checks, re-signature verification, unchanged-input checks, malformed input, mismatched metadata, unsupported layouts and failure cleanup. Fixtures are never launched. Native fixture tests need macOS/Xcode tools; non-macOS environments explicitly skip them. `scripts/test_release_workflow.py` also executes the workflow's assembly and checksum logic with isolated fixtures, including missing/stale artifacts, byte-exact preservation of native downloads, semantic-tag ownership, and exact three-download enforcement.

### Automatic semantic versions: M.N.P

`Cargo.toml` stores the **major/minor series and patch floor**, initially `0.1.0`. Before the native builds, a read-only metadata job with full Git history runs `scripts/release_version.py`. Starting at the commit that introduced the current Cargo version, each subsequent **first-parent commit** adds one to its patch component:

- The commit changing the base to `0.2.0` releases **0.2.0**; the following main commits release **0.2.1**, **0.2.2**, and so on.
- A manual major change to `1.0.0` starts **1.0.0**, then **1.0.1**, and so on.
- Merges count once along the main first-parent chain; side-branch commits are not counted separately. Cosmetic manifest changes do not reset the counter.
- Retries of one commit reuse the same version. Failed/skipped releases or pushes containing several commits can leave gaps: versions are monotonic identifiers, not a count of successful uploads. The initial automatic patch reflects the existing series' history, so it need not start at `0.1.1`.
- Shallow history, malformed versions, overflow, and uncommitted version changes are rejected. The workflow never commits a patch bump back to `main`, so there is no release-commit loop and no extra write permission for the metadata/build jobs.

**To bump minor or major manually:** edit the root `Cargo.toml` to the desired `M.N.0`, refresh its package entry in `Cargo.lock` with `cargo update --workspace`, and commit both files. Update `DISPLAY_VERSION` separately if the human-facing beta label should follow the new series. Do not use the numeric crate version as a release counter edited by CI.

Inspect the derived version locally with:

```sh
python3 scripts/release_version.py
```

CI passes its single computed value as **`FELINAUT_RELEASE_VERSION`** to both native builds, packaging, assembly, and publication. `build.rs` embeds it as `FELINAUT_BUILD_VERSION`; About, `CFBundleShortVersionString`, `CFBundleVersion`, all ZIP filenames, the release title and the `vM.N.P` tag agree. Overrides must be canonical numeric `M.N.P`, match Cargo's major/minor, and meet its patch floor. Local builds/source archives without an override keep the base Cargo version; local DMG filenames retain their marketing-label convention. The existing fixed `0.1 beta` marketing label remains separate from the effective numeric release version.

About also shows a 12-character Git hash, embedded at compile time. `build.rs` watches Git's HEAD, current branch reference, packed references, and the release-version environment setting so subsequent builds refresh metadata. Source archives without Git metadata, or builds without Git installed, display `unknown` instead of a hash. There are no runtime Git calls. The hash identifies the source commit, not uncommitted local edits; it is not a clean-working-tree guarantee.

## Browser identity

Pages see Felinaut as the Safari installed on the Mac, which uses the same system WebKit engine. The user agent is WebKit's own (`Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)`), followed by `Version/<version> Safari/605.1.15`. The version is the one Safari itself sends: `major.minor`, plus the patch level when there is one (`Version/26.6.2` for Safari 26.6.2).

- **Where the version comes from:** `/Applications/Safari.app/Contents/Info.plist`, read once at launch, before the first window opens (at most 256 KB). If that file can't be read, the macOS version gives an approximate one: the same version from macOS 26 on, and the macOS major version plus three for macOS 11 to 15 (Safari 14 to 18). It is approximate because Safari can be updated separately from macOS.
- **Where it applies:** every page Felinaut opens, including the hidden pages that refresh live folders.

**Why:** without the Safari tokens, sites treat Felinaut as an unknown embedded web view. Google, for example, serves an old, stripped-down results page with no **Sign in** button.

**What doesn't change:** Safari-only features such as Apple Pay and passkeys still need Apple's entitlements (see Passkeys below). Sites may offer them and then find them unavailable. The code is in `src/identity.rs`.

**Embedded-browser checks are bypassed too.** Some sign-in flows, Google's among them, deliberately look for embedded web views and refuse or reroute them. Felinaut no longer looks like one, so those flows now run in a setup the site does not officially support: they may work, or break in ways the site does not expect.

## Passkeys

**Today:** passwords, one-time codes and every other sign-in that runs inside the page work. Passkeys do not: WebKit refuses WebAuthn in an embedded `WKWebView` unless the app carries Apple's restricted browser entitlement. Measured in a dev-signed Felinaut build on macOS 26: `navigator.credentials` and `PublicKeyCredential` exist on https://webauthn.io, but `isUserVerifyingPlatformAuthenticatorAvailable()` returns `false` and both `create()` and `get()` reject immediately with `NotAllowedError: The request is not allowed by the user agent or the platform in the current context`. No passkey sheet appears. There is no JavaScript workaround: the passkey UI and the iCloud Keychain access belong to WebKit's own process.

**What unlocks it:** the `com.apple.developer.web-browser.public-key-credential` entitlement ([Apple docs](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.web-browser.public-key-credential), macOS 13.3+). It is a _managed capability_: Apple grants it per team, it must also be listed in a provisioning profile embedded in the app, and macOS kills at launch any non-App-Store app whose signature claims it without such a profile (the packager therefore never adds it on its own). The iOS default-browser entitlement `com.apple.developer.web-browser` does not exist on macOS and is not used.

Request steps (once, by the **Account Holder** of the Apple Developer team that owns the Developer ID certificate):

1. Ship a build that meets Apple's browser criteria: `http`/`https` in `CFBundleURLTypes` (done; Felinaut can be chosen as the default browser in **System Settings → Desktop & Dock** and opens links handed over by macOS), an address bar on launch, and direct navigation to the requested URL.
2. Fill in the [macOS Browsers Passkeys request form](https://developer.apple.com/contact/request/macos-browsers-passkeys/) while signed in as the Account Holder. Apple reviews it against the criteria above and, if accepted, adds the capability to the team.
3. In **Certificates, Identifiers & Profiles → Identifiers**, open the App ID `io.github.casaroli.felifox`, enable the _Web Browser Public Key Credential_ capability, then create a **Developer ID Application** provisioning profile for that App ID (Profiles → + → Developer ID) and download it.
4. Package with the profile. The packager embeds it as `Contents/embedded.provisionprofile`, checks that it actually grants every key in `assets/Felinaut.entitlements`, and signs with those entitlements:

```sh
FELINAUT_PROVISIONING_PROFILE=~/Downloads/Felinaut.provisionprofile \
FELINAUT_SIGN_IDENTITY="Developer ID Application: <name> (<team-id>)" \
FELINAUT_NOTARY_PROFILE=felinaut-notary cargo dist
```

`FELINAUT_PROVISIONING_PROFILE` requires `FELINAUT_SIGN_IDENTITY`; a Developer ID profile does not match an _Apple Development_ certificate. Without the variable nothing changes: the app is signed exactly as before.

**Testing:** open https://webauthn.io, enter any user name and click **Register**; macOS should show its passkey sheet (Touch ID or the account password) and the site should then **Authenticate** with the new passkey. For Gmail, open https://accounts.google.com, enter the address and choose **Try another way → Use your passkey** (or let the conditional-UI prompt appear in the address field) with a passkey that already exists in iCloud Keychain. Until the entitlement is granted both report that no passkey is available; the Google sign-in page falls back to the password.

## Controls

- Native address input with Unicode, IME, selection and clipboard.
- **Title-first address bar**: while you are not editing it, the address field shows the site's icon, the page title and the domain instead of the raw URL. Click it or press `⌘L` to get the full URL, selected for editing; clicking into the page ends the edit. **Theme ▾ → Address bar shows** switches between _Page title_, _Domain only_ and _Full address_.
- **History suggestions** while you type: a dropdown of previously visited pages, each with its icon, matched fuzzily against both title and URL; the first address containing your text is completed inline, and each row's × forgets that page. An empty address bar lists your most frecent pages (see below).
- Tabs retain their own live webview and native navigation history when switched.
- **Vertical tabs** in a collapsible sidebar (`⌘S`) that slides in and out (instantly with Reduce Motion or animations off). While it is hidden, touching the window's left edge peeks it, like Arc; it slides away once the pointer moves past it, and `⌘S` or its toggle keeps it open. Press `⌘D` (or drag a tab onto the tiles) to pin it: pinned tabs become favicon tiles, three per row, above the other tabs. **All tabs persist across launches** in `tabs.tsv` under Application Support (order, pins, split views and the active tab; blank New Tabs are dropped). Pinned tabs reopen their pinned URL, the others the page they were showing; each loads when first selected. `⌘W` on a pinned tab frees its page and resets it to the pinned URL; `⌘D` or dragging it out of the tiles unpins. Hovering a tab or its controls describes them in the status bar. Right-click a tab or tile for **Pin / Unpin**, **Show side by side with…** (split view), **End Split View** and **Close**. Drag tabs to reorder them: a line shows where the tab will go. Drop a tab onto the middle of another to open the two **side by side** (split view); click either half to make it active, and drag a half out of the row to end the split. Dropping onto a tile (or the tile grid) pins, onto **New tab** moves it to the top. Drag the sidebar's right edge to resize it (200–400 pt, saved).
- **Live folders** in the sidebar, Arc-style, from premade integrations. The **Live folders** header is always there: click it (**Edit**, or **File → New Live Folder…**) to add or remove an integration and choose its lists. **GitHub** shows your open pull requests in **Review requests**, **My pull requests** and **Mentions**; **Asana** shows **Due this week** (overdue included, soonest first) and **My tasks** (incomplete tasks assigned to you, first workspace only). Each list can be turned off; disabling all lists shows a settings hint and pauses refreshes. Asana is read from its own API (`app.asana.com/api/1.0`), which accepts the web session on a page load; GitHub from its pull request pages. Asana follows pagination before filtering and sorting (My tasks is most recently modified first), bounded to 8 task pages, 2 MiB of response/state data and 18 seconds. If that limit is reached or a refresh fails, cached items remain and the list shows an error rather than a partial result; API errors such as rate limits and permission failures do not pretend you are signed out. There is no token: the lists load in a hidden, offscreen webview that shares WebKit's storage, so they use the sessions you are signed in to in Felinaut. When a folder is added, its first refresh checks the sign-in. If the site asks you to sign in, the folder shows **Sign in to GitHub** (or Asana); click it to sign in in a normal tab, and the folder fills in as soon as that tab loads the site again. **Items are tabs that live in the folder**: click one to show it (it stays in the folder, highlighted like a selected tab), and close it with its × or `⌘W`. A closed item stays away until the site reports an update to it, then it returns with a dot. Missing timestamps preserve its last known update and dismissal; a first timestamp establishes a baseline, not an update. Explicitly reopening an item through agent control restores its sidebar entries. Merged and closed pull requests leave the folder, and so do their tabs (the one you are looking at stays, as an ordinary tab). New and updated pull requests (any activity GitHub reports: comments, reviews, pushes, CI) carry a dot until you open them; the folder's badge counts them, and so does a collapsed section's. Click the folder or a section to collapse it. Right-click the folder for **Refresh**, **Mark All as Seen** and **Remove**. Lists refresh every 5 minutes, one at a time and only while the window is active; at most 50 items each, only links on the site's own host. Kept in `live.tsv` under Application Support; item tabs are not restored after a restart. Agent control can drive them too: `felinaut-ctl live`, `live add github|asana`, `live open 0 0 1`, `live close 0 0 1`, `live refresh 0`, `live remove 0`.
- Back, forward, reload and home; native page titles and URLs update the chrome.
- **Error page:** when a page cannot load (offline, unknown site, refused or lost connection, timeout, certificate problem, redirect loop, or a port WebKit blocks such as `:1`), the tab shows what went wrong, the address and WebKit's error code instead of an endless spinner. **Try Again** or `⌘R` loads that address again, and Back returns to the page you were on (or Home). For an expired, self-signed, untrusted or wrong-name certificate, **Visit this site anyway** accepts that exact certificate for that site until Felinaut quits (nothing is saved); the toolbar then shows **⚠ Not secure**. Unlike Chrome, this cannot be refused for HSTS sites, because WebKit does not expose that list. A cancelled load shows nothing, and one that becomes a download goes to the downloads list (see below); a load that fails after the page appeared only stops the spinner.
- Hovering web links displays their destination in the status bar; leaving clears it.
- Cmd-click opens a link in a background tab; Cmd-Shift-click opens it in the foreground. New Tab quick links support the same modifiers.
- Right-click a web link for **Open Link in New Tab** (background). The document-local menu supports Enter, Escape, and outside-click dismissal; non-link right clicks keep WebKit's normal menu.
- New-window links open as tabs instead of unmanaged native windows.
- Native menus keep browser shortcuts available while the webview has focus.
- **Per-tab volume** from 0 % to 600 %, with mute and sound presets (see below).
- **Downloads** are saved to `~/Downloads` and listed under the toolbar's **↓** button, with progress, cancel, open and show in Finder (see below).

| Shortcut         | Action                                                          |
| ---------------- | --------------------------------------------------------------- |
| `⌘T`             | New tab                                                         |
| `⌘W`             | Close active tab                                                |
| `⌘⇧T`            | Reopen the last closed tab (back in its split view if possible) |
| `⌘L`             | Focus address and select all                                    |
| `⌘R`             | Reload                                                          |
| `⌘+` / `⌘-`      | Zoom in / out; the address bar shows the level, click it to reset |
| `⌘[` / `⌘]`      | Back / forward                                                  |
| `⌘⇧H`            | Home                                                            |
| `⌘⇧M`            | Mute / unmute active tab                                        |
| `⌘⇧J`            | Show / hide the downloads list                                  |
| `⌘S`             | Show / hide the tab sidebar (remembered)                        |
| `⌘D`             | Pin / unpin active tab                                          |
| `⌘1`–`⌘8` / `⌘9` | Go to tab 1–8 / last tab                                        |
| `⌃Tab` / `⌃⇧Tab` | Next / previous tab (also `⌘⇧]` / `⌘⇧[`)                        |
| `⌘,`             | Open Settings                                                   |
| `⌘Q`             | Quit                                                            |

### Title-first address bar and history suggestions

The idle address field reads like Arc's or Safari's: **icon · page title · domain**. The domain stays visible on purpose, since hiding it is what drew the criticism when Chrome trialled a domain-only bar, and the full URL is one click or `⌘L` away. Tabs show the same icon. Prefer something else? The Theme ▾ dropdown's **Address bar shows** setting offers _Page title_ (default), _Domain only_ (Safari-style) or _Full address_ (the editable URL at all times, Chrome-style); the choice is saved with the theme.

Icons come from the page itself: a document-start script reads the site's `<link rel="icon">` (or `/favicon.ico`), draws it onto a 32 × 32 canvas and hands the native side a small PNG. Only same-origin or CORS-readable icons can be read, and strict `img-src` policies are handled by decoding the blob directly. Native code validates the PNG (signature, dimensions, 64 KB cap) before it is drawn, and keeps it as `history/icons/<host>.png`. Sites without a readable icon get a themed monogram of their host instead.

Every completed HTTP(S) page load is remembered with its title, visit count and last visit. Focusing an empty address bar lists up to eight of your most frecent pages; typing shows up to eight matching pages beneath it, like Chrome, Firefox or Safari:

- Matching is case-insensitive and fuzzy: contiguous text ranks highest, with a bonus for starting the host (`rust` → **rust**-lang.org) or a word; scattered subsequences (`hkrn` → **H**ac**k**e**r** **N**ews) still match. Every space-separated word must match the title or the URL. Equal matches are ordered by frecency: more visits and more recent visits first.
- Each row shows the title and the URL with the matched characters highlighted.
- Typing completes the first row whose address contains the text, drawn dimmed around what you typed: `le` shows `goog`**le**`.com`, `lang.org` shows `rust-`**lang.org**. `Enter` opens that page, `→` at the end of the text accepts the completion into the field, and editing keeps working on just what you typed. Rows that only match by title, multi-word queries and edits mid-text are never completed.
- `↓` / `↑` move through the rows and put the selected URL into the address bar; stepping past the end restores what you typed. `Enter` opens the selected row, or, with nothing selected, resolves the typed text as usual (URL, host, or a search with your search engine). `Esc` closes the list and restores the typed text; clicking a row opens it.
- Hovering a row reveals an **×** at its right edge that forgets the page: it leaves history on disk as well, and the list refills from what you typed.

History lives in `~/Library/Application Support/Felifox/history/visits.tsv`, one tab-separated line per URL (last visit as unix seconds, visit count, URL, title), newest first, with site icons beside it in `history/icons/`. Delete the folder to clear both. The GPUI Home page, `about:` pages and failed loads are not recorded; the file is capped at 10 000 pages, dropping the least recently visited.

Home is a GPUI page outside WebKit's history. Going back from the first web page shows Home; Forward restores the retained web page. Closing a tab destroys its webview. Opening a new tab does not allocate a webview until you navigate.

## Agent control

An agent that can run shell commands can drive Felinaut. This is off by default.

**Caution:** each process of your user that connects can use your logged-in sessions. Start Felinaut with agent control only when you need it.

```sh
FELINAUT_AGENT=1 cargo run                 # opens the control socket
cargo run --bin felinaut-ctl -- snapshot   # or: target/debug/felinaut-ctl snapshot
```

| Command                                             | Result                                                                            |
| --------------------------------------------------- | --------------------------------------------------------------------------------- |
| `tabs`, `tab <index>`                               | List the tabs, or select one                                                      |
| `navigate <url>`                                    | Load a page in the active tab                                                     |
| `snapshot`                                          | List the visible elements that accept input, each with a reference such as `@e12` |
| `click <@ref>`, `type <@ref> <text>`, `press <key>` | Operate the page                                                                  |
| `eval <expression>`                                 | Evaluate one JavaScript expression                                                |
| `screenshot [path]`                                 | Save WebKit's own PNG snapshot of the page                                        |
| `downloads`, `downloads show`, `downloads cancel <id>` | List the downloads (name, state, bytes, path), open or close the list, or stop a running download |

`click`, `type` and `press` send synthetic JavaScript events by default. Some sites ignore these events (`isTrusted` is false). Add `--native` to send a real mouse or keyboard event to the WKWebView.

The socket is `~/Library/Application Support/Felifox/agent.sock`. Each request is one JSON line with the token from `agent.token` (a new random token for each start). Both files are readable only by your user. The commands operate the active tab, in the main frame only.

## Tab volume

Every tab has its own volume, built in rather than added by an extension. Click the speaker button left of **Theme ▾** (or **Sound → Volume Controls…**) to open the popover:

- **Slider from 0 % to 600 %.** The left half covers 0–100 % for fine control; the right colored half is the boost range. It snaps to the page's natural 100 %, and the knob is the astronaut-cat helmet. Drag, click, scroll, or use the **−/+** and **200 %/400 %** chips.
- **Mute / Unmute**, and **Reset** back to the untouched default.
- **Sound presets:** _Natural_ (as the site made it), _Voice_ (clearer dialogue) and _Bass_ (warmer low end).
- **Tabs with sound** lists every tab that is playing or has custom settings; click a row to switch to it, or its speaker to mute it.
- Keyboard while the popover is open: `←`/`→` (or `↓`/`↑`) adjust in 5 % steps below 100 % and 25 % steps above, `M` mutes, `0` resets, `Esc` closes.

Tabs that are playing sound or muted show a speaker in the sidebar; clicking it toggles mute. The toolbar button shows the active tab's level or "Muted". Settings belong to the tab, survive navigation within it, and last for the session only.

How it works: a document-start script (`src/audio_control.js`) runs in every frame. While a tab uses the default settings, it never touches page media. Once you change volume or sound, playing media is routed through Web Audio (EQ shelves, gain, and a limiter when boosting), and page-created `AudioContext`s feed the same chain. Native code only sends a validated gain and preset name, and pages report two booleans back, so the bridge exposes no new native capability.

Limitation: media served from another origin without CORS would go silent in Web Audio, so it is only lowered or muted through its `volume` (no boost or EQ). The popover says so when this applies. Audio from plugins, WebRTC streams and offline rendering is not boosted.

## Downloads

A download link, or a response WebKit cannot show (an attachment, an archive…), is saved to `~/Downloads`. Nothing asks where to put it, and nothing is ever overwritten: a second `report.pdf` becomes `report (1).pdf`. The name comes from the server, so Felinaut treats it as untrusted: only its last path component is used, without control or text-direction characters, leading dots or colons, cut to a length the disk accepts. A file can therefore never land outside `~/Downloads`. Pages you cannot see (the readers behind live folders) cannot download at all.

The **↓** button left of the speaker (or **View → Downloads**, `⌘⇧J`) opens the list, newest first:

- Each row shows the **file name**, the **host** it came from and its state: bytes so far with a progress bar while running, the size once done, or _Failed_, _Cancelled_ or _Interrupted_. When the server does not say how large the file is, the row shows the bytes received and a half-lit bar instead of an invented percentage.
- **Cancel** stops a running download. **Open** opens a finished file with its default app, **Show in Finder** reveals it, and **Remove** takes the row off the list without touching the file. **Clear finished** removes every row that is not running.
- Keyboard while the list is open: `↑`/`↓` select a row, `↩` opens the file, `⌘↩` shows it in Finder, `⌫` cancels a running download or removes a finished row, `Esc` closes.
- While something is downloading, the toolbar button turns to the theme's primary color and shows a small bar: the overall progress, or half-lit when a size is unknown.

The list is kept in `downloads.tsv` (see _Scope and limitations_) and survives restarts; a download that was still running when Felinaut quit comes back as _Interrupted_. A cancelled or failed download's partial file is deleted, because WebKit writes it under the final name where it would pass for the whole file. An interrupted download's partial file stays in `~/Downloads`.

How it works: Wry's download callbacks report neither progress nor the download object, so Felinaut registers its own `WKDownloadDelegate` (`src/downloads/native.rs`) by replacing the two `didBecomeDownload` methods of Wry's navigation delegate. Byte counts are read from the download's `NSProgress` on the UI's 100 ms drain. WebKit marks the files with the macOS quarantine attribute.

Limitations: there is no pause or resume, no "ask where to save", no per-site folder, and no retry; start the download again from its page. Felinaut does not check whether a listed file was moved or deleted afterwards, so **Open** and **Show in Finder** then do nothing. A download stops when Felinaut quits.

## Artwork and appearance

The toolbar's **Theme ▾** button opens six curated palettes: **Milky Way** (the new-profile default), Midnight Party, Confetti Sunset, Neon Wool, Alpine Pop, and Forest Carnival. Existing saved selections keep their original theme keys. Themes recolor Felinaut's chrome, address field, New Tab page and About window—not websites. Theme, animation and address-bar choices are saved to `~/Library/Application Support/Felifox/settings` (`key=value` lines, including sidebar choices and `search=` for the search engine) and restored on launch.

Address-bar searches use **DuckDuckGo** by default. Choose another in **Felinaut → Settings…** (`⌘,`), under **Search engine**; the change applies to your next search. The other choices are **Google**, **Bing**, **Brave Search**, **Ecosia** and **Startpage**; the choice is saved as `search=duckduckgo|google|bing|brave|ecosia|startpage`. The list is fixed: only the encoded query varies, so a saved value cannot send searches to any other site. A missing, unknown or edited value falls back to DuckDuckGo, and older settings files keep working.

**Website language** (also in Settings) is **Same as macOS** by default, or **English** or **Français**, saved as `language=system|en|fr`. Many sites, Google among them, pick a language from your location rather than your browser, so a chosen language works in two ways:

- **Searches** carry the engine's own language parameter right away: `hl=` for Google, `setlang=` for Bing, `lang=` for Brave Search and `kad=` for DuckDuckGo. Ecosia and Startpage have none and follow the browser language below. The query stays the only other parameter.
- **All websites** receive it as their `Accept-Language` header and `navigator.language` after you restart Felinaut; Settings says when a restart is pending. At launch Felinaut writes the choice to its own `AppleLanguages` default (never the system-wide one), which WebKit reads when it starts; **Same as macOS** removes it again. Felinaut's own interface is English either way.

Sites that decide by location alone may still ignore the browser language. Cookie-consent pages appear once per site regardless of language.

- **Original embedded SVG artwork:** a happy orange-and-cream astronaut cat exploring colorful yarn planets, a simplified helmet symbol, and a floating cardboard exploration box on New Tab. She has one clear smile, a cyan bubble visor, and a white/lavender suit. No fox emoji, external assets or large background bitmaps.
- **Six distinct procedural scenes:** Milky Way has layered galactic nebulae, subtle stars, moons and cross-wound yarn planets. All themes share restrained orbital threads and wool-planet discoveries; Midnight Party has tumbling confetti, streamers, golden stars and neon ribbons; Confetti Sunset has a golden sun, peach/coral hills and falling confetti; Alpine Pop has snowcapped ice mountains, glacier facets, drifting snow and ridge glints; Neon Wool has rolling yarn balls and flowing loose threads; Forest Carnival has swaying foliage, citrus, cherries, flowers and fluttering petals.
- **Visible native animation:** backgrounds are drawn from responsive GPUI paths, with perceptible movement within a second rather than tiny nearly-static color shifts. The mascot also gently bobs; no background image frames or new animation dependencies are used.
- **Motion:** the dropdown includes an Animate artwork checkbox. Animation stops while the window is inactive or web content replaces New Tab, and follows the live macOS Reduce Motion setting. Active animation is limited to about 30 fps; pausing does not affect website animation.
- **About:** open **Felinaut → About Felinaut**, click the toolbar's **ⓘ**, or use the About link on New Tab. The independently closable window displays the **beta label, numeric version and build commit**, the mascot, and Rust / GPUI / WebKit credits. Escape or Cmd-W closes About, not the browser.

The theme chooser is a small native popup, so it appears correctly above the embedded WebKit view without hiding or reloading the page. The volume popover and the address bar's history dropdown use the same approach; the dropdown is opened without keyboard focus so typing continues in the address bar. The artwork is original vector work inspired by the supplied design direction, not a copy of the reference illustrations.

## Architecture

```text
GPUI: tabs, address bar, controls, new-tab page
                     │
              Wry native child view
                     │
         macOS WebKit + JavaScriptCore
```

Two small document-start scripts run in every frame: one for links, one for tab audio. A third runs in the main frame only and reports the site icon as a bounded PNG data URL. The link script supplies hover destinations, modifier-click requests and the link menu, including inside frames. Its narrowly scoped native message handler accepts only bounded HTTP(S) URLs; it exposes no filesystem or general native-command API. The link menu uses an isolated shadow root within the page because a GPUI overlay would be covered by WebKit's native view. Web pages remain untrusted and can interfere with document-local UI.

A GPUI canvas supplies the webview's window-relative logical bounds so it stays below the toolbar and above the status bar during resizing. Hidden tabs keep their documents and JavaScript state. Native callbacks enqueue events, which the UI drains every 100 ms; they never re-enter GPUI while it is rendering. That drain also records finished loads and same-document URL changes in the in-memory history; the file is rewritten atomically (temporary file, then rename) on a background thread at most once a second and once more when the window closes, so the UI thread never waits on disk.

The previous WebRender/SWGL pipeline, HTML extraction, custom HTTP loader and `--render-check` command have been removed. WebKit now owns the entire web-content pipeline.

## Scope and limitations

Keep it small: no bookmarks manager, extensions, permission UI, tracker-blocker promise, or private-mode promise. Felinaut persists browsing history and site icons (`~/Library/Application Support/Felifox/history/`, delete it to clear them), Appearance, search engine and website language choices (`…/Felifox/settings`, plus the app's own `AppleLanguages` default when a website language is chosen), saved tabs (`…/Felifox/tabs.tsv`), live folders (`…/Felifox/live.tsv`) and the downloads list (`…/Felifox/downloads.tsv`: state, start time, size, source host and file path of at most 200 downloads, never the download URL; delete it to clear the list, the files stay). Volume choices last for the session only. WebKit's normal website storage is enabled; cookies and local storage may persist independently of the in-memory tab list.

- Compatibility is **WebKit compatibility**, not Chromium compatibility. Websites requiring specific browser integrations, DRM, permissions, or custom URL schemes may still fail.
- Top-level navigation permits HTTP(S) and `about:` pages; file, data, JavaScript and external-app schemes are blocked. This is not a general request filter: WebKit manages subresources and web APIs.
- This is an early browser shell, not a security-audited Safari replacement. WebKit's engine security updates come from macOS updates.
- Only macOS is configured and tested. Windows and Linux backends are not configured.

## Validate

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
python3 -m unittest discover -s scripts -p 'test_*.py'
# Opt-in native integration test (opens a temporary window, loopback traffic only):
FELINAUT_NATIVE_TEST=1 cargo test --test native_webview
# Native artwork, theme-handler and About-window checks:
FELINAUT_NATIVE_TEST=1 cargo test --test brand_ui
# Per-tab audio in real WebKit (plays a quiet tone from two loopback origins):
FELINAUT_NATIVE_TEST=1 cargo test --test native_audio
```

Add `-F gpui/runtime_shaders` to these commands on machines without full Xcode.

The native webview test verifies actual WebKit JavaScript execution, DOM title changes, localStorage, native history, URL polling of a tab whose host never resolved, and injected DOM-event handling for hover, Cmd-click, middle-click and the right-click menu. It also downloads a loopback attachment whose name points outside the downloads folder (into a temporary home, never the real `~/Downloads`) and checks that the file stays inside it and that a hidden page's download is refused. The brand UI test also verifies that the actual artwork timer advances when enabled and stops when disabled (or when macOS Reduce Motion applies). Geometry tests cover distinct scenes, tessellation and changes over time for all six themes. Native tests use programmatically dispatched DOM events or shared UI handlers, not physical mouse automation. It is skipped by default because WebKit needs a macOS desktop session and main-thread event loop.

## Source

- `src/main.rs`: GPUI window, tabs and navigation controls.
- `src/input.rs`: address input adapted from Zed's Apache-2.0 GPUI input example.
- `src/engine.rs`: address-to-URL/search resolution and the fixed list of search engines.
- `src/identity.rs`: the Safari-compatible user agent every page sends.
- `src/history.rs`: on-disk visit store, fuzzy matcher and frecency ranking.
- `src/suggestions.rs`: the native, non-focusing history dropdown under the address bar.
- `src/icons.rs`, `src/page_icon.js`: site icons handed over by the page as validated PNGs, stored per host, with monogram fallback.
- `src/webview.rs`: Wry child views, native callbacks, history, bounds and focus.
- `src/theme.rs`: shared palettes, contrast checks, native Reduce Motion support and the saved Appearance settings.
- `src/art.rs` and `src/art/`: theme dispatch and six procedural scenes with shared orbital/yarn geometry; no textures or animation framework.
- `src/link_interactions.js`: document-level link status, modifier-click and accessible context-menu handling.
- `src/audio.rs`, `src/audio_control.js`: per-tab volume model, slider mapping, the audio bridge and Web Audio routing.
- `src/volume.rs`: the native volume popover.
- `src/brand.rs` and `assets/`: embedded original SVG mascot, symbol and monochrome UI icons.
- `src/appearance.rs`: native theme dropdown and animation control.
- `src/about.rs`, `build.rs`: themed About window and compile-time Git build identity.
- `src/settings.rs`: the Settings window (`⌘,`): search engine and website language.
- `src/language.rs`: the website language choice and how it reaches WebKit at launch.
- `tests/native_webview.rs`: opt-in native JavaScript integration test.
- `tests/brand_ui.rs`: opt-in SVG decode, theme-handler, motion-toggle and About lifecycle checks.
- `tests/native_audio.rs`: opt-in WebKit check of audio routing, cross-origin fallback and reset.

Felinaut source is MIT licensed. Dependencies retain their own licenses, including GPUI (Apache-2.0) and Wry (Apache-2.0 OR MIT).
