# Felinaut — active design references

These are **conceptual reference boards, not screenshots of the browser**.
The source of truth is [`../BRAND.md`](../BRAND.md): Felinaut is a happy female
astronaut cat, a curious explorer who loves colorful balls—especially wool
balls and planets that look like yarn. She is the same orange/cream tabby in
every environment, with visible ears inside a cyan bubble helmet, a soft
white/lavender suit, rounded paws, and **one** readable smile.

## The current boards

Each board has a self-contained, editable SVG and a **1600 × 1200 PNG** preview:

| Board                                                                     | What it documents                                                                                                                                                                                                                                                                                                                                                                                                         |
| ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Brand](felinaut-brand-board.png) · [SVG](felinaut-brand-board.svg)       | Canonical explorer, yarn-planet interaction, Georgia wordmark direction, compact helmet, and expedition colors.                                                                                                                                                                                                                                                                                                           |
| [Themes](felinaut-theme-board.png) · [SVG](felinaut-theme-board.svg)      | All six current themes, with exact Rust variant names, persisted keys, and all eleven color tokens per palette. The scenes are illustrative studies, not captures of procedural rendering.                                                                                                                                                                                                                                |
| [Artwork](felinaut-artwork-board.png) · [SVG](felinaut-artwork-board.svg) | Study 01 illustrates the **current static New Tab composition**: the canonical foreground box overlays the canonical mascot at identical bounds, so she sits inside it. Studies 02–04 (tab-shaped toy, yarn-planet rest stop, and accumulated discoveries) remain explicitly labeled **future interaction concepts**, not implemented animations or browser state. This board is still an illustration, not a screenshot. |
| [Icons](felinaut-icon-board.png) · [SVG](felinaut-icon-board.svg)         | Canonical helmet on light/dark backgrounds at 16/24/32/64 px, a conceptual application tile, clear-space study, and the existing monochrome controls/integration marks. Not a promise of new controls or an actual `.icns` export.                                                                                                                                                                                        |

The six themes are **Milky Way**, **Midnight Party**, **Confetti Sunset**,
**Neon Wool**, **Alpine Pop**, and **Forest Carnival**. The generator reads their
names, variant identifiers, persisted keys, and colors directly from
`src/theme.rs`; the boards do not maintain a second palette table. Milky Way
is presented first and labeled as the new-profile default. Existing persisted
keys, including `alpine-pop`, are unchanged by reference generation.

## Rebuild, offline

From the repository root:

```sh
python3 scripts/render_brand_references.py
```

Requirements: **Python 3.11+**, plus locally installed **librsvg's
`rsvg-convert`** for PNG previews. There are no third-party Python packages,
network requests, external art services, or new application dependencies.
The SVG-only path works without a rasterizer:

```sh
python3 scripts/render_brand_references.py --svg-only
# Isolated review output, without changing the active boards:
python3 scripts/render_brand_references.py --output target/brand-reference-review
```

The generator inlines the canonical `assets/felinaut-mascot.svg`,
`assets/felinaut-symbol.svg`, `assets/felinaut-box.svg`, and existing UI icon
SVGs as **nested vector SVGs**, not remote links, raster images, or data URLs. Each placement receives
unique paint/clip IDs. Extra yarn planets are cropped from the canonical
mascot's actual wool geometry; the face is never independently redrawn for a
board. In artwork study 01 the actual runtime box is painted **after** the
mascot at the same x/y, width/height and viewBox; its foreground walls cover
her lower suit and boots while preserving her head, paws and yarn planets.
There is no independently redrawn box behind or beside her. The tab-shaped
toy in future study 02 remains reference-only geometry. Nothing in this
script changes browser behavior, settings, or user data, and it never moves
or rewrites archived originals.

SVG output is deterministic for the same source files. Board text uses locally
available Helvetica/Arial and Menlo/DejaVu fallbacks; the wordmark uses Georgia
or a local serif fallback. Those fonts are **board-only**: none of the three
runtime brand assets contains text or requires a font. Rasterized text and antialiasing
can differ between systems and librsvg versions, so PNGs are visual previews,
not cross-platform pixel-exact golden files.

## Validation

```sh
python3 -m unittest discover -s scripts -p 'test_brand_references.py' -v
```

The offline tests exercise palette parsing and invalid/incomplete inputs,
deterministic generation, canonical mascot/symbol/box geometry preservation,
foreground box paint order and identical placement, unique local paint IDs,
absence of remote resources, exact theme-token swatches, one current static
composition versus three future-concept labels, missing-asset errors,
SVG-only operation without a
renderer, current active SVGs, and archive SHA-256 preservation. With
`rsvg-convert` present they actually regenerate all four PNGs in temporary
folders under ignored `target/` and check dimensions. Additional RGBA pixel
checks run when **ImageMagick's `magick`** is also present; both generated and
active previews must contain detailed artwork, orange fur, and cyan glass.
A targeted raster regression also compares study 01 with the canonical
mascot-plus-box composite: the box must cover both boots in brown paper,
its lower wall must remain visible above the caption, and the head, paws
and yarn-planet details must remain unchanged.
Unavailable optional raster checks are reported as skipped, never silently
counted as having run.

The runtime assets also have independent behavioral raster tests in
`src/brand.rs` (see `assets/README.md`). Board validation is not a substitute
for the parent's native AppKit/GPUI `tests/brand_ui.rs` validation.

## Historical references

The four original supplied PNGs are preserved **byte-for-byte, with their
original filenames**, in [`archive/legacy-felifox/`](archive/legacy-felifox/).
That archive's README records their hashes and explicitly marks the older
party-cat/flaming-yarn branding and speculative UI as historical, not current
Felinaut guidance or shipped functionality. Do not use those images for new
brand work.
