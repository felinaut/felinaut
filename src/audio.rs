//! Per-tab volume model and the narrow page ↔ native audio bridge.
//!
//! Pages report only booleans (is this frame audible, is some media limited). Native code
//! sends a gain and a preset name into `audio_control.js`; nothing else crosses the bridge.

/// Highest supported boost, matching common volume-booster extensions.
pub const MAX_PERCENT: u16 = 600;
/// Values this close to 100 % snap to it while dragging.
const SNAP: f32 = 4.;
/// Frames tracked per page; a hostile page cannot grow native memory without bound.
const MAX_FRAMES: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Preset {
    #[default]
    Flat,
    Voice,
    Bass,
}

impl Preset {
    pub const ALL: [Self; 3] = [Self::Flat, Self::Voice, Self::Bass];

    pub fn name(self) -> &'static str {
        match self {
            Self::Flat => "Natural",
            Self::Voice => "Voice",
            Self::Bass => "Bass",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Flat => "Untouched",
            Self::Voice => "Clear dialogue",
            Self::Bass => "Warm low end",
        }
    }

    /// Stable identifier understood by `audio_control.js`.
    fn key(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Voice => "voice",
            Self::Bass => "bass",
        }
    }
}

/// A tab's audio settings. They belong to the tab and survive navigation within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabAudio {
    pub percent: u16,
    pub muted: bool,
    pub preset: Preset,
}

impl Default for TabAudio {
    fn default() -> Self {
        Self {
            percent: 100,
            muted: false,
            preset: Preset::Flat,
        }
    }
}

impl TabAudio {
    /// Default settings leave the page's audio completely untouched.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn boosted(&self) -> bool {
        !self.muted && self.percent > 100
    }

    pub fn with_percent(self, percent: u16) -> Self {
        Self {
            percent: percent.min(MAX_PERCENT),
            ..self
        }
    }

    /// Linear output gain; 1.0 is the page's own level.
    pub fn gain(&self) -> f32 {
        if self.muted {
            0.
        } else {
            f32::from(self.percent) / 100.
        }
    }

    /// Short label for compact chrome, e.g. the toolbar button.
    pub fn badge(&self) -> Option<String> {
        if self.muted {
            Some("Muted".into())
        } else if self.percent != 100 {
            Some(format!("{}%", self.percent))
        } else {
            None
        }
    }

    /// Speaker glyph level: 0 = muted / silent, 1–3 = increasing waves.
    pub fn level(&self) -> u8 {
        match (self.muted, self.percent) {
            (true, _) | (false, 0) => 0,
            (false, 1..=50) => 1,
            (false, 51..=100) => 2,
            _ => 3,
        }
    }

    /// The only script native code evaluates for audio: two validated literals.
    pub fn script(&self) -> String {
        format!(
            "window.__felifoxAudio&&window.__felifoxAudio({:.2},\"{}\")",
            self.gain(),
            self.preset.key()
        )
    }
}

/// The slider spends its first half on 0–100 % (fine control) and its second half on the
/// 100–600 % boost range, so the page's natural level sits in the middle of the track.
pub fn percent_to_position(percent: u16) -> f32 {
    let percent = f32::from(percent.min(MAX_PERCENT));
    if percent <= 100. {
        percent / 200.
    } else {
        0.5 + (percent - 100.) / (2. * f32::from(MAX_PERCENT - 100))
    }
}

/// Inverse of [`percent_to_position`], with a gentle snap to 100 % and tidy boost steps.
pub fn position_to_percent(position: f32) -> u16 {
    let position = if position.is_finite() {
        position.clamp(0., 1.)
    } else {
        0.
    };
    let raw = if position <= 0.5 {
        position * 200.
    } else {
        100. + (position - 0.5) * 2. * f32::from(MAX_PERCENT - 100)
    };
    if (raw - 100.).abs() <= SNAP {
        return 100;
    }
    let rounded = if raw > 100. {
        (raw / 5.).round() * 5.
    } else {
        raw.round()
    };
    rounded.clamp(0., f32::from(MAX_PERCENT)) as u16
}

/// Keyboard / button steps: 5 % below 100, 25 % above, always stopping at 100.
pub fn step_percent(percent: u16, up: bool) -> u16 {
    match (up, percent) {
        (true, p) if p < 100 => (p + 5).min(100),
        (true, p) => (p + 25).min(MAX_PERCENT),
        (false, p) if p > 100 => p.saturating_sub(25).max(100),
        (false, p) => p.saturating_sub(5),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioReport {
    /// A main-frame document started and wants the tab's current settings.
    Hello,
    /// A frame's playback summary. `limited` means some playing media cannot be boosted
    /// or equalised (for example cross-origin media without CORS).
    Frame {
        id: String,
        audible: bool,
        limited: bool,
    },
}

/// Parses `felifox-audio:…` IPC messages. Anything unexpected is ignored.
pub fn parse_report(message: &str) -> Option<AudioReport> {
    let rest = message.strip_prefix("felifox-audio:")?;
    if rest == "hello" {
        return Some(AudioReport::Hello);
    }
    let mut parts = rest.strip_prefix("state:")?.split(':');
    let id = parts.next()?;
    let flag = |value: Option<&str>| match value {
        Some("0") => Some(false),
        Some("1") => Some(true),
        _ => None,
    };
    let audible = flag(parts.next())?;
    let limited = flag(parts.next())?;
    if parts.next().is_some()
        || id.is_empty()
        || id.len() > 16
        || !id.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(AudioReport::Frame {
        id: id.into(),
        audible,
        limited,
    })
}

/// What a tab's frames currently report. Cleared whenever the main frame navigates.
#[derive(Clone, Debug, Default)]
pub struct PageSound {
    frames: Vec<(String, bool, bool)>,
}

impl PageSound {
    pub fn report(&mut self, id: &str, audible: bool, limited: bool) {
        let existing = self.frames.iter().position(|(frame, ..)| frame == id);
        match (existing, audible || limited) {
            (Some(index), true) => self.frames[index] = (id.into(), audible, limited),
            (Some(index), false) => {
                self.frames.swap_remove(index);
            }
            (None, true) if self.frames.len() < MAX_FRAMES => {
                self.frames.push((id.into(), audible, limited));
            }
            _ => {}
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn audible(&self) -> bool {
        self.frames.iter().any(|(_, audible, _)| *audible)
    }

    pub fn limited(&self) -> bool {
        self.frames.iter().any(|(_, _, limited)| *limited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_transparent() {
        let audio = TabAudio::default();
        assert!(audio.is_default());
        assert_eq!(audio.gain(), 1.);
        assert_eq!(audio.badge(), None);
        assert_eq!(
            audio.script(),
            "window.__felifoxAudio&&window.__felifoxAudio(1.00,\"flat\")"
        );
    }

    #[test]
    fn mute_and_boost_change_gain_and_badge() {
        let boosted = TabAudio::default().with_percent(250);
        assert!(boosted.boosted());
        assert_eq!(boosted.gain(), 2.5);
        assert_eq!(boosted.badge().as_deref(), Some("250%"));
        let muted = TabAudio {
            muted: true,
            ..boosted
        };
        assert!(!muted.boosted());
        assert_eq!(muted.gain(), 0.);
        assert_eq!(muted.level(), 0);
        assert_eq!(muted.badge().as_deref(), Some("Muted"));
        assert_eq!(
            TabAudio {
                preset: Preset::Bass,
                ..TabAudio::default()
            }
            .script(),
            "window.__felifoxAudio&&window.__felifoxAudio(1.00,\"bass\")"
        );
        assert_eq!(TabAudio::default().with_percent(9000).percent, MAX_PERCENT);
    }

    #[test]
    fn slider_mapping_round_trips_and_snaps() {
        assert_eq!(percent_to_position(0), 0.);
        assert_eq!(percent_to_position(100), 0.5);
        assert_eq!(percent_to_position(MAX_PERCENT), 1.);
        for percent in [0, 1, 37, 99, 100, 150, 255, 400, 600] {
            let back = position_to_percent(percent_to_position(percent));
            let expected = if (96..=104).contains(&percent) {
                100
            } else if percent > 100 {
                (percent + 2) / 5 * 5
            } else {
                percent
            };
            assert_eq!(back, expected, "{percent}");
        }
        assert_eq!(position_to_percent(0.49), 100, "snaps to natural level");
        assert_eq!(position_to_percent(-3.), 0);
        assert_eq!(position_to_percent(7.), MAX_PERCENT);
        assert_eq!(position_to_percent(f32::NAN), 0);
    }

    #[test]
    fn steps_stop_at_natural_level() {
        assert_eq!(step_percent(97, true), 100);
        assert_eq!(step_percent(100, true), 125);
        assert_eq!(step_percent(590, true), MAX_PERCENT);
        assert_eq!(step_percent(110, false), 100);
        assert_eq!(step_percent(100, false), 95);
        assert_eq!(step_percent(3, false), 0);
    }

    #[test]
    fn reports_are_strict() {
        assert_eq!(
            parse_report("felifox-audio:hello"),
            Some(AudioReport::Hello)
        );
        assert_eq!(
            parse_report("felifox-audio:state:a1b2c3:1:0"),
            Some(AudioReport::Frame {
                id: "a1b2c3".into(),
                audible: true,
                limited: false
            })
        );
        for message in [
            "felifox-audio:state::1:0",
            "felifox-audio:state:a1:2:0",
            "felifox-audio:state:a1:1",
            "felifox-audio:state:a1:1:0:extra",
            "felifox-audio:state:a-1:1:0",
            "felifox-audio:state:aaaaaaaaaaaaaaaaa:1:0",
            "felifox-audio:goodbye",
            "felifox-link:hover:",
        ] {
            assert_eq!(parse_report(message), None, "{message}");
        }
    }

    #[test]
    fn page_sound_aggregates_bounded_frames() {
        let mut sound = PageSound::default();
        sound.report("main", true, false);
        sound.report("ad", false, true);
        assert!(sound.audible() && sound.limited());
        sound.report("main", false, false);
        assert!(!sound.audible() && sound.limited());
        for index in 0..200 {
            sound.report(&format!("f{index}"), true, false);
        }
        assert_eq!(sound.frames.len(), MAX_FRAMES);
        sound.clear();
        assert!(!sound.audible() && !sound.limited());
    }
}
