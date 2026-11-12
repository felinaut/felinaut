//! Shared color tokens: themes change browser chrome, never website styling.

use std::path::{Path, PathBuf};

use gpui::{App, Global};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    MidnightParty,
    ConfettiSunset,
    NeonWool,
    AlpinePop,
    ForestCarnival,
    #[default]
    MilkyWay,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub theme: Theme,
    pub background: u32,
    pub surface: u32,
    pub elevated: u32,
    pub input: u32,
    pub text: u32,
    pub muted: u32,
    pub border: u32,
    pub primary: u32,
    pub secondary: u32,
    pub accent: u32,
    pub highlight: u32,
    pub dark: bool,
}

impl Theme {
    pub const ALL: [Self; 6] = [
        Self::MidnightParty,
        Self::ConfettiSunset,
        Self::NeonWool,
        Self::AlpinePop,
        Self::ForestCarnival,
        Self::MilkyWay,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::MidnightParty => "Midnight Party",
            Self::ConfettiSunset => "Confetti Sunset",
            Self::NeonWool => "Neon Wool",
            Self::AlpinePop => "Alpine Pop",
            Self::ForestCarnival => "Forest Carnival",
            Self::MilkyWay => "Milky Way",
        }
    }

    pub fn palette(self) -> Palette {
        match self {
            Self::MilkyWay => Palette {
                theme: self,
                background: 0x0b1026,
                surface: 0x131b36,
                elevated: 0x1d2948,
                input: 0x090e21,
                text: 0xf7f6ff,
                muted: 0xb9c5e2,
                border: 0x4b5c82,
                primary: 0xb3a0ff,
                secondary: 0x8fe4cc,
                accent: 0xffb45e,
                highlight: 0x78d9ef,
                dark: true,
            },
            Self::MidnightParty => Palette {
                theme: self,
                background: 0x2b0a54,
                surface: 0x3c126f,
                elevated: 0x512287,
                input: 0x23083f,
                text: 0xf8f7ff,
                muted: 0xc6b5de,
                border: 0x70499a,
                primary: 0xff3ea5,
                secondary: 0x8a4dff,
                accent: 0xff8a00,
                highlight: 0x00d4ff,
                dark: true,
            },
            Self::ConfettiSunset => Palette {
                theme: self,
                background: 0xfff8ed,
                surface: 0xffffff,
                elevated: 0xffe6ee,
                input: 0xfffaf5,
                text: 0x2b0a54,
                muted: 0x77566e,
                border: 0xd9b7d5,
                primary: 0xba165e,
                secondary: 0xff8a00,
                accent: 0xffd83d,
                highlight: 0x008da9,
                dark: false,
            },
            Self::NeonWool => Palette {
                theme: self,
                background: 0x1b0638,
                surface: 0x341364,
                elevated: 0x4c2383,
                input: 0x220b43,
                text: 0xf8f7ff,
                muted: 0xc4b4df,
                border: 0x6b4694,
                primary: 0xae80ff,
                secondary: 0x00d4ff,
                accent: 0xff3ea5,
                highlight: 0xffd83d,
                dark: true,
            },
            Self::AlpinePop => Palette {
                theme: self,
                background: 0xf2faff,
                surface: 0xffffff,
                elevated: 0xdff3ff,
                input: 0xf6fbff,
                text: 0x2b0a54,
                muted: 0x52657c,
                border: 0xafcadf,
                primary: 0x007c9e,
                secondary: 0x8a4dff,
                accent: 0xffb400,
                highlight: 0xd4207f,
                dark: false,
            },
            Self::ForestCarnival => Palette {
                theme: self,
                background: 0xf3fff2,
                surface: 0xffffff,
                elevated: 0xe0f5df,
                input: 0xf8fff7,
                text: 0x1b0638,
                muted: 0x536b57,
                border: 0xaccfae,
                primary: 0x007e38,
                secondary: 0x8a4dff,
                accent: 0xffd83d,
                highlight: 0xd4207f,
                dark: false,
            },
        }
    }
}

/// What the address field shows while it is not being edited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AddressDisplay {
    /// Site icon, page title and the domain.
    #[default]
    Title,
    /// Site icon and the domain only, like Safari.
    Domain,
    /// The full URL in the editable field, like Chrome.
    Url,
}

impl AddressDisplay {
    pub const ALL: [Self; 3] = [Self::Title, Self::Domain, Self::Url];

    pub fn name(self) -> &'static str {
        match self {
            Self::Title => "Page title",
            Self::Domain => "Domain only",
            Self::Url => "Full address",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Domain => "domain",
            Self::Url => "url",
        }
    }
}

impl Theme {
    // Persisted identifiers are independent of display names and branding.
    fn key(self) -> &'static str {
        match self {
            Self::MidnightParty => "midnight-party",
            Self::ConfettiSunset => "confetti-sunset",
            Self::NeonWool => "neon-wool",
            Self::AlpinePop => "alpine-pop",
            Self::ForestCarnival => "forest-carnival",
            Self::MilkyWay => "milky-way",
        }
    }
}

/// The Appearance dropdown's choices, kept in `~/Library/Application Support/Felifox/settings`
/// as `key=value` lines.
pub struct ThemeSettings {
    pub theme: Theme,
    /// Preference, additionally constrained by macOS Reduce Motion.
    pub animate: bool,
    pub address: AddressDisplay,
    /// Whether the vertical tab sidebar is shown (⌘S).
    pub sidebar: bool,
    /// Its width in points, set by dragging its right edge.
    pub sidebar_width: f32,
}

pub const SIDEBAR_MIN: f32 = 200.;
pub const SIDEBAR_MAX: f32 = 400.;
impl Global for ThemeSettings {}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            animate: true,
            address: AddressDisplay::default(),
            sidebar: true,
            sidebar_width: 240.,
        }
    }
}

pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Felifox/settings"))
}

impl ThemeSettings {
    /// Unknown keys and values keep their defaults, so an edited file never breaks startup.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                ("theme", value) => {
                    if let Some(theme) = Theme::ALL.into_iter().find(|t| t.key() == value) {
                        settings.theme = theme;
                    }
                }
                ("animate", value) => settings.animate = value != "false",
                ("sidebar", value) => settings.sidebar = value != "false",
                ("sidebar_width", value) => {
                    if let Ok(width) = value.parse::<f32>()
                        && width.is_finite()
                    {
                        settings.sidebar_width = width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                    }
                }
                ("address", value) => {
                    if let Some(display) =
                        AddressDisplay::ALL.into_iter().find(|d| d.key() == value)
                    {
                        settings.address = display;
                    }
                }
                _ => {}
            }
        }
        settings
    }

    pub fn serialize(&self) -> String {
        format!(
            "theme={}\nanimate={}\naddress={}\nsidebar={}\nsidebar_width={:.0}\n",
            self.theme.key(),
            self.animate,
            self.address.key(),
            self.sidebar,
            self.sidebar_width
        )
    }

    pub fn load() -> Self {
        settings_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    /// A few short lines, written when the user changes a choice.
    pub fn save(&self) {
        // The native UI tests drive these handlers; they must not touch the user's file.
        if cfg!(test) {
            return;
        }
        let Some(path) = settings_path() else {
            return;
        };
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, self.serialize()));
        if let Err(error) = result {
            eprintln!("Could not save settings: {error}");
        }
    }
}

pub fn palette(cx: &App) -> Palette {
    cx.global::<ThemeSettings>().theme.palette()
}

pub fn system_reduced_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

pub fn motion_allowed(cx: &App) -> bool {
    cx.global::<ThemeSettings>().animate && !system_reduced_motion()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn luminance(color: u32) -> f64 {
        let channel = |shift: u32| {
            let c = ((color >> shift) & 255u32) as f64 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }
    fn contrast(a: u32, b: u32) -> f64 {
        let a = luminance(a);
        let b = luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn settings_round_trip_and_tolerate_edits() {
        let settings = ThemeSettings {
            theme: Theme::AlpinePop,
            animate: false,
            address: AddressDisplay::Domain,
            sidebar: false,
            sidebar_width: 312.4,
        };
        let text = settings.serialize();
        assert_eq!(
            text,
            "theme=alpine-pop\nanimate=false\naddress=domain\nsidebar=false\nsidebar_width=312\n"
        );
        assert_eq!(ThemeSettings::parse(&text).sidebar_width, 312.);
        for (text, width) in [("sidebar_width=10", 200.), ("sidebar_width=9999", 400.)] {
            assert_eq!(ThemeSettings::parse(text).sidebar_width, width);
        }
        for text in [
            "sidebar_width=NaN",
            "sidebar_width=inf",
            "sidebar_width=wide",
        ] {
            assert_eq!(ThemeSettings::parse(text).sidebar_width, 240.);
        }
        let parsed = ThemeSettings::parse(&text);
        assert_eq!(parsed.theme, Theme::AlpinePop);
        assert!(!parsed.animate);
        assert!(!parsed.sidebar);
        assert_eq!(parsed.address, AddressDisplay::Domain);

        let sloppy = ThemeSettings::parse("junk\ntheme = Neon Wool\naddress=url\nanimate=yes\n");
        assert_eq!(
            sloppy.theme,
            Theme::default(),
            "unknown theme keeps the default"
        );
        assert_eq!(sloppy.address, AddressDisplay::Url);
        assert!(sloppy.animate);
        let empty = ThemeSettings::parse("");
        assert!(empty.animate && empty.address == AddressDisplay::Title && empty.sidebar);
    }

    #[test]
    fn every_theme_round_trips_and_preserves_legacy_keys() {
        let keys = [
            ("midnight-party", Theme::MidnightParty),
            ("confetti-sunset", Theme::ConfettiSunset),
            ("neon-wool", Theme::NeonWool),
            ("alpine-pop", Theme::AlpinePop),
            ("forest-carnival", Theme::ForestCarnival),
            ("milky-way", Theme::MilkyWay),
        ];
        assert_eq!(Theme::ALL.len(), keys.len());
        for (key, theme) in keys {
            assert_eq!(theme.key(), key);
            assert_eq!(ThemeSettings::parse(&format!("theme={key}")).theme, theme);
            for animate in [false, true] {
                let settings = ThemeSettings {
                    theme,
                    animate,
                    ..ThemeSettings::default()
                };
                let parsed = ThemeSettings::parse(&settings.serialize());
                assert_eq!(parsed.theme, theme);
                assert_eq!(parsed.animate, animate);
                assert_eq!(parsed.address, settings.address);
                assert_eq!(parsed.sidebar, settings.sidebar);
                assert_eq!(parsed.sidebar_width, settings.sidebar_width);
            }
        }
    }

    #[test]
    fn new_missing_and_invalid_themes_default_to_milky_way() {
        assert_eq!(Theme::default(), Theme::MilkyWay);
        assert_eq!(ThemeSettings::default().theme, Theme::MilkyWay);
        for text in [
            "",
            "theme=",
            "theme=unknown",
            "theme=Milky Way",
            "theme=milky_way",
            "theme=💫",
            "theme=milky-way=oops",
            "invalid",
        ] {
            assert_eq!(
                ThemeSettings::parse(text).theme,
                Theme::MilkyWay,
                "{text:?}"
            );
        }
        assert_eq!(
            ThemeSettings::parse(" theme = milky-way \n").theme,
            Theme::MilkyWay
        );
    }

    #[test]
    fn mountain_theme_is_named_alpine_pop() {
        assert_eq!(Theme::AlpinePop.name(), "Alpine Pop");
        assert!(Theme::ALL.contains(&Theme::AlpinePop));
    }

    #[test]
    fn text_is_readable_in_every_theme() {
        for theme in Theme::ALL {
            let p = theme.palette();
            for background in [p.background, p.surface, p.elevated, p.input] {
                assert!(
                    contrast(p.text, background) >= 4.5,
                    "{}: primary text",
                    theme.name()
                );
                assert!(
                    contrast(p.muted, background) >= 4.5,
                    "{}: muted text",
                    theme.name()
                );
            }
            for background in [p.background, p.surface, p.elevated, p.input] {
                assert!(
                    contrast(p.primary, background) >= 3.,
                    "{}: primary control accent",
                    theme.name()
                );
            }
        }
    }
}
