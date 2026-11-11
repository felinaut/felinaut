//! Embedded, original Felifox brand artwork (see `assets/README.md`).
//!
//! Register with `Application::new().with_assets(BrandAssets)` and use
//! `brand::mascot()` or `brand::symbol()` wherever a GPUI image is accepted.
//! The host application must declare `mod brand;`; this module does not change
//! application startup or apply layout/sizing to its images.

use std::borrow::Cow;

/// Virtual asset paths, independent of the process working directory.
pub const MASCOT: &str = "brand/mascot.svg";
pub const SYMBOL: &str = "brand/symbol.svg";

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
    pub const ALL: [&str; 11] = [
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
    ];
}

/// A zero-I/O asset source containing the Felifox illustrations and UI icons.
pub struct BrandAssets;

impl gpui::AssetSource for BrandAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            MASCOT => include_bytes!("../assets/felifox-mascot.svg"),
            SYMBOL => include_bytes!("../assets/felifox-symbol.svg"),
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
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        // Match directory boundaries rather than arbitrary string prefixes.
        Ok(match path {
            "" | "/" => [MASCOT, SYMBOL]
                .into_iter()
                .chain(icon::ALL)
                .map(Into::into)
                .collect(),
            "brand" | "brand/" => vec![MASCOT.into(), SYMBOL.into()],
            "icons" | "icons/" => icon::ALL.into_iter().map(Into::into).collect(),
            _ => Vec::new(),
        })
    }
}

/// The full transparent 560 × 420 mascot, with no imposed layout styles.
pub fn mascot() -> gpui::Img {
    gpui::img(gpui::ImageSource::Resource(gpui::Resource::Embedded(
        MASCOT.into(),
    )))
}

/// A monochrome UI icon from [`icon`]; size it and set `text_color` to tint it.
pub fn icon(path: &'static str) -> gpui::Svg {
    gpui::svg().path(path)
}

/// The transparent 64 × 64 flaming-yarn mark, designed for small UI sizes.
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
    fn embedded_assets_are_borrowed_svg_bytes() {
        for path in [MASCOT, SYMBOL].into_iter().chain(icon::ALL) {
            let bytes = BrandAssets.load(path).unwrap().unwrap();
            assert!(matches!(bytes, Cow::Borrowed(_)));
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(svg.contains("<svg "));
            assert!(svg.contains("</svg>"));
        }
    }

    #[test]
    fn lists_only_known_directories() {
        for directory in ["brand", "brand/"] {
            let paths = BrandAssets.list(directory).unwrap();
            assert_eq!(paths, vec![gpui::SharedString::from(MASCOT), SYMBOL.into()]);
        }
        for directory in ["icons", "icons/"] {
            assert_eq!(BrandAssets.list(directory).unwrap().len(), icon::ALL.len());
        }
        for directory in ["", "/", "brand", "icons"] {
            for path in BrandAssets.list(directory).unwrap() {
                assert!(BrandAssets.load(path.as_ref()).unwrap().is_some());
            }
        }
        assert_eq!(BrandAssets.list("").unwrap().len(), 2 + icon::ALL.len());
        for path in ["bran", "brands", "brand/missing", "icon", MASCOT] {
            assert!(BrandAssets.list(path).unwrap().is_empty());
        }
    }

    #[test]
    fn unknown_assets_are_not_found() {
        for path in [
            "",
            "felifox-mascot.svg",
            "brand/missing.svg",
            "../brand/symbol.svg",
        ] {
            assert!(BrandAssets.load(path).unwrap().is_none());
        }
    }

    #[test]
    fn image_helpers_construct_gpui_images() {
        let _: gpui::Img = mascot();
        let _: gpui::Img = symbol();
    }
}
