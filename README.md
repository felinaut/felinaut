# Felifox · 0.1 beta

A simple Rust desktop browser: **GPUI for the interface, Wry / WKWebView for the web**. On macOS, WebKit handles HTML, CSS, JavaScript, images, forms, networking and web storage. There is no bundled browser engine and no custom page renderer.

## Run

Currently targets **macOS**. Requirements:

| Requirement | Why | Install |
| --- | --- | --- |
| Rust 1.94+ (edition 2024) | Compiles Felifox | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Xcode (full app, with its Metal toolchain) | GPUI compiles its Metal shaders at build time via `xcrun metal` | App Store, then `sudo xcode-select -s /Applications/Xcode.app` and `xcodebuild -downloadComponent MetalToolchain` |
| Python 3.11+ | `cargo dist` packaging only | `brew install python@3.13` |

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
./target/release/felifox
```

The first build downloads dependencies from crates.io. The new-tab page contains no remote assets or telemetry. Search queries go to DuckDuckGo. Websites control their own requests once loaded.

## Build a DMG for friends

From the project root, run:

```sh
cargo dist
```

This runs a locked, optimized release build from the current source, creates and verifies the app bundle, then builds and verifies the illustrated drag-to-install DMG. Previous local artifacts are replaced automatically, so you can use the same command after every change.

Requires **Python 3.11+**, the Rust/Xcode tools above, and a macOS desktop session with Finder. No pip packages or third-party packaging utilities are needed. If prompted, allow your terminal/editor to control Finder in **System Settings → Privacy & Security → Automation** so the installer layout can be saved.

Outputs are generated in `dist/` (excluded from Git):

- **`Felifox.app`** — an optimized native app bundle with the custom cat-and-flaming-yarn `.icns` icon, bundle metadata, version, and license.
- **`Felifox-0.1-beta-macos-arm64.dmg`** on Apple Silicon, or **`Felifox-0.1-beta-macos-x86_64.dmg`** on an Intel build host.
- **`SHA256SUMS`** — checksum for the DMG.

Double-click the DMG, then **drag Felifox onto Applications**. The installer includes a high-resolution illustrated background, a directional arrow, correctly positioned icons, and a real `/Applications` shortcut. Its artwork is drawn at build time from the existing SVG mascot plus native AppKit gradients and paths.

**Send the `.dmg` file to your friends**, not the `.app` folder or the contents of `target/`. On this Apple Silicon Mac the file is `dist/Felifox-0.1-beta-macos-arm64.dmg`; testers need an Apple Silicon Mac for this build. Intel builds must be produced on an Intel host. There is no updater: send a newly generated DMG for each test build.

`cargo dist` uses a tiny dependency-free Rust launcher to run the locked release build **before** invoking the Python packager. Packaging is skipped if the build fails, and the packager reuses that freshly built executable rather than compiling twice. `make dist` delegates to the same command. Use `cargo dist --app-only` to produce just the app. For artwork-only packaging iterations, the lower-level script also accepts `--skip-build`, but that intentionally reuses an older executable and should not be used when distributing code changes.

**Signing:** by default packages have a verified **local ad-hoc signature**, which Gatekeeper blocks when downloaded onto another Mac. To produce a DMG that opens anywhere, sign with a **Developer ID Application** certificate and notarize:

```sh
xcrun notarytool store-credentials felifox-notary --apple-id <apple-id> --team-id <team-id>   # once; prompts for an app-specific password
FELIFOX_SIGN_IDENTITY="Developer ID Application: <name> (<team-id>)" FELIFOX_NOTARY_PROFILE=felifox-notary cargo dist
```

This signs the app (hardened runtime, secure timestamp) and the DMG, submits the DMG to Apple, staples the tickets, and checks both with `spctl`. Packaging targets the host architecture, not a universal binary. A macOS 12.0 minimum deployment target is configured, but compatibility has only been verified on the build Mac.

The build preserves Cargo's procedural-macro metadata (`CARGO_PROFILE_RELEASE_STRIP=none`) to avoid a stripping issue on the current toolchain, then strips only the finished copied executable before signing. This leaves the normal Cargo profile unchanged. Embedded SVGs and Metal shaders require no separate runtime asset downloads.

Packaging checks:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
codesign --verify --deep --strict dist/Felifox.app
hdiutil verify dist/Felifox-0.1-beta-macos-arm64.dmg
```

## Passkeys

**Today:** passwords, one-time codes and every other sign-in that runs inside the page work. Passkeys do not: WebKit refuses WebAuthn in an embedded `WKWebView` unless the app carries Apple's restricted browser entitlement. Measured in a dev-signed Felifox build on macOS 26: `navigator.credentials` and `PublicKeyCredential` exist on https://webauthn.io, but `isUserVerifyingPlatformAuthenticatorAvailable()` returns `false` and both `create()` and `get()` reject immediately with `NotAllowedError: The request is not allowed by the user agent or the platform in the current context`. No passkey sheet appears. There is no JavaScript workaround: the passkey UI and the iCloud Keychain access belong to WebKit's own process.

**What unlocks it:** the `com.apple.developer.web-browser.public-key-credential` entitlement ([Apple docs](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.web-browser.public-key-credential), macOS 13.3+). It is a *managed capability*: Apple grants it per team, it must also be listed in a provisioning profile embedded in the app, and macOS kills at launch any non-App-Store app whose signature claims it without such a profile (the packager therefore never adds it on its own). The iOS default-browser entitlement `com.apple.developer.web-browser` does not exist on macOS and is not used.

Request steps (once, by the **Account Holder** of the Apple Developer team that owns the Developer ID certificate):

1. Ship a build that meets Apple's browser criteria: `http`/`https` in `CFBundleURLTypes` (done; Felifox can be chosen as the default browser in **System Settings → Desktop & Dock** and opens links handed over by macOS), an address bar on launch, and direct navigation to the requested URL.
2. Fill in the [macOS Browsers Passkeys request form](https://developer.apple.com/contact/request/macos-browsers-passkeys/) while signed in as the Account Holder. Apple reviews it against the criteria above and, if accepted, adds the capability to the team.
3. In **Certificates, Identifiers & Profiles → Identifiers**, open the App ID `io.github.casaroli.felifox`, enable the *Web Browser Public Key Credential* capability, then create a **Developer ID Application** provisioning profile for that App ID (Profiles → + → Developer ID) and download it.
4. Package with the profile. The packager embeds it as `Contents/embedded.provisionprofile`, checks that it actually grants every key in `assets/Felifox.entitlements`, and signs with those entitlements:

```sh
FELIFOX_PROVISIONING_PROFILE=~/Downloads/Felifox.provisionprofile \
FELIFOX_SIGN_IDENTITY="Developer ID Application: <name> (<team-id>)" \
FELIFOX_NOTARY_PROFILE=felifox-notary cargo dist
```

`FELIFOX_PROVISIONING_PROFILE` requires `FELIFOX_SIGN_IDENTITY`; a Developer ID profile does not match an *Apple Development* certificate. Without the variable nothing changes: the app is signed exactly as before.

**Testing:** open https://webauthn.io, enter any user name and click **Register**; macOS should show its passkey sheet (Touch ID or the account password) and the site should then **Authenticate** with the new passkey. For Gmail, open https://accounts.google.com, enter the address and choose **Try another way → Use your passkey** (or let the conditional-UI prompt appear in the address field) with a passkey that already exists in iCloud Keychain. Until the entitlement is granted both report that no passkey is available; the Google sign-in page falls back to the password.

## Controls

- Native address input with Unicode, IME, selection and clipboard.
- **Title-first address bar**: while you are not editing it, the address field shows the site's icon, the page title and the domain instead of the raw URL. Click it or press `⌘L` to get the full URL, selected for editing; clicking into the page ends the edit. **Theme ▾ → Address bar shows** switches between *Page title*, *Domain only* and *Full address*.
- **History suggestions** while you type: a dropdown of previously visited pages, each with its icon, matched fuzzily against both title and URL; the first address containing your text is completed inline, and each row's × forgets that page. An empty address bar lists your most frecent pages (see below).
- Tabs retain their own live webview and native navigation history when switched.
- **Vertical tabs** in a collapsible sidebar (`⌘S`) that slides in and out (instantly with Reduce Motion or animations off). While it is hidden, touching the window's left edge peeks it, like Arc; it slides away once the pointer moves past it, and `⌘S` or its toggle keeps it open. Press `⌘D` (or drag a tab onto the tiles) to pin it: pinned tabs become favicon tiles, three per row, above the other tabs. **All tabs persist across launches** in `tabs.tsv` under Application Support (order, pins, split views and the active tab; blank New Tabs are dropped). Pinned tabs reopen their pinned URL, the others the page they were showing; each loads when first selected. `⌘W` on a pinned tab frees its page and resets it to the pinned URL; `⌘D` or dragging it out of the tiles unpins. Hovering a tab or its controls describes them in the status bar. Right-click a tab or tile for **Pin / Unpin**, **Show side by side with…** (split view), **End Split View** and **Close**. Drag tabs to reorder them: a line shows where the tab will go. Drop a tab onto the middle of another to open the two **side by side** (split view); click either half to make it active, and drag a half out of the row to end the split. Dropping onto a tile (or the tile grid) pins, onto **New tab** moves it to the top. Drag the sidebar's right edge to resize it (200–400 pt, saved).
- **Live folders** in the sidebar, Arc-style, from premade integrations. GitHub is the first: **Add live folder** (or **File → New Live Folder…**) → **GitHub** adds a folder with your open pull requests in three sections: **Review requests**, **My pull requests** and **Mentions**. There is no token: the lists load in a hidden, offscreen webview that shares WebKit's storage, so they use the GitHub session you are signed in to in Felifox. When the folder is added, its first refresh checks the sign-in. If GitHub asks you to sign in, the folder shows **Sign in to GitHub**. Click it to sign in in a normal tab; the folder fills in as soon as that tab loads github.com again. Click the folder or a section to show or hide it. New pull requests carry a dot until you open them, and the folder's badge counts them. Right-click the folder for **Refresh**, **Mark All as Seen** and **Remove**. Sections refresh every 5 minutes, one at a time and only while the window is active; at most 50 items each, only github.com links. Kept in `live.tsv` under Application Support. Agent control can drive them too: `felifox-ctl live`, `live add github`, `live refresh 0`, `live remove 0`.
- Back, forward, reload and home; native page titles and URLs update the chrome.
- Hovering web links displays their destination in the status bar; leaving clears it.
- Cmd-click opens a link in a background tab; Cmd-Shift-click opens it in the foreground. New Tab quick links support the same modifiers.
- Right-click a web link for **Open Link in New Tab** (background). The document-local menu supports Enter, Escape, and outside-click dismissal; non-link right clicks keep WebKit's normal menu.
- New-window links open as tabs instead of unmanaged native windows.
- Native menus keep browser shortcuts available while the webview has focus.
- **Per-tab volume** from 0 % to 600 %, with mute and sound presets (see below).

| Shortcut    | Action                       |
| ----------- | ---------------------------- |
| `⌘T`        | New tab                      |
| `⌘W`        | Close active tab             |
| `⌘⇧T`       | Reopen the last closed tab (back in its split view if possible) |
| `⌘L`        | Focus address and select all |
| `⌘R`        | Reload                       |
| `⌘[` / `⌘]` | Back / forward               |
| `⌘⇧H`       | Home                         |
| `⌘⇧M`       | Mute / unmute active tab     |
| `⌘S`        | Show / hide the tab sidebar (remembered) |
| `⌘D`        | Pin / unpin active tab       |
| `⌘1`–`⌘8` / `⌘9` | Go to tab 1–8 / last tab |
| `⌃Tab` / `⌃⇧Tab` | Next / previous tab (also `⌘⇧]` / `⌘⇧[`) |
| `⌘Q`        | Quit                         |

### Title-first address bar and history suggestions

The idle address field reads like Arc's or Safari's: **icon · page title · domain**. The domain stays visible on purpose, since hiding it is what drew the criticism when Chrome trialled a domain-only bar, and the full URL is one click or `⌘L` away. Tabs show the same icon. Prefer something else? The Theme ▾ dropdown's **Address bar shows** setting offers *Page title* (default), *Domain only* (Safari-style) or *Full address* (the editable URL at all times, Chrome-style); the choice is saved with the theme.

Icons come from the page itself: a document-start script reads the site's `<link rel="icon">` (or `/favicon.ico`), draws it onto a 32 × 32 canvas and hands the native side a small PNG. Only same-origin or CORS-readable icons can be read, and strict `img-src` policies are handled by decoding the blob directly. Native code validates the PNG (signature, dimensions, 64 KB cap) before it is drawn, and keeps it as `history/icons/<host>.png`. Sites without a readable icon get a themed monogram of their host instead.

Every completed HTTP(S) page load is remembered with its title, visit count and last visit. Focusing an empty address bar lists up to eight of your most frecent pages; typing shows up to eight matching pages beneath it, like Chrome, Firefox or Safari:

- Matching is case-insensitive and fuzzy: contiguous text ranks highest, with a bonus for starting the host (`rust` → **rust**-lang.org) or a word; scattered subsequences (`hkrn` → **H**ac**k**e**r** **N**ews) still match. Every space-separated word must match the title or the URL. Equal matches are ordered by frecency: more visits and more recent visits first.
- Each row shows the title and the URL with the matched characters highlighted.
- Typing completes the first row whose address contains the text, drawn dimmed around what you typed: `le` shows `goog`**le**`.com`, `lang.org` shows `rust-`**lang.org**. `Enter` opens that page, `→` at the end of the text accepts the completion into the field, and editing keeps working on just what you typed. Rows that only match by title, multi-word queries and edits mid-text are never completed.
- `↓` / `↑` move through the rows and put the selected URL into the address bar; stepping past the end restores what you typed. `Enter` opens the selected row, or, with nothing selected, resolves the typed text as usual (URL, host, or DuckDuckGo search). `Esc` closes the list and restores the typed text; clicking a row opens it.
- Hovering a row reveals an **×** at its right edge that forgets the page: it leaves history on disk as well, and the list refills from what you typed.

History lives in `~/Library/Application Support/Felifox/history/visits.tsv`, one tab-separated line per URL (last visit as unix seconds, visit count, URL, title), newest first, with site icons beside it in `history/icons/`. Delete the folder to clear both. The GPUI Home page, `about:` pages and failed loads are not recorded; the file is capped at 10 000 pages, dropping the least recently visited.

Home is a GPUI page outside WebKit's history. Going back from the first web page shows Home; Forward restores the retained web page. Closing a tab destroys its webview. Opening a new tab does not allocate a webview until you navigate.

## Agent control

An agent that can run shell commands can drive Felifox. This is off by default.

**Caution:** each process of your user that connects can use your logged-in sessions. Start Felifox with agent control only when you need it.

```sh
FELIFOX_AGENT=1 cargo run                 # opens the control socket
cargo run --bin felifox-ctl -- snapshot   # or: target/debug/felifox-ctl snapshot
```

| Command | Result |
| --- | --- |
| `tabs`, `tab <index>` | List the tabs, or select one |
| `navigate <url>` | Load a page in the active tab |
| `snapshot` | List the visible elements that accept input, each with a reference such as `@e12` |
| `click <@ref>`, `type <@ref> <text>`, `press <key>` | Operate the page |
| `eval <expression>` | Evaluate one JavaScript expression |
| `screenshot [path]` | Save WebKit's own PNG snapshot of the page |

`click`, `type` and `press` send synthetic JavaScript events by default. Some sites ignore these events (`isTrusted` is false). Add `--native` to send a real mouse or keyboard event to the WKWebView.

The socket is `~/Library/Application Support/Felifox/agent.sock`. Each request is one JSON line with the token from `agent.token` (a new random token for each start). Both files are readable only by your user. The commands operate the active tab, in the main frame only.

## Tab volume

Every tab has its own volume, built in rather than added by an extension. Click the speaker button left of **Theme ▾** (or **Sound → Volume Controls…**) to open the popover:

- **Slider from 0 % to 600 %.** The left half covers 0–100 % for fine control; the right "flame" half is the boost range. It snaps to the page's natural 100 %, and the knob is the flaming-yarn mark. Drag, click, scroll, or use the **−/+** and **200 %/400 %** chips.
- **Mute / Unmute**, and **Reset** back to the untouched default.
- **Sound presets:** *Natural* (as the site made it), *Voice* (clearer dialogue) and *Bass* (warmer low end).
- **Tabs with sound** lists every tab that is playing or has custom settings; click a row to switch to it, or its speaker to mute it.
- Keyboard while the popover is open: `←`/`→` (or `↓`/`↑`) adjust in 5 % steps below 100 % and 25 % steps above, `M` mutes, `0` resets, `Esc` closes.

Tabs that are playing sound or muted show a speaker in the sidebar; clicking it toggles mute. The toolbar button shows the active tab's level or "Muted". Settings belong to the tab, survive navigation within it, and last for the session only.

How it works: a document-start script (`src/audio_control.js`) runs in every frame. While a tab uses the default settings, it never touches page media. Once you change volume or sound, playing media is routed through Web Audio (EQ shelves, gain, and a limiter when boosting), and page-created `AudioContext`s feed the same chain. Native code only sends a validated gain and preset name, and pages report two booleans back, so the bridge exposes no new native capability.

Limitation: media served from another origin without CORS would go silent in Web Audio, so it is only lowered or muted through its `volume` (no boost or EQ). The popover says so when this applies. Audio from plugins, WebRTC streams and offline rendering is not boosted.

## Artwork and appearance

The toolbar's **Theme ▾** button opens five curated palettes: Midnight Party (default), Confetti Sunset, Neon Wool, Alpine Pop, and Forest Carnival. Themes recolor Felifox's chrome, address field, New Tab page and About window—not websites. Theme, animation and address-bar choices are saved to `~/Library/Application Support/Felifox/settings` (three `key=value` lines) and restored on launch.

- **Original embedded SVG artwork:** a mischievous midnight-purple cat playing with a flaming wool ball, plus a simplified flaming-yarn symbol. No fox emoji, external assets or large background bitmaps.
- **Five distinct procedural scenes:** Midnight Party has tumbling confetti, streamers, golden stars and neon ribbons; Confetti Sunset has a golden sun, peach/coral hills and falling confetti; Alpine Pop has snowcapped ice mountains, glacier facets, drifting snow and ridge glints; Neon Wool has rolling yarn balls and flowing loose threads; Forest Carnival has swaying foliage, citrus, cherries, flowers and fluttering petals.
- **Visible native animation:** backgrounds are drawn from responsive GPUI paths, with perceptible movement within a second rather than tiny nearly-static color shifts. The mascot also gently bobs; no background image frames or new animation dependencies are used.
- **Motion:** the dropdown includes an Animate artwork checkbox. Animation stops while the window is inactive or web content replaces New Tab, and follows the live macOS Reduce Motion setting. Active animation is limited to about 30 fps; pausing does not affect website animation.
- **About:** open **Felifox → About Felifox**, click the toolbar's **ⓘ**, or use the About link on New Tab. The independently closable window displays **0.1 beta**, the mascot, and Rust / GPUI / WebKit credits. Escape or Cmd-W closes About, not the browser.

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

Keep it small: no bookmarks manager, extensions, download UI, permission UI, session restoration, or private-mode promise. Felifox itself persists only browsing history and site icons (`~/Library/Application Support/Felifox/history/`, delete it to clear them) and the Appearance choices (`…/Felifox/settings`). Tabs and volume choices still last for the session only. WebKit's normal website storage is enabled; cookies and local storage may persist independently of the in-memory tab list.

- Compatibility is **WebKit compatibility**, not Chromium compatibility. Websites requiring specific browser integrations, DRM, permissions, or custom URL schemes may still fail.
- Top-level navigation permits HTTP(S) and `about:` pages; file, data, JavaScript and external-app schemes are blocked. This is not a general request filter: WebKit manages subresources and web APIs.
- Wry's page-load callback does not expose native load failures. Some failed loads may leave the loading status displayed; Felifox does not yet provide a custom network-error page.
- This is an early browser shell, not a security-audited Safari replacement. WebKit's engine security updates come from macOS updates.
- Only macOS is configured and tested. Windows and Linux backends are not configured.

## Validate

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
# Opt-in native integration test (opens a temporary window, loopback traffic only):
FELIFOX_NATIVE_TEST=1 cargo test --test native_webview
# Native artwork, theme-handler and About-window checks:
FELIFOX_NATIVE_TEST=1 cargo test --test brand_ui
# Per-tab audio in real WebKit (plays a quiet tone from two loopback origins):
FELIFOX_NATIVE_TEST=1 cargo test --test native_audio
```

Add `-F gpui/runtime_shaders` to these commands on machines without full Xcode.

The native webview test verifies actual WebKit JavaScript execution, DOM title changes, localStorage, native history, URL polling of a tab whose host never resolved, and injected DOM-event handling for hover, Cmd-click, middle-click and the right-click menu. The brand UI test also verifies that the actual artwork timer advances when enabled and stops when disabled (or when macOS Reduce Motion applies). Geometry tests cover distinct scenes, tessellation and changes over time for all five themes. Native tests use programmatically dispatched DOM events or shared UI handlers, not physical mouse automation. It is skipped by default because WebKit needs a macOS desktop session and main-thread event loop.

## Source

- `src/main.rs`: GPUI window, tabs and navigation controls.
- `src/input.rs`: address input adapted from Zed's Apache-2.0 GPUI input example.
- `src/engine.rs`: address-to-URL/search resolution only.
- `src/history.rs`: on-disk visit store, fuzzy matcher and frecency ranking.
- `src/suggestions.rs`: the native, non-focusing history dropdown under the address bar.
- `src/icons.rs`, `src/page_icon.js`: site icons handed over by the page as validated PNGs, stored per host, with monogram fallback.
- `src/webview.rs`: Wry child views, native callbacks, history, bounds and focus.
- `src/theme.rs`: shared palettes, contrast checks, native Reduce Motion support and the saved Appearance settings.
- `src/art.rs` and `src/art/`: theme dispatch and five independent procedural scenes; no textures or animation framework.
- `src/link_interactions.js`: document-level link status, modifier-click and accessible context-menu handling.
- `src/audio.rs`, `src/audio_control.js`: per-tab volume model, slider mapping, the audio bridge and Web Audio routing.
- `src/volume.rs`: the native volume popover.
- `src/brand.rs` and `assets/`: embedded original SVG mascot, symbol and monochrome UI icons.
- `src/appearance.rs`: native theme dropdown and animation control.
- `src/about.rs`: themed About window showing 0.1 beta.
- `tests/native_webview.rs`: opt-in native JavaScript integration test.
- `tests/brand_ui.rs`: opt-in SVG decode, theme-handler, motion-toggle and About lifecycle checks.
- `tests/native_audio.rs`: opt-in WebKit check of audio routing, cross-origin fallback and reset.

Felifox source is MIT licensed. Dependencies retain their own licenses, including GPUI (Apache-2.0) and Wry (Apache-2.0 OR MIT).
