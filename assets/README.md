# Felinaut artwork

These SVGs are original, hand-authored vector artwork created for Felinaut,
under this repository's MIT license. Follow `../BRAND.md` before changing
these assets. Felinaut is a happy female astronaut cat and curious explorer;
she loves colorful balls, especially wool balls and yarn-like planets. No asset
traces a reference image or uses emoji, fonts, embedded bitmaps, scripts,
external images, or remote resources.

## The three brand SVGs

- `felinaut-mascot.svg` — **560 × 420**, transparent. A warm orange-and-cream
  tabby floats in a white and lavender EVA suit, curiously reaching a rounded,
  pink-padded paw toward a prominent colorful yarn planet. A second little
  wool ball trails a thread toward her other paw. Crossing curved strands,
  volume shading, contrasting wound bands and loose threads make both planets
  read as spherical wool rather than flat striped discs. The large round cyan
  bubble helmet encloses the pointed ears; crescent eyes, rosy cheeks and
  **one** open smile give her a joyful expression. The short nose connector
  is not a second W-shaped muzzle smile.
  Layered neck seals, articulated boots, a life-support pack, a small control
  panel and a geometric paw-and-star mission patch add suit detail. A curling
  striped tail, broken orbital threads, the two wool planets and spaced stars
  complete the floating pose without filling the background. Its original
  560 × 420 viewBox is retained for existing New Tab and About layouts.
- `felinaut-symbol.svg` — **64 × 64**, transparent. A separately drawn,
  optically simplified astronaut-cat head: broad orange ears, a cream muzzle,
  happy eyes and a smile inside a cyan visor, with a white/lavender helmet rim
  and compact neck seal. There is no miniature body or surrounding decoration.
  Prefer this over shrinking the full mascot. Designed to remain recognizable
  at **16, 24, 32 and 64 px**, including against light and dark backgrounds.

- `felinaut-box.svg` — **560 × 420**, transparent foreground overlay. Soft
  warm cardboard walls, folded flaps, subtle corrugated edges, a taped corner
  and a small paw-and-star mission sticker form her beloved exploration ship.
  There is no face, mouth or lettering on the box. Its geometry occupies only
  the lower canvas (approximately x 175–362, y 295–400), covering the lower
  suit and both boots while leaving the helmet, face, paws, curled tail and
  wool planets clear. Place it **over** the canonical mascot with exactly the
  same rectangle and move the pair as one unit for the New Tab box pose.
  Omit the overlay in About to retain the full floating astronaut. The source
  mascot is unchanged; the box is a separate native image, not a face redraw.

The shared palette is warm apricot/orange, cream, cyan, white, lavender and
soft ink-purple, with warm paper browns for the exploration box. All three
assets use ordinary paths, circles, simple
linear gradients, circular wool clip paths and strokes. There are no SVG filters, masks, text elements,
CSS dependencies or background rectangles. Small facial features are drawn
as vector geometry, not font glyphs. This deliberately conservative SVG
subset is suitable for native AppKit and resvg decoding; the native harness
must still be run when changing the artwork.

`icons/*.svg` are the existing **24 × 24 monochrome UI icons** (speaker levels,
muted speaker, voice, bass and natural sound presets, sidebar, disclosure
chevron and check mark). `icons/github.svg` is GitHub's mark from Primer
Octicons (MIT, GitHub Inc.), used for the GitHub live folder.
`icons/asana.svg` redraws Asana's three-dot mark as plain circles for the Asana
live folder. These are single-color masks: GPUI's `svg()` tints them with the
element's text color so they follow every theme. The colored mascot and
symbol and cardboard overlay instead use `img()` to preserve their palette.

## GPUI integration

`src/brand.rs` embeds all three Felinaut SVGs with `include_bytes!` and implements
GPUI 0.2.2's `AssetSource`. No runtime filesystem access or network request is
needed. The host declares `mod brand;` and registers the source when
constructing its application:

```rust
Application::new().with_assets(brand::BrandAssets)
```

Use `brand::mascot()`, `brand::symbol()` and `brand::exploration_box()` for
unstyled `gpui::Img` elements; set their size with the host's usual GPUI styling.
The virtual paths are **`brand/mascot.svg`** (`brand::MASCOT`),
**`brand/symbol.svg`** (`brand::SYMBOL`) and **`brand/box.svg`** (`brand::BOX`).
The first two paths remain unchanged. The optional box helper imposes no
layout, sizing or animation; the host aligns it above the mascot in the same
560 × 420 coordinate system for New Tab only.

`load` returns borrowed embedded bytes or `Ok(None)` for an unknown path.
`list` returns mascot, symbol, box and UI icon paths for the root (`""` or `"/"`),
just the three brand SVGs for `brand`/`brand/`, and just UI icons for
`icons`/`icons/`. Unknown directories and partial prefixes return an empty
list. If another asset source is already registered, compose/delegate to it
in the host rather than replacing it.

## Validation

The ordinary `brand::tests` cover embedded asset loading, directory boundaries,
unknown paths and GPUI image construction for all three assets. Additional
**opt-in behavioral raster tests** use locally installed librsvg
(`rsvg-convert`) and ImageMagick
(`magick`), with no new Rust or runtime dependencies:

```sh
cargo test --locked --bins -F gpui/runtime_shaders brand::tests
cargo test --locked --bins -F gpui/runtime_shaders \
  brand::tests -- --include-ignored
```

The opt-in test feeds the **embedded SVG bytes** through librsvg and decodes
the resulting PNG into RGBA pixels. It checks the full mascot at 560 × 420
and 280 × 210, and the symbol at 16, 24, 32 and 64 px: exact output dimensions,
transparent corners, substantial but non-background silhouette coverage,
visible orange, cyan, cream and contrasting ink details, both wool-planet
positions, and clear muzzle space above the single smile. It tests rendering
behavior rather than searching SVG source for expected strings.
`exploration_box_renders_paper_and_covers_boots_without_obscuring_the_cat`
checks the box at 560 × 420 and 280 × 210: dimensions, transparent corners,
strictly bounded foreground coverage, fully opaque paper over both boot
positions, clear space for the rest of the cat and planets, and a majority
of warm brown paper pixels. Both raster tests are ignored by default because
the command-line tools are optional development tools; requesting them
explicitly without those tools fails rather than silently passing.

For a local visual preview (do not commit generated PNGs):

```sh
rsvg-convert assets/felinaut-mascot.svg -o /tmp/felinaut-mascot-preview.png
rsvg-convert assets/felinaut-symbol.svg -w 256 -h 256 -o /tmp/felinaut-symbol-preview.png
rsvg-convert assets/felinaut-box.svg -o /tmp/felinaut-box-preview.png
```

The active conceptual reference boards in `../references/` inline these
canonical SVGs without redrawing the face. Regenerate them after an asset
change with `python3 scripts/render_brand_references.py`; see
`../references/README.md` for offline tooling, validation and historical
archive details. Planned personality poses and animation directions are
**not** claims of implemented browser behavior. The separate exploration-box
SVG belongs to the host composition, not the transparent mascot file; adding
it to New Tab does not change the astronaut used by About. Artwork reference
study 01 illustrates this current static New Tab composition using the actual
canonical foreground overlay, aligned over the mascot at identical bounds.
The other three interaction studies remain labeled as future concepts; none
of the reference boards is an actual browser screenshot.

The parent application's opt-in `tests/brand_ui.rs` validates actual native
SVG decoding and UI integration. The librsvg test is an additional offline
check, **not** a substitute for AppKit/GPUI native validation. Use the canonical
`FELINAUT_NATIVE_TEST` variable; `FELIFOX_NATIVE_TEST` remains a legacy fallback:

```sh
FELINAUT_NATIVE_TEST=1 cargo test --locked -F gpui/runtime_shaders --test brand_ui
```
