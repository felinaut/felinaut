# Felinaut — brand and artwork guide

This is the source of truth for Felinaut's visual identity. Read it **before creating or changing artwork**, themes, icons, packaging illustrations, or reference boards. The browser is **Felinaut**, never a fox: a feline astronaut and curious explorer. Keep the implementation, `assets/README.md`, and the active boards in `references/` aligned with this guide.

## Character and personality

Our mascot is a **happy astronaut cat who loves colorful things, balls, and especially balls of wool**. Planets that look like yarn are the signature connection between cats and space. The cat is curious, welcoming, playful, and eager to explore—not aggressive, lonely, militaristic, or a generic corporate robot.

- Warm apricot/orange tabby fur, cream muzzle, triangular ears, pink paw pads, and a curling striped tail.
- A round transparent cyan bubble helmet, substantial white/lavender rim, soft white spacesuit, compact backpack, rounded boots, and a paw/star mission patch.
- Keep the ears visible inside the helmet so the silhouette is recognizably feline. Suit collars must read as equipment, not another face.
- Express happiness with relaxed crescent eyes, a friendly wave or reaching paw, and **one clearly readable smile**. The open grin may have one small tongue. Do not stack a W-shaped muzzle smile above an open mouth: it reads as two mouths. A short nose-to-mouth connector is fine.
- Draw anatomically coherent limbs and paws; do not add floating extra hands, duplicated faces, extra mouths, or human fingers. Equipment and decoration must not obscure the expression.
- Reuse the same character across every theme. Change the environment and accent lighting, not the species, suit, face construction, or personality.

## Personality, habits, and interaction direction

**She is a brave space explorer who is easily distracted by anything round, colourful, or shiny, and she always lands on her feet—even with no gravity.** Use she/her for Felinaut. She is playful as well as happy: a cat with recognizable habits, not a permanent smiling sticker.

These are art and interaction directions, **not a claim that every animation or browser feature below is implemented**. Keep planned concepts labeled as such in reference boards and documentation.

### Habits and poses

- **Batting things out of orbit:** a closing-tab animation can show her batting a small tab-shaped toy away. Keep it fast, chrome-only, reversible through the usual reopen command, and absent with Reduce Motion. Closing a tab must never wait for an animation.
- **Sitting in boxes:** the New Tab hero is her little cardboard exploration box floating in space, with rounded yarn planets nearby. A cardboard box is a beloved ship, not discarded packaging or visual clutter. Her helmet, paws, and tail must remain readable.
- **Zoomies:** loop-de-loops can be an occasional decorative celebration, not a response to every scroll. Never intercept scrolling, move toolbar targets, cover page content, or animate an inactive window.
- **Proud keyboard helper:** paw prints may appear in a separate illustration or opt-in demo. **Never inject letters or paw prints into the real search/address input**, submit a query, or change typed text. Her help must not sabotage the user.
- **Naps:** after a long session, an optional calm illustration can put her on a warm yarn planet, visor softly fogged from purring. Do not infer that a tab is safe to suspend, discard work, or affect page execution.
- **Yarn accumulation:** the illustration may gain a few orbiting balls as the tab count rises. Around 30 tabs, the playful concept is that she is buried in yarn with her tail poking out; keep rendering bounded, disclose no tab titles/URLs, and never guilt or block the user for having many tabs.

### Small animation vocabulary

A quick load can get a tiny “mrrp!” bounce; a slow load a restrained tail swish. Slow-loading motion must be bounded, not accelerate indefinitely or make the interface anxious. Link-hover curiosity can be suggested by an ear tilt in decorative artwork without moving controls. A download's wide eyes and anticipatory tail wiggle are **future download-UI concepts**, not an invented download status signal.

All motion is opt-out, honors Reduce Motion, pauses when inactive, and has a complete static equivalent. The expression remains recognizable at rest. Do not animate page contents or use page-injected scripts just for mascot antics.

### Voice and error-copy proposals

Warm, specific, short, and never mocking. Useful recovery controls and actual technical status come first; her personality is a secondary line. Only show a message when the application can reliably identify that condition:

| Real condition                                      | Proposed secondary copy                                                                                                                       |
| --------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| Confirmed HTTP 404                                  | “This planet doesn’t exist. Felinaut checked under the couch too.”                                                                            |
| Confirmed connection failure                        | “Lost signal. Felinaut is chasing the Wi-Fi.”                                                                                                 |
| Recoverable process failure                         | “Oops. Felinaut got tangled. 🧶 Untangling…” — only say this while a real recovery is under way                                               |
| Optional high-tab-count hint                        | “That’s a lot of yarn. Want help untangling?” — offer dismissible help, never fabricate a hard limit                                          |
| A future real Swat blocker reporting an exact count | “Felinaut swatted 14 trackers off the table. Not sorry.” — **never claim tracker blocking or a count without a real verified implementation** |

Current WebKit callback limitations and unimplemented downloads/crash-recovery/blocker UI must not be papered over with invented states. Do not inspect browsing contents or add telemetry to decide which cute message to show.

### Easter eggs and sounds — future concepts

- `about:meow`: a **local browser-owned** illustration, possibly a purr only if sounds have been enabled. No remote request, URL leakage, or relaxation of navigation restrictions.
- Konami code: an explicitly activated, easily dismissed **chrome-only** yarn-planet celebration, at most one minute. Do not transform real websites or install broad keyboard hooks that interfere with input.
- Idle laser dot: optional decoration limited to the New Tab illustration, never fake the cursor, capture it, or overlay a laser on websites.
- **18 October**, the anniversary of Félicette’s flight: an optional vintage French space-helmet illustration, respectful and based on the local date, without a network holiday lookup.
- Overnight gift: a tiny illustrated toy “mouse” or cookie token can accompany “She brought you a gift.” **Do not inspect, read, delete, or misrepresent real cookies**, run overnight work, or imply a security action occurred.
- Sounds are **off by default**, with a clear dedicated preference, accessible mute, and no surprise playback: soft launch purr, tiny tab-switch boing, and a slow-blink load chime. A slow blink means affection, not an alert. Never change website audio or the user's tab-volume settings to play mascot sounds.

## Signature motifs

**Yarn planets** are colorful spherical balls of wool, not flaming meteors: use curved crossing thread bands that wrap convincingly around the sphere, a loose trailing strand, and optional thin orbital rings. Pink, violet, cyan, apricot, and buttery yellow may appear together, with deliberate separation and a restrained ink outline. Preserve their round volume; do not turn them into striped flat discs.

Supporting motifs: softly glowing stars, playful constellations, orbital threads, round moons, colorful balls, nebula ribbons, and distant planets. A paw reaching toward a yarn planet is a good expression of curiosity. Keep large yarn planets and decorative ribbons at the edges so the main cat, title, and search field remain readable.

Avoid party hats, flame-shaped yarn, old non-astronaut cat artwork, fox imagery, sharp weapons, alarm-red space hazards, photorealistic clutter, and overloaded neon behind text. Historical boards are archival only, not current design direction.

## Color and rendering

| Role             | Color     | Use                                          |
| ---------------- | --------- | -------------------------------------------- |
| Deep-space ink   | `#0B1026` | Milky Way background; calm, not pure black   |
| Orbital surface  | `#131B36` | Dark panels and cards                        |
| Elevated surface | `#1D2948` | Raised controls                              |
| Starlight        | `#F7F6FF` | Primary dark-theme text and suit highlights  |
| Moon dust        | `#B9C5E2` | Muted dark-theme text                        |
| Nebula lavender  | `#B3A0FF` | Primary dark-theme accents and suit shading  |
| Visor cyan       | `#78D9EF` | Helmet glass, orbital highlights             |
| Wool pink        | `#FF72B6` | Yarn bands and small playful accents         |
| Tabby apricot    | `#FFB45E` | Warm fur and yarn highlights                 |
| Mint aurora      | `#8FE4CC` | Secondary highlights and exploration details |

Illustrations may use deeper outline shades and related intermediate gradient colors. UI palettes must use tested contrast rather than copying decorative colors indiscriminately: at least **4.5:1** for normal and muted text on their surfaces, and **3:1** for primary control accents on their surface.

Use original vector geometry with rounded joins, confident outlines, simple gradients, and restrained translucent reflections. Make helmet glass legible without washing out the face. Assets must work on light and dark backgrounds. Do not depend on external images, web fonts, embedded raster data, scripts, SVG filters, or remotely fetched assets.

## Themes and composition

All six themes share the astronaut cat and yarn-planet language:

- **Milky Way** — default for new profiles: deep navy, lavender nebulae, warm yarn planets, delicate stars and orbiting threads. A quiet central clearing surrounded by rich peripheral detail.
- **Midnight Party** — a joyful violet orbital celebration; confetti-like stars and colorful orbital ribbons, never a party-hat mascot.
- **Confetti Sunset** — a warm, bright exploration scene with peach light, soft horizon curves, and yarn planets catching the sunset.
- **Neon Wool** — the most playful yarn system: vivid pink/cyan/violet wool planets and loose thread orbits on dark violet.
- **Alpine Pop** — an icy planetary expedition: cyan and white facets, distant moons, glints, and an astronaut explorer. Preserve the existing `alpine-pop` settings key.
- **Forest Carnival** — a friendly green exoplanet garden: rounded foliage, mint light, colorful spherical discoveries, and orbital details.

Retain the five existing persisted theme keys. New installations may default to Milky Way; existing explicit selections must keep working. Every scene needs a complete still frame, responsive geometry, and a visually quiet center for the mascot, name, and search controls. Animate gently and deterministically; no flashing or forced motion. Honor Reduce Motion, the animation setting, and inactive-window pauses.

## Wordmark, icon, and layout

Write **Felinaut** with this capitalization. The in-app wordmark uses the existing bold italic Georgia treatment, paired with the native system font for controls and supporting text. Do not add a font download or bake UI text into the mascot. Short supporting copy should invite exploration, for example **“A little curiosity. A whole universe.”**

- Full mascot: transparent **560 × 420** SVG; preserve its viewBox and breathing room.
- Compact symbol: a separately simplified astronaut-cat helmet at **64 × 64**, recognizable at **16, 24, 32, and 64 px**. No miniature full body or tiny yarn scene inside the icon.
- macOS app icon: the mascot or helmet on a softly rounded deep-space tile; clear silhouette and modest stars/yarn details. Do not merely put a new label on the old cat icon.
- Toolbar/action icons remain monochrome, optically consistent, and tinted by the UI theme. Do not replace functional controls with ambiguous decorative space symbols.
- About, New Tab, installers, and brand boards must use the canonical mascot/symbol assets rather than redrawing subtly different faces.

## Reference boards and artwork production

The active `references/` boards must show **Felinaut**, the astronaut cat, yarn planets, and all six current themes, including Milky Way. Retire the old Felifox/party-cat boards into an explicitly labeled historical archive rather than presenting them as current guidance. Preserve originals for provenance; do not silently retouch them or claim they depict the shipped browser.

Reference boards are design illustrations, not promises of unimplemented features. Label any mockups as concepts; do not invent bookmarks, downloads, private browsing, or sync capabilities that the browser does not ship. Prefer reproducible vector boards composed from the canonical assets and current palettes, with rendered PNG previews. Do not claim an external generative-image service was used when artwork was hand-authored.

For generative tools, use this direction and then inspect the result manually:

> Original Felinaut browser artwork: a joyful orange-and-cream tabby astronaut, visible triangular ears inside a round transparent cyan bubble helmet, rounded white/lavender spacesuit, paw/star patch, curling striped tail, one unambiguous happy smile, waving or curiously reaching toward colorful yarn-ball planets. Wrapped curved wool strands, trailing orbital threads, soft lavender nebulae, warm apricot/pink/cyan accents, friendly clean vector illustration, coherent anatomy, generous quiet central space for browser UI. No party hat, no fox, no flames, no second mouth, no extra limbs, no text baked into the character, no unsupported product features.

## Review and validation checklist

- Same happy astronaut-cat identity in the mascot, symbol, app icon, themes, About, installer, and active references.
- One mouth; coherent face, helmet, paws, boots, tail, and suit joins at actual UI sizes.
- Yarn looks like wrapped wool and communicates planetary volume.
- Legible silhouette and transparent edges at intended sizes; test both light and dark backgrounds.
- Theme persistence round trips, old-key compatibility, contrast, geometry tessellation, invalid bounds/phases, and static/motion behavior have behavioral tests.
- Offline SVG decoding/raster tests pass; native UI checks are reported honestly, not inferred from skipped harnesses.
- No copyrighted character tracing, secrets, telemetry, remote artwork requests, new runtime dependencies, or unexplained profile-data changes.
