//! Embedded, original Felinaut astronaut-cat artwork (see `assets/README.md`).
//!
//! Register with `Application::new().with_assets(BrandAssets)` and use
//! `brand::mascot()`, `brand::symbol()`, or `brand::exploration_box()` wherever
//! a GPUI image is accepted.
//! The host application must declare `mod brand;`; this module does not change
//! application startup or apply layout/sizing to its images.

use std::borrow::Cow;

/// Virtual asset paths, independent of the process working directory.
pub const MASCOT: &str = "brand/mascot.svg";
pub const SYMBOL: &str = "brand/symbol.svg";
pub const BOX: &str = "brand/box.svg";

/// Monochrome 24 × 24 UI icons, tinted with the element's text color via `gpui::svg()`.
pub mod icon {
    pub const SPEAKER_MUTED: &str = "icons/speaker-muted.svg";
    pub const SPEAKER_LOW: &str = "icons/speaker-low.svg";
    pub const SPEAKER: &str = "icons/speaker.svg";
    pub const SPEAKER_BOOST: &str = "icons/speaker-boost.svg";
    pub const VOICE: &str = "icons/voice.svg";
    pub const BASS: &str = "icons/bass.svg";
    pub const NATURAL: &str = "icons/natural.svg";
    pub const SIDEBAR: &str = "icons/sidebar.svg";
    pub const CHEVRON: &str = "icons/chevron.svg";
    pub const GITHUB: &str = "icons/github.svg";
    pub const CHECK: &str = "icons/check.svg";
    pub const ASANA: &str = "icons/asana.svg";
    pub const ALL: [&str; 12] = [
        SPEAKER_MUTED,
        SPEAKER_LOW,
        SPEAKER,
        SPEAKER_BOOST,
        VOICE,
        BASS,
        NATURAL,
        SIDEBAR,
        CHEVRON,
        GITHUB,
        CHECK,
        ASANA,
    ];
}

/// A zero-I/O asset source containing the Felinaut illustrations and UI icons.
pub struct BrandAssets;

impl gpui::AssetSource for BrandAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            MASCOT => include_bytes!("../assets/felinaut-mascot.svg"),
            SYMBOL => include_bytes!("../assets/felinaut-symbol.svg"),
            BOX => include_bytes!("../assets/felinaut-box.svg"),
            icon::SPEAKER_MUTED => include_bytes!("../assets/icons/speaker-muted.svg"),
            icon::SPEAKER_LOW => include_bytes!("../assets/icons/speaker-low.svg"),
            icon::SPEAKER => include_bytes!("../assets/icons/speaker.svg"),
            icon::SPEAKER_BOOST => include_bytes!("../assets/icons/speaker-boost.svg"),
            icon::VOICE => include_bytes!("../assets/icons/voice.svg"),
            icon::BASS => include_bytes!("../assets/icons/bass.svg"),
            icon::NATURAL => include_bytes!("../assets/icons/natural.svg"),
            icon::SIDEBAR => include_bytes!("../assets/icons/sidebar.svg"),
            icon::CHEVRON => include_bytes!("../assets/icons/chevron.svg"),
            icon::GITHUB => include_bytes!("../assets/icons/github.svg"),
            icon::CHECK => include_bytes!("../assets/icons/check.svg"),
            icon::ASANA => include_bytes!("../assets/icons/asana.svg"),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        // Match directory boundaries rather than arbitrary string prefixes.
        Ok(match path {
            "" | "/" => [MASCOT, SYMBOL, BOX]
                .into_iter()
                .chain(icon::ALL)
                .map(Into::into)
                .collect(),
            "brand" | "brand/" => vec![MASCOT.into(), SYMBOL.into(), BOX.into()],
            "icons" | "icons/" => icon::ALL.into_iter().map(Into::into).collect(),
            _ => Vec::new(),
        })
    }
}

/// The transparent 560 × 420 explorer cat reaching toward wool planets; no layout styles.
pub fn mascot() -> gpui::Img {
    gpui::img(gpui::ImageSource::Resource(gpui::Resource::Embedded(
        MASCOT.into(),
    )))
}

/// Foreground 560 × 420 cardboard box; align over [`mascot`] at the same bounds.
/// No imposed layout styles. Leave it out for the full astronaut pose in About.
pub fn exploration_box() -> gpui::Img {
    gpui::img(gpui::ImageSource::Resource(gpui::Resource::Embedded(
        BOX.into(),
    )))
}

/// A monochrome UI icon from [`icon`]; size it and set `text_color` to tint it.
pub fn icon(path: &'static str) -> gpui::Svg {
    gpui::svg().path(path)
}

/// The transparent 64 × 64 astronaut-cat helmet, optically drawn for small UI sizes.
pub fn symbol() -> gpui::Img {
    gpui::img(gpui::ImageSource::Resource(gpui::Resource::Embedded(
        SYMBOL.into(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource;

    #[test]
    fn embedded_assets_are_nonempty_borrowed_bytes() {
        for path in [MASCOT, SYMBOL, BOX].into_iter().chain(icon::ALL) {
            let bytes = BrandAssets.load(path).unwrap().unwrap();
            assert!(matches!(bytes, Cow::Borrowed(_)));
            assert!(!bytes.is_empty());
        }
    }

    #[test]
    fn lists_only_known_directories() {
        for directory in ["brand", "brand/"] {
            let paths = BrandAssets.list(directory).unwrap();
            assert_eq!(
                paths,
                vec![gpui::SharedString::from(MASCOT), SYMBOL.into(), BOX.into()]
            );
        }
        for directory in ["icons", "icons/"] {
            assert_eq!(BrandAssets.list(directory).unwrap().len(), icon::ALL.len());
        }
        for directory in ["", "/", "brand", "icons"] {
            for path in BrandAssets.list(directory).unwrap() {
                assert!(BrandAssets.load(path.as_ref()).unwrap().is_some());
            }
        }
        assert_eq!(BrandAssets.list("").unwrap().len(), 3 + icon::ALL.len());
        for path in ["bran", "brands", "brand/missing", "icon", MASCOT] {
            assert!(BrandAssets.list(path).unwrap().is_empty());
        }
    }

    #[test]
    fn unknown_assets_are_not_found() {
        for path in [
            "",
            "felinaut-mascot.svg",
            "felinaut-symbol.svg",
            "felinaut-box.svg",
            "/brand/box.svg",
            "brand/missing.svg",
            "../brand/symbol.svg",
        ] {
            assert!(BrandAssets.load(path).unwrap().is_none());
        }
    }

    // Keep optional renderer tools out of the application's dependency graph.
    // Pipe embedded bytes rather than reading files that may differ from this build.
    fn render_filter(program: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("could not run {program}: {error}"));
        let mut stdin = child.stdin.take().unwrap();
        std::thread::scope(|scope| {
            // Drain output while feeding input so neither pipe can fill and deadlock.
            let writer = scope.spawn(move || stdin.write_all(input));
            let output = child.wait_with_output().unwrap();
            writer.join().unwrap().unwrap();
            assert!(
                output.status.success(),
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        })
    }

    #[test]
    #[ignore = "requires rsvg-convert and magick on PATH; see assets/README.md"]
    fn illustrations_render_transparently_at_native_and_ui_sizes() {
        for (path, width, height) in [
            (MASCOT, 560, 420),
            (MASCOT, 280, 210),
            (SYMBOL, 64, 64),
            (SYMBOL, 32, 32),
            (SYMBOL, 24, 24),
            (SYMBOL, 16, 16),
        ] {
            let bytes = BrandAssets.load(path).unwrap().unwrap();
            let png = render_filter(
                "rsvg-convert",
                &["-w", &width.to_string(), "-h", &height.to_string()],
                &bytes,
            );
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), width);
            assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), height);
            let rgba = render_filter("magick", &["png:-", "-depth", "8", "rgba:-"], &png);
            let pixels: Vec<_> = rgba.chunks_exact(4).collect();
            assert_eq!(rgba.len(), (width * height * 4) as usize);
            for index in [0, width - 1, (height - 1) * width, height * width - 1] {
                assert_eq!(pixels[index as usize][3], 0, "{path}: transparent corner");
            }
            let pixel = |x: u32, y: u32| pixels[(y * width + x) as usize];
            if path == MASCOT && width == 560 {
                for (x, y) in [(453, 81), (92, 242)] {
                    assert_eq!(pixel(x, y)[3], 255, "both wool planets must be visible");
                }
                let gap = pixel(255, 195);
                assert!(
                    gap[0] > 230 && gap[1] > 200 && gap[2] > 170,
                    "cream muzzle above the one smile, not a second W-shaped mouth"
                );
            }
            if path == SYMBOL && width == 64 {
                for (x, y) in [(28, 38), (36, 38)] {
                    let gap = pixel(x, y);
                    assert!(
                        gap[0] > 230 && gap[1] > 200 && gap[2] > 170,
                        "the compact face also keeps one unambiguous smile"
                    );
                }
            }
            let visible = pixels.iter().filter(|pixel| pixel[3] > 0).count();
            assert!(
                visible > pixels.len() / 5,
                "{path}: recognizable silhouette"
            );
            assert!(
                visible < pixels.len() * 9 / 10,
                "{path}: no opaque backdrop"
            );

            // These broad color families survive optical reduction. They catch an
            // empty render, missing fills/gradients, or accidentally restoring yarn.
            let opaque: Vec<_> = pixels.iter().filter(|pixel| pixel[3] >= 220).collect();
            assert!(
                opaque
                    .iter()
                    .any(|p| p[0] > 210 && p[1] > 100 && p[1] < 205 && p[2] < 160),
                "{path} at {width}px: orange tabby"
            );
            assert!(
                opaque
                    .iter()
                    .any(|p| p[1] > 140 && p[2] > 150 && u16::from(p[0]) + 20 < u16::from(p[1])),
                "{path} at {width}px: cyan visor"
            );
            assert!(
                opaque.iter().any(|p| p[0] > 235
                    && p[1] > 210
                    && p[2] > 160
                    && u16::from(p[2]) + 10 < u16::from(p[1])),
                "{path} at {width}px: cream muzzle"
            );
            assert!(
                opaque
                    .iter()
                    .any(|p| p[0] < 130 && p[1] < 120 && p[2] < 145),
                "{path} at {width}px: contrasting face and outline"
            );
        }
    }

    #[test]
    #[ignore = "requires rsvg-convert and magick on PATH; see assets/README.md"]
    fn exploration_box_renders_paper_and_covers_boots_without_obscuring_the_cat() {
        let bytes = BrandAssets.load(BOX).unwrap().unwrap();
        for (width, height) in [(560, 420), (280, 210)] {
            let png = render_filter(
                "rsvg-convert",
                &["-w", &width.to_string(), "-h", &height.to_string()],
                &bytes,
            );
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), width);
            assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), height);
            let rgba = render_filter("magick", &["png:-", "-depth", "8", "rgba:-"], &png);
            assert_eq!(rgba.len(), (width * height * 4) as usize);
            let pixels: Vec<_> = rgba.chunks_exact(4).collect();
            for index in [0, width - 1, (height - 1) * width, height * width - 1] {
                assert_eq!(
                    pixels[index as usize][3], 0,
                    "transparent box canvas corners"
                );
            }
            let scale = 560 / width;
            for y in 0..height {
                for x in 0..width {
                    if x * scale < 170 || x * scale > 365 || y * scale < 292 || y * scale > 402 {
                        assert_eq!(
                            pixels[(y * width + x) as usize][3],
                            0,
                            "overlay must leave the helmet, paws, tail and wool planets clear"
                        );
                    }
                }
            }
            for (x, y) in [(185, 343), (216, 357), (310, 362), (344, 364)] {
                let index = (y / scale * width + x / scale) as usize;
                assert_eq!(pixels[index][3], 255, "paper walls must cover both boots");
            }
            let visible = pixels.iter().filter(|p| p[3] > 0).count();
            assert!(
                visible > pixels.len() / 30,
                "substantial cardboard silhouette"
            );
            assert!(
                visible < pixels.len() / 8,
                "box stays a small foreground overlay"
            );
            let paper = pixels
                .iter()
                .filter(|p| {
                    p[3] == 255
                        && p[0] > 150
                        && p[1] > 90
                        && p[2] > 50
                        && u16::from(p[0]) > u16::from(p[1]) + 20
                        && u16::from(p[1]) > u16::from(p[2]) + 20
                })
                .count();
            assert!(
                paper > visible / 2,
                "warm brown paper, not an opaque blank panel"
            );
        }
    }

    #[test]
    fn image_helpers_construct_gpui_images() {
        let _: gpui::Img = mascot();
        let _: gpui::Img = symbol();
        let _: gpui::Img = exploration_box();
    }
}
