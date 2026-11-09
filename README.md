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

**Signing:** packages have a verified **local ad-hoc signature**, not a Developer ID signature or Apple notarization. Gatekeeper can warn or block when these are downloaded onto another Mac. Public distribution still requires an Apple Developer identity and notarization; the script does not disable macOS security settings. Packaging targets the host architecture, not a universal binary. A macOS 12.0 minimum deployment target is configured, but compatibility has only been verified on the build Mac.

The build preserves Cargo's procedural-macro metadata (`CARGO_PROFILE_RELEASE_STRIP=none`) to avoid a stripping issue on the current toolchain, then strips only the finished copied executable before signing. This leaves the normal Cargo profile unchanged. Embedded SVGs and Metal shaders require no separate runtime asset downloads.

Packaging checks:

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
codesign --verify --deep --strict dist/Felifox.app
hdiutil verify dist/Felifox-0.1-beta-macos-arm64.dmg
```

## Controls

- Native address input with Unicode, IME, selection and clipboard.
- Tabs retain their own live webview and native navigation history when switched.
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
| `⌘L`        | Focus address and select all |
| `⌘R`        | Reload                       |
| `⌘[` / `⌘]` | Back / forward               |
| `⌘⇧H`       | Home                         |
| `⌘⇧M`       | Mute / unmute active tab     |
| `⌘Q`        | Quit                         |

Home is a GPUI page outside WebKit's history. Going back from the first web page shows Home; Forward restores the retained web page. Closing a tab destroys its webview. Opening a new tab does not allocate a webview until you navigate.

## Tab volume

Every tab has its own volume, built in rather than added by an extension. Click the speaker button left of **Theme ▾** (or **Sound → Volume Controls…**) to open the popover:

- **Slider from 0 % to 600 %.** The left half covers 0–100 % for fine control; the right "flame" half is the boost range. It snaps to the page's natural 100 %, and the knob is the flaming-yarn mark. Drag, click, scroll, or use the **−/+** and **200 %/400 %** chips.
- **Mute / Unmute**, and **Reset** back to the untouched default.
- **Sound presets:** *Natural* (as the site made it), *Voice* (clearer dialogue) and *Bass* (warmer low end).
- **Tabs with sound** lists every tab that is playing or has custom settings; click a row to switch to it, or its speaker to mute it.
- Keyboard while the popover is open: `←`/`→` (or `↓`/`↑`) adjust in 5 % steps below 100 % and 25 % steps above, `M` mutes, `0` resets, `Esc` closes.

Tabs that are playing sound or muted show a speaker in the tab strip; clicking it toggles mute. The toolbar button shows the active tab's level or "Muted". Settings belong to the tab, survive navigation within it, and last for the session only.

How it works: a document-start script (`src/audio_control.js`) runs in every frame. While a tab uses the default settings, it never touches page media. Once you change volume or sound, playing media is routed through Web Audio (EQ shelves, gain, and a limiter when boosting), and page-created `AudioContext`s feed the same chain. Native code only sends a validated gain and preset name, and pages report two booleans back, so the bridge exposes no new native capability.

Limitation: media served from another origin without CORS would go silent in Web Audio, so it is only lowered or muted through its `volume` (no boost or EQ). The popover says so when this applies. Audio from plugins, WebRTC streams and offline rendering is not boosted.

## Artwork and appearance

The toolbar's **Theme ▾** button opens five curated palettes: Midnight Party (default), Confetti Sunset, Neon Wool, Alpine Pop, and Forest Carnival. Themes recolor Felifox's chrome, address field, New Tab page and About window—not websites. Theme and animation choices currently last for the session only.

- **Original embedded SVG artwork:** a mischievous midnight-purple cat playing with a flaming wool ball, plus a simplified flaming-yarn symbol. No fox emoji, external assets or large background bitmaps.
- **Five distinct procedural scenes:** Midnight Party has tumbling confetti, streamers, golden stars and neon ribbons; Confetti Sunset has a golden sun, peach/coral hills and falling confetti; Alpine Pop has snowcapped ice mountains, glacier facets, drifting snow and ridge glints; Neon Wool has rolling yarn balls and flowing loose threads; Forest Carnival has swaying foliage, citrus, cherries, flowers and fluttering petals.
- **Visible native animation:** backgrounds are drawn from responsive GPUI paths, with perceptible movement within a second rather than tiny nearly-static color shifts. The mascot also gently bobs; no background image frames or new animation dependencies are used.
- **Motion:** the dropdown includes an Animate artwork checkbox. Animation stops while the window is inactive or web content replaces New Tab, and follows the live macOS Reduce Motion setting. Active animation is limited to about 30 fps; pausing does not affect website animation.
- **About:** open **Felifox → About Felifox**, click the toolbar's **ⓘ**, or use the About link on New Tab. The independently closable window displays **0.1 beta**, the mascot, and Rust / GPUI / WebKit credits. Escape or Cmd-W closes About, not the browser.

The theme chooser is a small native popup, so it appears correctly above the embedded WebKit view without hiding or reloading the page. The artwork is original vector work inspired by the supplied design direction, not a copy of the reference illustrations.

## Architecture

```text
GPUI: tabs, address bar, controls, new-tab page
                     │
              Wry native child view
                     │
         macOS WebKit + JavaScriptCore
```

Two small document-start scripts run in every frame: one for links, one for tab audio. The link script supplies hover destinations, modifier-click requests and the link menu, including inside frames. Its narrowly scoped native message handler accepts only bounded HTTP(S) URLs; it exposes no filesystem or general native-command API. The link menu uses an isolated shadow root within the page because a GPUI overlay would be covered by WebKit's native view. Web pages remain untrusted and can interfere with document-local UI.

A GPUI canvas supplies the webview's window-relative logical bounds so it stays below the toolbar and above the status bar during resizing. Hidden tabs keep their documents and JavaScript state. Native callbacks enqueue events, which the UI drains every 100 ms; they never re-enter GPUI while it is rendering.

The previous WebRender/SWGL pipeline, HTML extraction, custom HTTP loader and `--render-check` command have been removed. WebKit now owns the entire web-content pipeline.

## Scope and limitations

Keep it small: no bookmarks manager, extensions, download UI, permission UI, session restoration, or private-mode promise. WebKit's normal website storage is enabled; cookies and local storage may persist independently of the in-memory tab list.

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

The native webview test verifies actual WebKit JavaScript execution, DOM title changes, localStorage, native history, and injected DOM-event handling for hover, Cmd-click, middle-click and the right-click menu. The brand UI test also verifies that the actual artwork timer advances when enabled and stops when disabled (or when macOS Reduce Motion applies). Geometry tests cover distinct scenes, tessellation and changes over time for all five themes. Native tests use programmatically dispatched DOM events or shared UI handlers, not physical mouse automation. It is skipped by default because WebKit needs a macOS desktop session and main-thread event loop.

## Source

- `src/main.rs`: GPUI window, tabs and navigation controls.
- `src/input.rs`: address input adapted from Zed's Apache-2.0 GPUI input example.
- `src/engine.rs`: address-to-URL/search resolution only.
- `src/webview.rs`: Wry child views, native callbacks, history, bounds and focus.
- `src/theme.rs`: shared palettes, contrast checks and native Reduce Motion support.
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
