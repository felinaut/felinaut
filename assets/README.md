# Felifox artwork

These SVGs are original, hand-authored vector artwork created for Felifox, under
this repository's MIT license. Supplied visual references informed the palette
and playful cat / flaming-yarn brief; these assets do not embed or trace a
reference image, use emoji, or depend on fonts, external images, or external URLs.

- `felifox-mascot.svg` — 560 × 420, transparent. A midnight-purple cat with a
  golden eye, wink, smile, hooked tail and two paws hugging/batting a flaming ball
  of yarn. Crossing, curved strands distinguish yarn from a generic fireball.
  A tapered orbital ribbon, loose yarn end and spaced sparks complete the pose.
- `felifox-symbol.svg` — 64 × 64, transparent. A separately drawn, reduced-detail
  flaming-yarn symbol for small UI sizes. Prefer this over shrinking the mascot;
  preview at 16, 24, 32 and 64 px when choosing an application placement.

- `icons/*.svg` — 24 × 24 monochrome UI icons (speaker levels, muted speaker, voice,
  bass and natural sound presets, sidebar, disclosure chevron, check mark). `icons/github.svg` is GitHub's
  mark from Primer Octicons (MIT, GitHub Inc.), used for the GitHub live folder. `icons/asana.svg`
  redraws Asana's three-dot mark as plain circles for the Asana live folder. They are single-color masks: GPUI's `svg()` tints
  them with the element's text color, so they follow every theme.

The shared palette is midnight purple, fuchsia, amber, yellow, aqua and violet.
Shapes, gradients, clip paths, titles and descriptions are readable SVG source;
there is no background rectangle, raster payload, filter or font requirement.

## GPUI integration

`src/brand.rs` embeds both files using `include_bytes!` and implements GPUI 0.2.2's
`AssetSource`. No runtime filesystem access is needed. The host can declare
`mod brand;` and register the source when constructing its application:

```rust
Application::new().with_assets(brand::BrandAssets)
```

Use `brand::mascot()` and `brand::symbol()` for unstyled `gpui::Img` elements;
set their size with the host's usual GPUI styling. Direct virtual paths are
`brand::MASCOT` (`brand/mascot.svg`) and `brand::SYMBOL` (`brand/symbol.svg`).
`load` returns `Ok(None)` for unknown paths. `list` returns both virtual paths
for the root or `brand` directory, and an empty list for unknown directories.

Integration is intentionally left to the host: these assets do not modify main,
themes, Cargo configuration or any other asset source. If another source is
already registered, compose/delegate to it in the host instead of replacing it.

## Validation

Parse SVG XML without third-party Python dependencies:

```sh
python3 -c 'import xml.etree.ElementTree as ET; from pathlib import Path; [ET.parse(p) for p in Path("assets").glob("*.svg")]; print("SVG XML OK")'
```

With librsvg installed, a transparent preview can be rendered locally using
`rsvg-convert assets/felifox-mascot.svg -o assets/felifox-mascot-preview.png`.
Generated previews are not required or embedded by the application.
