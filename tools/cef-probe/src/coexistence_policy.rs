//! Pure policy for the fixed, same-origin `/cef` and `/webkit` fixture.
//!
//! Drain `next_action()` until `None`, executing each returned action once, then
//! feed queued fixture titles to `update` with the engine that emitted them.
//! `None` means waiting (or finished), not permission to repeat an action. The
//! parent owns native load/error handling, dispatch errors and the deadline.
//! Only pass fixture reports here, not native loading/blank title notifications.
//!
//! `CheckStorage` evaluates `window.probe.report('storage')` in BOTH engines;
//! `CefReport` evaluates that same fixed script in CEF only. Click coordinates
//! are pane-local CSS/logical pixels. `Resize` requests a 1200 x 760 window with
//! two 600-CSS-pixel panes. The other actions without arguments are log markers.
//!
//! Reports have exactly seven colon-delimited fields:
//! `COEX:<cef|webkit>:<phase>:<width>:<ok|bad>:<text>:<true|false>`.
//! Width is canonical decimal in 1..=16384; text must be a prefix of the fixed
//! phrase (CEF cannot have the WebKit suffix). `ok` means storage was initially
//! empty AND both localStorage and the root-path cookie still contain this role.
//! The boolean is event.isTrusted for focus/typing/input, otherwise the current
//! successful button-verification state. Failures latch; duplicate accepted
//! reports are harmless but never discharge a later wait. No clock or IO here.

const TEXT: &str = "Hello from Felinaut";
const RETURN_TEXT: &str = "Hello from Felinaut Meow";
const MAX_TITLE_BYTES: usize = 128;
const MAX_REPORTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Cef,
    WebKit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Loaded,
    CheckStorage,
    StorageVerified,
    WebKitClick(f64, f64),
    WebKitType(&'static str),
    CefClick(f64, f64),
    CefType(&'static str),
    CefReport,
    FocusVerified,
    Resize,
    ResizeVerified,
    Complete,
}

const ACTIONS: [Action; 17] = [
    Action::Loaded,
    Action::CheckStorage,
    Action::StorageVerified,
    Action::WebKitClick(100., 150.),
    Action::WebKitType(TEXT),
    Action::WebKitClick(110., 220.),
    Action::CefClick(100., 150.),
    Action::CefType(TEXT),
    Action::CefClick(110., 220.),
    Action::WebKitClick(100., 150.),
    Action::WebKitType(" Meow"),
    Action::WebKitClick(110., 220.),
    Action::CefReport,
    Action::FocusVerified,
    Action::Resize,
    Action::ResizeVerified,
    Action::Complete,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Ready,
    Storage,
    Focus,
    Typing,
    Input,
    Resize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Report {
    engine: Engine,
    phase: Phase,
    width: u16,
    text_len: usize,
    trusted: bool,
}

impl Report {
    fn parse(engine: Engine, title: &str) -> Result<Self, &'static str> {
        if title.len() > MAX_TITLE_BYTES {
            return Err("Coexistence report exceeds title limit");
        }
        let mut parts = title.split(':');
        if parts.next() != Some("COEX") {
            return Err("Invalid coexistence report prefix");
        }
        let reported_engine = match parts.next() {
            Some("cef") => Engine::Cef,
            Some("webkit") => Engine::WebKit,
            _ => return Err("Invalid coexistence report engine"),
        };
        if reported_engine != engine {
            return Err("Coexistence report came from the wrong engine");
        }
        let phase = match parts.next() {
            Some("ready") => Phase::Ready,
            Some("storage") => Phase::Storage,
            Some("focus") => Phase::Focus,
            Some("typing") => Phase::Typing,
            Some("input") => Phase::Input,
            Some("resize") => Phase::Resize,
            _ => return Err("Invalid coexistence report phase"),
        };
        let width = parts.next().ok_or("Missing coexistence report width")?;
        if width.starts_with('0') || !width.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Invalid coexistence report width");
        }
        let width: u16 = width
            .parse()
            .map_err(|_| "Invalid coexistence report width")?;
        if !(1..=16384).contains(&width) {
            return Err("Coexistence report width is out of bounds");
        }
        match parts.next() {
            Some("ok") => (),
            Some("bad") => return Err("Coexistence storage or cookie isolation failed"),
            _ => return Err("Invalid coexistence report isolation status"),
        }
        let text = parts.next().ok_or("Missing coexistence report text")?;
        let allowed = match engine {
            Engine::Cef => TEXT,
            Engine::WebKit => RETURN_TEXT,
        };
        if !allowed.starts_with(text) {
            return Err("Unexpected coexistence input text");
        }
        let trusted = match parts.next() {
            Some("true") => true,
            Some("false") => false,
            _ => return Err("Invalid coexistence report trust flag"),
        };
        if parts.next().is_some() {
            return Err("Extra coexistence report fields");
        }
        Ok(Self {
            engine,
            phase,
            width,
            text_len: text.len(),
            trusted,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    None,
    Ready,
    Storage,
    Focus(Engine, usize),
    Typing(Engine, usize, usize),
    Input(Engine, usize),
    CefStorage,
    Resize,
}

pub struct Probe {
    cursor: usize,
    wait: Wait,
    both: [bool; 2],
    typing_len: usize,
    seen: Vec<Report>,
    failure: Option<&'static str>,
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            wait: Wait::Ready,
            both: [false; 2],
            typing_len: 0,
            seen: Vec::new(),
            failure: None,
        }
    }

    /// A failure is terminal: no further actions, including Complete, can emit.
    pub fn update(&mut self, engine: Engine, title: &str) -> Result<(), &'static str> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let result = Report::parse(engine, title).and_then(|report| self.accept(report));
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }

    fn accept(&mut self, report: Report) -> Result<(), &'static str> {
        if self.seen.contains(&report) {
            return Ok(());
        }
        if self.seen.len() == MAX_REPORTS {
            return Err("Too many distinct coexistence reports");
        }
        let complete = match self.wait {
            Wait::Ready | Wait::Storage => {
                let phase = if self.wait == Wait::Ready {
                    Phase::Ready
                } else {
                    Phase::Storage
                };
                if report.phase != phase || report.text_len != 0 || report.trusted {
                    return Err("Unexpected coexistence loading/storage report");
                }
                self.both[engine_index(report.engine)] = true;
                self.both == [true; 2]
            }
            Wait::Focus(engine, length) => {
                require_event(report, engine, Phase::Focus, length)?;
                true
            }
            Wait::Typing(engine, start, end) => {
                if report.engine != engine
                    || report.phase != Phase::Typing
                    || !report.trusted
                    || report.text_len <= start
                    || report.text_len > end
                    || report.text_len < self.typing_len
                {
                    return Err("Unexpected coexistence typing report");
                }
                self.typing_len = report.text_len;
                report.text_len == end
            }
            Wait::Input(engine, length) => {
                require_event(report, engine, Phase::Input, length)?;
                true
            }
            Wait::CefStorage => {
                require_event(report, Engine::Cef, Phase::Storage, TEXT.len())?;
                true
            }
            Wait::Resize => {
                let length = match report.engine {
                    Engine::Cef => TEXT.len(),
                    Engine::WebKit => RETURN_TEXT.len(),
                };
                require_event(report, report.engine, Phase::Resize, length)?;
                // Intermediate native layout sizes are evidence, not completion.
                self.both[engine_index(report.engine)] = report.width == 600;
                self.both == [true; 2]
            }
            Wait::None => return Err("Coexistence report arrived before its action"),
        };
        self.seen.push(report);
        if complete {
            self.wait = Wait::None;
        }
        Ok(())
    }

    /// Marker actions do not wait; side-effect actions arm their wait before
    /// returning, so repeated calls cannot reissue clicks, typing or scripts.
    pub fn next_action(&mut self) -> Option<Action> {
        if self.failure.is_some() || self.wait != Wait::None {
            return None;
        }
        let action = *ACTIONS.get(self.cursor)?;
        self.cursor += 1;
        self.wait = match self.cursor - 1 {
            1 => Wait::Storage,
            3 => Wait::Focus(Engine::WebKit, 0),
            4 => Wait::Typing(Engine::WebKit, 0, TEXT.len()),
            5 => Wait::Input(Engine::WebKit, TEXT.len()),
            6 => Wait::Focus(Engine::Cef, 0),
            7 => Wait::Typing(Engine::Cef, 0, TEXT.len()),
            8 => Wait::Input(Engine::Cef, TEXT.len()),
            9 => Wait::Focus(Engine::WebKit, TEXT.len()),
            10 => Wait::Typing(Engine::WebKit, TEXT.len(), RETURN_TEXT.len()),
            11 => Wait::Input(Engine::WebKit, RETURN_TEXT.len()),
            12 => Wait::CefStorage,
            14 => Wait::Resize,
            _ => Wait::None,
        };
        if matches!(self.wait, Wait::Ready | Wait::Storage | Wait::Resize) {
            self.both = [false; 2];
        }
        if let Wait::Typing(_, start, _) = self.wait {
            self.typing_len = start;
        }
        Some(action)
    }
}

fn engine_index(engine: Engine) -> usize {
    match engine {
        Engine::Cef => 0,
        Engine::WebKit => 1,
    }
}

fn require_event(
    report: Report,
    engine: Engine,
    phase: Phase,
    length: usize,
) -> Result<(), &'static str> {
    if report.engine != engine
        || report.phase != phase
        || report.text_len != length
        || !report.trusted
    {
        Err("Unexpected or untrusted coexistence event")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(engine: Engine, phase: &str, width: u16, text: &str, trusted: bool) -> String {
        let role = match engine {
            Engine::Cef => "cef",
            Engine::WebKit => "webkit",
        };
        format!("COEX:{role}:{phase}:{width}:ok:{text}:{trusted}")
    }

    fn report(probe: &mut Probe, engine: Engine, phase: &str, text: &str, trusted: bool) {
        probe
            .update(engine, &title(engine, phase, 550, text, trusted))
            .unwrap();
    }

    fn action(probe: &mut Probe, expected: Action) {
        assert_eq!(probe.next_action(), Some(expected));
    }

    fn loading() -> Probe {
        let mut probe = Probe::new();
        report(&mut probe, Engine::Cef, "ready", "", false);
        report(&mut probe, Engine::WebKit, "ready", "", false);
        action(&mut probe, Action::Loaded);
        action(&mut probe, Action::CheckStorage);
        probe
    }

    fn storage() -> Probe {
        let mut probe = loading();
        report(&mut probe, Engine::Cef, "storage", "", false);
        report(&mut probe, Engine::WebKit, "storage", "", false);
        action(&mut probe, Action::StorageVerified);
        probe
    }

    fn input(probe: &mut Probe, engine: Engine, returning: bool) {
        let (click_field, type_text, click_button, before, after) = match (engine, returning) {
            (Engine::Cef, false) => (
                Action::CefClick(100., 150.),
                Action::CefType(TEXT),
                Action::CefClick(110., 220.),
                "",
                TEXT,
            ),
            (Engine::WebKit, false) => (
                Action::WebKitClick(100., 150.),
                Action::WebKitType(TEXT),
                Action::WebKitClick(110., 220.),
                "",
                TEXT,
            ),
            (Engine::WebKit, true) => (
                Action::WebKitClick(100., 150.),
                Action::WebKitType(" Meow"),
                Action::WebKitClick(110., 220.),
                TEXT,
                RETURN_TEXT,
            ),
            _ => unreachable!(),
        };
        action(probe, click_field);
        assert_eq!(probe.next_action(), None);
        report(probe, engine, "focus", before, true);
        action(probe, type_text);
        assert_eq!(probe.next_action(), None);
        for length in before.len() + 1..=after.len() {
            report(probe, engine, "typing", &after[..length], true);
        }
        action(probe, click_button);
        assert_eq!(probe.next_action(), None);
        report(probe, engine, "input", after, true);
    }

    fn focus() -> Probe {
        let mut probe = storage();
        input(&mut probe, Engine::WebKit, false);
        input(&mut probe, Engine::Cef, false);
        input(&mut probe, Engine::WebKit, true);
        action(&mut probe, Action::CefReport);
        assert_eq!(probe.next_action(), None);
        report(&mut probe, Engine::Cef, "storage", TEXT, true);
        action(&mut probe, Action::FocusVerified);
        probe
    }

    fn resize(probe: &mut Probe, engine: Engine, width: u16) {
        let text = if engine == Engine::Cef {
            TEXT
        } else {
            RETURN_TEXT
        };
        probe
            .update(engine, &title(engine, "resize", width, text, true))
            .unwrap();
    }

    #[test]
    fn full_flow_includes_each_marker_and_completes_once() {
        let mut probe = focus();
        action(&mut probe, Action::Resize);
        resize(&mut probe, Engine::Cef, 600);
        resize(&mut probe, Engine::WebKit, 600);
        action(&mut probe, Action::ResizeVerified);
        action(&mut probe, Action::Complete);
        assert_eq!(probe.next_action(), None);
        assert_eq!(probe.next_action(), None);
    }

    #[test]
    fn either_loading_order_requires_both_roles() {
        for first in [Engine::Cef, Engine::WebKit] {
            let second = if first == Engine::Cef {
                Engine::WebKit
            } else {
                Engine::Cef
            };
            let mut probe = Probe::default();
            assert_eq!(probe.next_action(), None);
            report(&mut probe, first, "ready", "", false);
            report(&mut probe, first, "ready", "", false);
            assert_eq!(probe.next_action(), None);
            report(&mut probe, second, "ready", "", false);
            action(&mut probe, Action::Loaded);
            action(&mut probe, Action::CheckStorage);
            assert_eq!(probe.next_action(), None);
        }
    }

    #[test]
    fn storage_requires_both_and_can_arrive_in_reverse_order() {
        let mut probe = loading();
        report(&mut probe, Engine::WebKit, "storage", "", false);
        report(&mut probe, Engine::WebKit, "storage", "", false);
        assert_eq!(probe.next_action(), None);
        report(&mut probe, Engine::Cef, "storage", "", false);
        action(&mut probe, Action::StorageVerified);
    }

    #[test]
    fn duplicates_and_stale_callbacks_never_advance_later_waits() {
        let mut probe = storage();
        input(&mut probe, Engine::WebKit, false);
        action(&mut probe, Action::CefClick(100., 150.));
        for (phase, text, trusted) in [
            ("ready", "", false),
            ("storage", "", false),
            ("focus", "", true),
            ("typing", "H", true),
            ("typing", TEXT, true),
            ("input", TEXT, true),
        ] {
            report(&mut probe, Engine::WebKit, phase, text, trusted);
            assert_eq!(probe.next_action(), None);
        }
        report(&mut probe, Engine::Cef, "focus", "", true);
        action(&mut probe, Action::CefType(TEXT));
    }

    #[test]
    fn intermediate_typing_and_replayed_prefixes_do_not_finish_early() {
        let mut probe = storage();
        action(&mut probe, Action::WebKitClick(100., 150.));
        report(&mut probe, Engine::WebKit, "focus", "", true);
        action(&mut probe, Action::WebKitType(TEXT));
        report(&mut probe, Engine::WebKit, "typing", "H", true);
        report(&mut probe, Engine::WebKit, "typing", "Hello", true);
        report(&mut probe, Engine::WebKit, "typing", "H", true);
        assert_eq!(probe.next_action(), None);
        report(&mut probe, Engine::WebKit, "typing", TEXT, true);
        action(&mut probe, Action::WebKitClick(110., 220.));
    }

    #[test]
    fn coalesced_typing_can_report_only_the_complete_phrase() {
        let mut probe = storage();
        action(&mut probe, Action::WebKitClick(100., 150.));
        report(&mut probe, Engine::WebKit, "focus", "", true);
        action(&mut probe, Action::WebKitType(TEXT));
        report(&mut probe, Engine::WebKit, "typing", TEXT, true);
        action(&mut probe, Action::WebKitClick(110., 220.));
    }

    #[test]
    fn wrong_engine_is_terminal_even_with_an_otherwise_valid_report() {
        let mut probe = Probe::new();
        assert!(
            probe
                .update(Engine::Cef, &title(Engine::WebKit, "ready", 550, "", false))
                .is_err()
        );
        assert!(
            probe
                .update(Engine::Cef, &title(Engine::Cef, "ready", 550, "", false))
                .is_err()
        );
        assert_eq!(probe.next_action(), None);
    }

    #[test]
    fn isolation_failure_is_terminal_in_every_phase() {
        for mut probe in [Probe::new(), loading(), storage(), focus()] {
            assert!(
                probe
                    .update(Engine::Cef, "COEX:cef:storage:550:bad::false")
                    .is_err()
            );
            assert_eq!(probe.next_action(), None);
        }
    }

    #[test]
    fn untrusted_click_cannot_verify_input() {
        let mut probe = storage();
        action(&mut probe, Action::WebKitClick(100., 150.));
        report(&mut probe, Engine::WebKit, "focus", "", true);
        action(&mut probe, Action::WebKitType(TEXT));
        report(&mut probe, Engine::WebKit, "typing", TEXT, true);
        action(&mut probe, Action::WebKitClick(110., 220.));
        assert!(
            probe
                .update(
                    Engine::WebKit,
                    &title(Engine::WebKit, "input", 550, TEXT, false)
                )
                .is_err()
        );
        assert_eq!(probe.next_action(), None);
    }

    #[test]
    fn wrong_text_and_cef_suffix_are_rejected() {
        for (engine, text) in [
            (Engine::Cef, RETURN_TEXT),
            (Engine::WebKit, "Hello from someone"),
            (Engine::WebKit, "Hello from Felinaut Meow!"),
        ] {
            let mut probe = Probe::new();
            assert!(
                probe
                    .update(engine, &title(engine, "typing", 550, text, true))
                    .is_err()
            );
        }
    }

    #[test]
    fn malformed_titles_are_rejected() {
        for value in [
            "",
            "loading",
            "COEX:cef",
            "COEX:CEF:ready:550:ok::false",
            "COEX:cef:unknown:550:ok::false",
            "COEX:cef:ready:550:maybe::false",
            "COEX:cef:ready:550:ok::False",
            "COEX:cef:ready:550:ok::",
            "COEX:cef:ready:550:ok::false:extra",
            "COEX:cef:ready:550:ok:é:false",
            "COEX:cef:ready:550:ok:\n:false",
            "COEX:cef:ready:550:ok::false\0",
        ] {
            assert!(
                Probe::new().update(Engine::Cef, value).is_err(),
                "{value:?}"
            );
        }
        assert!(
            Probe::new()
                .update(Engine::Cef, &"x".repeat(MAX_TITLE_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn widths_are_bounded_canonical_decimal() {
        for width in [
            "",
            "0",
            "00",
            "0550",
            "-1",
            "+550",
            "550.0",
            " 550",
            "16385",
            "65536",
            "999999999999999999999",
        ] {
            assert!(
                Probe::new()
                    .update(Engine::Cef, &format!("COEX:cef:ready:{width}:ok::false"))
                    .is_err(),
                "{width:?}"
            );
        }
        for width in [1, 550, 600, 16384] {
            assert!(
                Probe::new()
                    .update(Engine::Cef, &title(Engine::Cef, "ready", width, "", false))
                    .is_ok()
            );
        }
    }

    #[test]
    fn out_of_order_and_unsolicited_reports_fail() {
        assert!(
            Probe::new()
                .update(Engine::Cef, "COEX:cef:storage:550:ok::false")
                .is_err()
        );
        let mut probe = storage();
        assert!(
            probe
                .update(Engine::WebKit, "COEX:webkit:focus:550:ok::true")
                .is_err()
        );
        assert_eq!(probe.next_action(), None);
    }

    #[test]
    fn other_engine_cannot_satisfy_a_focus_wait() {
        let mut probe = storage();
        action(&mut probe, Action::WebKitClick(100., 150.));
        assert!(
            probe
                .update(Engine::Cef, "COEX:cef:focus:550:ok::true")
                .is_err()
        );
    }

    #[test]
    fn untrusted_focus_and_typing_are_rejected() {
        let mut probe = storage();
        action(&mut probe, Action::WebKitClick(100., 150.));
        assert!(
            probe
                .update(Engine::WebKit, "COEX:webkit:focus:550:ok::false")
                .is_err()
        );
        let mut probe = storage();
        action(&mut probe, Action::WebKitClick(100., 150.));
        report(&mut probe, Engine::WebKit, "focus", "", true);
        action(&mut probe, Action::WebKitType(TEXT));
        assert!(
            probe
                .update(Engine::WebKit, "COEX:webkit:typing:550:ok:H:false")
                .is_err()
        );
    }

    #[test]
    fn unseen_backward_typing_is_not_a_duplicate() {
        let mut probe = storage();
        action(&mut probe, Action::WebKitClick(100., 150.));
        report(&mut probe, Engine::WebKit, "focus", "", true);
        action(&mut probe, Action::WebKitType(TEXT));
        report(&mut probe, Engine::WebKit, "typing", "Hello", true);
        assert!(
            probe
                .update(Engine::WebKit, "COEX:webkit:typing:550:ok:He:true")
                .is_err()
        );
    }

    #[test]
    fn cef_recheck_requires_unchanged_full_text_and_verified_click() {
        for (text, trusted) in [("", false), ("Hello", true), (TEXT, false)] {
            let mut probe = storage();
            input(&mut probe, Engine::WebKit, false);
            input(&mut probe, Engine::Cef, false);
            input(&mut probe, Engine::WebKit, true);
            action(&mut probe, Action::CefReport);
            // The exact old initial storage report is harmless, but cannot pass.
            let result = probe.update(
                Engine::Cef,
                &title(Engine::Cef, "storage", 550, text, trusted),
            );
            if text.is_empty() {
                assert!(result.is_ok());
            } else {
                assert!(result.is_err());
            }
            assert_eq!(probe.next_action(), None);
        }
    }

    #[test]
    fn resize_needs_exactly_600_from_both_roles_in_either_order() {
        for first in [Engine::Cef, Engine::WebKit] {
            let second = if first == Engine::Cef {
                Engine::WebKit
            } else {
                Engine::Cef
            };
            let mut probe = focus();
            action(&mut probe, Action::Resize);
            resize(&mut probe, first, 600);
            resize(&mut probe, first, 600);
            resize(&mut probe, second, 599);
            assert_eq!(probe.next_action(), None);
            resize(&mut probe, second, 600);
            action(&mut probe, Action::ResizeVerified);
        }
    }

    #[test]
    fn resize_requires_correct_text_isolation_and_verification() {
        for value in [
            "COEX:cef:resize:600:ok:Hello:true",
            "COEX:cef:resize:600:ok:Hello from Felinaut:false",
            "COEX:webkit:resize:600:ok:Hello from Felinaut:true",
            "COEX:webkit:resize:600:bad:Hello from Felinaut Meow:true",
        ] {
            let mut probe = focus();
            action(&mut probe, Action::Resize);
            let engine = if value.starts_with("COEX:cef:") {
                Engine::Cef
            } else {
                Engine::WebKit
            };
            assert!(probe.update(engine, value).is_err());
            assert_eq!(probe.next_action(), None);
        }
    }

    #[test]
    fn missing_events_cannot_complete_or_repeat_side_effects() {
        let mut probe = storage();
        for (click, engine, before, typed, after) in [
            (
                Action::WebKitClick(100., 150.),
                Engine::WebKit,
                "",
                Action::WebKitType(TEXT),
                TEXT,
            ),
            (
                Action::CefClick(100., 150.),
                Engine::Cef,
                "",
                Action::CefType(TEXT),
                TEXT,
            ),
            (
                Action::WebKitClick(100., 150.),
                Engine::WebKit,
                TEXT,
                Action::WebKitType(" Meow"),
                RETURN_TEXT,
            ),
        ] {
            action(&mut probe, click);
            for _ in 0..10 {
                assert_eq!(probe.next_action(), None);
            }
            report(&mut probe, engine, "focus", before, true);
            action(&mut probe, typed);
            for _ in 0..10 {
                assert_eq!(probe.next_action(), None);
            }
            report(&mut probe, engine, "typing", after, true);
            action(
                &mut probe,
                if engine == Engine::Cef {
                    Action::CefClick(110., 220.)
                } else {
                    Action::WebKitClick(110., 220.)
                },
            );
            for _ in 0..10 {
                assert_eq!(probe.next_action(), None);
            }
            report(&mut probe, engine, "input", after, true);
        }
        action(&mut probe, Action::CefReport);
        for _ in 0..10 {
            assert_eq!(probe.next_action(), None);
        }
        report(&mut probe, Engine::Cef, "storage", TEXT, true);
        action(&mut probe, Action::FocusVerified);
        action(&mut probe, Action::Resize);
        resize(&mut probe, Engine::Cef, 600);
        for _ in 0..10 {
            assert_eq!(probe.next_action(), None);
        }
    }

    #[test]
    fn distinct_report_budget_is_bounded_and_failure_latches() {
        let mut probe = Probe::new();
        for width in 1..=MAX_REPORTS as u16 {
            probe
                .update(Engine::Cef, &title(Engine::Cef, "ready", width, "", false))
                .unwrap();
        }
        assert!(
            probe
                .update(
                    Engine::Cef,
                    &title(Engine::Cef, "ready", MAX_REPORTS as u16 + 1, "", false)
                )
                .is_err()
        );
        assert_eq!(probe.next_action(), None);
    }
}
