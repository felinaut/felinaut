//! The camera and microphone prompt, the toolbar's site permission button and the capture
//! indicator of a tab.
//!
//! The prompt is a bar between the toolbar and the page. GPUI content drawn over the page
//! would sit behind Wry's child NSView, so the bar takes a row of its own and the page moves
//! down. No key grants by accident: the bar never takes the keys on its own, it starts with
//! no choice selected, and an allow choice counts only some time after the last key press.

use std::time::{Duration, Instant};

use gpui::{
    App, Context, Div, FontWeight, KeyBinding, MouseButton, Stateful, Window, actions, div,
    prelude::*, px, rgb,
};

use crate::{
    Browser, brand, history,
    permissions::{self, Decision, native},
    theme::Palette,
};

actions!(
    permissions,
    [
        NotNow,
        NextChoice,
        PreviousChoice,
        ConfirmChoice,
        FocusPermissionPrompt,
        ResetSitePermissions
    ]
);

const CONTEXT: &str = "FelinautPermission";
/// A new bar moves the page down, under a pointer that was about to click. An allow choice
/// counts only after this time, so such a click cannot allow by accident. Each key press on
/// the bar starts the time again, so a fast run of keys cannot allow either.
const ALLOW_DELAY: Duration = Duration::from_millis(500);

const BAR_HEIGHT: f32 = 64.;
const BAR_PADDING: f32 = 16.;
const ORIGIN_TEXT: f32 = 13.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    NotNow,
    Never,
    Once,
    Always,
}

/// Left to right, the safe choices first.
const CHOICES: [(Choice, &str); 4] = [
    (Choice::NotNow, "Not Now"),
    (Choice::Never, "Never Allow"),
    (Choice::Once, "Allow Once"),
    (Choice::Always, "Always Allow"),
];

impl Choice {
    /// Whether the request is granted, and the decision to store for later requests.
    fn outcome(self) -> (bool, Option<Decision>) {
        match self {
            Self::NotNow => (false, None),
            Self::Never => (false, Some(Decision::Deny)),
            Self::Once => (true, None),
            Self::Always => (true, Some(Decision::Allow)),
        }
    }

    /// Whether a click or key counts, `age` after the prompt appeared or got its last key.
    fn counts(self, age: Duration) -> bool {
        !self.outcome().0 || age >= ALLOW_DELAY
    }
}

/// The choice that Tab or an arrow key selects next. The first key selects the safest end.
/// A step back from the safest end stays there: it never wraps to the allow choices.
fn step(selected: Option<usize>, forward: bool) -> usize {
    match (selected, forward) {
        (None, _) => 0,
        (Some(index), true) => (index + 1) % CHOICES.len(),
        (Some(index), false) => index.saturating_sub(1),
    }
}

/// The choice that the keys selected, and the time from which an allow choice counts.
struct Selection {
    index: Option<usize>,
    since: Instant,
}

impl Selection {
    fn new(now: Instant) -> Self {
        Self {
            index: None,
            since: now,
        }
    }

    /// Tab or an arrow key. The wait for an allow choice starts again.
    fn step(&mut self, forward: bool, now: Instant) {
        self.index = Some(step(self.index, forward));
        self.since = now;
    }

    /// Return: the selected choice, if it counts now. A Return that comes too early clears
    /// the selection and starts the wait again, so a second Return cannot confirm it.
    fn confirm(&mut self, now: Instant) -> Option<Choice> {
        let choice = CHOICES[self.index?].0;
        if choice.counts(now.duration_since(self.since)) {
            return Some(choice);
        }
        *self = Self::new(now);
        None
    }
}

/// The request on display and the choice that Return confirms.
pub struct Prompt {
    request: native::Request,
    selection: Selection,
}

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", NotNow, context),
        KeyBinding::new("tab", NextChoice, context),
        KeyBinding::new("right", NextChoice, context),
        KeyBinding::new("shift-tab", PreviousChoice, context),
        KeyBinding::new("left", PreviousChoice, context),
        // Space is not here: pages use it a lot, and only Return confirms.
        KeyBinding::new("enter", ConfirmChoice, context),
        KeyBinding::new("cmd-shift-p", FocusPermissionPrompt, None),
    ]);
}

impl Browser {
    /// Answers the waiting requests that have a stored decision, picks the request to show,
    /// reads each tab's capture state and saves changed decisions. Runs on the 100 ms poll.
    /// True if something on display changed.
    pub(crate) fn poll_permissions(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            let capture = tab
                .page
                .as_ref()
                .map_or((false, false), |page| page.capture());
            changed |= tab.capture != capture;
            tab.capture = capture;
        }
        // Only a page on display gets a prompt. A request of another tab waits for its turn.
        let shown = [Some(self.active), self.partner(self.active)];
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| permissions::TabState {
                page: tab.page.as_ref().map(|page| page.permission_key()),
                url: &tab.url,
                on_display: shown.contains(&Some(index)) && !tab.home && tab.load_failure.is_none(),
            })
            .collect();
        let routed = permissions::route(native::requests(), &tabs, &self.permissions);
        for (id, grant) in routed.answers {
            native::answer(id, grant);
        }
        let next = routed.prompt;
        let current = self
            .permission_prompt
            .as_ref()
            .map(|prompt| prompt.request.id);
        if next.as_ref().map(|request| request.id) != current {
            changed = true;
            // A new bar never takes the keys: they stay with the page, so a key that was
            // meant for the page cannot answer. The user moves to the bar with a click or
            // with ⌘⇧P. The keys return to the page when the bar goes away.
            if next.is_none() && self.permission_focus.is_focused(window) {
                window.focus(&self.focus);
                let tab = &self.tabs[self.active];
                if let Some(page) = tab.page.as_ref().filter(|_| !tab.home) {
                    let _ = page.focus();
                }
            }
            self.permission_prompt = next.map(|request| Prompt {
                request,
                selection: Selection::new(Instant::now()),
            });
        }
        if let Some((path, contents)) = self.permissions.snapshot_due(Instant::now()) {
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = history::write(&path, &contents) {
                        eprintln!("Could not save site permissions: {error}");
                    }
                })
                .detach();
        }
        changed
    }

    /// A click on a choice, or an agent's answer. An allow choice that comes too early does
    /// nothing.
    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let early = self
            .permission_prompt
            .as_ref()
            .is_some_and(|prompt| !choice.counts(prompt.selection.since.elapsed()));
        if !early {
            self.decide(choice, window, cx);
        }
    }

    /// Answers the request on display.
    fn decide(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Prompt { request, .. }) = &self.permission_prompt else {
            return;
        };
        let (grant, decision) = choice.outcome();
        if choice == Choice::NotNow {
            self.permissions.dismiss(&request.origin);
        }
        if let Some(decision) = decision {
            self.permissions
                .remember(&request.origin, request.kinds, decision);
        }
        native::answer(request.id, grant);
        self.poll_permissions(window, cx);
        cx.notify();
    }

    fn step_choice(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(prompt) = &mut self.permission_prompt {
            prompt.selection.step(forward, Instant::now());
            cx.notify();
        }
    }

    /// ⌘⇧P or a click on the bar: the keys go to the bar. Only the user moves them there.
    pub(crate) fn focus_permission_prompt(
        &mut self,
        _: &FocusPermissionPrompt,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        if self.permission_prompt.is_none() {
            return;
        }
        if let Some(page) = &self.tabs[self.active].page {
            let _ = page.focus_parent();
        }
        window.focus(&self.permission_focus);
    }

    /// The prompt for the request on display, if there is one.
    pub(crate) fn permission_bar(
        &self,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let prompt = self.permission_prompt.as_ref()?;
        let buttons = CHOICES
            .into_iter()
            .enumerate()
            .map(|(index, (choice, label))| {
                let selected = prompt.selection.index == Some(index);
                div()
                    .id(("permission-choice", index))
                    .h(px(28.))
                    .px_2()
                    .flex_shrink_0()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if selected { p.primary } else { p.border }))
                    .bg(rgb(if selected { p.elevated } else { p.background }))
                    .flex()
                    .items_center()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(p.elevated)))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.choose(choice, window, cx)),
                    )
                    .child(label)
            });
        let line = || div().overflow_hidden().text_ellipsis().whitespace_nowrap();
        Some(
            div()
                .id("permission-prompt")
                .key_context(CONTEXT)
                .track_focus(&self.permission_focus)
                .on_action(cx.listener(|this, _: &NotNow, window, cx| {
                    this.choose(Choice::NotNow, window, cx);
                }))
                .on_action(cx.listener(|this, _: &NextChoice, _, cx| this.step_choice(true, cx)))
                .on_action(
                    cx.listener(|this, _: &PreviousChoice, _, cx| this.step_choice(false, cx)),
                )
                .on_action(cx.listener(|this, _: &ConfirmChoice, window, cx| {
                    let confirmed = this
                        .permission_prompt
                        .as_mut()
                        .and_then(|prompt| prompt.selection.confirm(Instant::now()));
                    match confirmed {
                        Some(choice) => this.decide(choice, window, cx),
                        // An early Return cleared the selection: show that.
                        None => cx.notify(),
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.focus_permission_prompt(&FocusPermissionPrompt, window, cx);
                    }),
                )
                .h(px(BAR_HEIGHT))
                .flex_shrink_0()
                .px(px(BAR_PADDING))
                .flex()
                .flex_col()
                .justify_center()
                .gap_1()
                .bg(rgb(p.surface))
                .border_b_1()
                .border_color(rgb(p.border))
                // The origin has a row of its own and never shrinks, so its end, which names
                // the owner of the site, is always on display. Only the key hint is cut.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .text_size(px(ORIGIN_TEXT))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(permissions::display_origin(&prompt.request.origin)),
                        )
                        .child(
                            line()
                                .flex_1()
                                .min_w_0()
                                .text_right()
                                .text_size(px(11.))
                                .text_color(rgb(p.muted))
                                .child("⌘⇧P for keys: ⇥ choose  ·  ↩ confirm  ·  Esc not now"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(line().flex_1().min_w_0().text_size(px(11.)).child(format!(
                            "wants your {}",
                            permissions::describe(prompt.request.kinds)
                        )))
                        .children(buttons),
                ),
        )
    }

    /// The origin of the active tab's page, if it could ask.
    fn active_origin(&self) -> Option<String> {
        let tab = &self.tabs[self.active];
        (!tab.home)
            .then(|| permissions::origin_of_url(&tab.url))
            .flatten()
    }

    /// The toolbar button for the active site: what it uses now, or its stored decisions.
    /// A click stops the use and forgets the decisions.
    pub(crate) fn permission_button(
        &self,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let label = match self.tabs[self.active].capture {
            (true, true) => "● Camera and microphone in use".to_owned(),
            (true, false) => "● Camera in use".to_owned(),
            (false, true) => "● Microphone in use".to_owned(),
            (false, false) => self.permissions.summary(&self.active_origin()?)?,
        };
        Some(
            self.button("site-permissions", "", p)
                .flex_shrink_0()
                .text_size(px(12.))
                .text_color(rgb(p.text))
                .on_hover(Self::hint_on_hover(
                    "Stop camera and microphone use and forget this site's decisions",
                    cx,
                ))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.reset_site_permissions(&ResetSitePermissions, window, cx);
                }))
                .child(format!("{label}  ×")),
        )
    }

    /// Forgets the active site's decisions and stops its use of the camera and the microphone,
    /// in each tab that shows the site.
    pub(crate) fn reset_site_permissions(
        &mut self,
        _: &ResetSitePermissions,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(page) = &self.tabs[self.active].page {
            page.stop_capture();
        }
        if let Some(origin) = self.active_origin() {
            self.permissions.revoke(&origin);
            for tab in &self.tabs {
                if let Some(page) = &tab.page
                    && permissions::origin_of_url(&tab.url).as_ref() == Some(&origin)
                {
                    page.stop_capture();
                }
            }
        }
        cx.notify();
    }

    /// Agent control's `permissions`: the request on display, the active site's stored
    /// decisions and the active tab's capture, after an answer or a reset if asked. An agent
    /// can refuse, block and reset. It cannot allow: only the user grants.
    pub(crate) fn agent_permissions(
        &mut self,
        command: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, String> {
        let choice = match command["answer"].as_str() {
            None => None,
            Some("deny") => Some(Choice::NotNow),
            Some("block") => Some(Choice::Never),
            Some(_) => return Err("An agent can only deny or block a request".into()),
        };
        if let Some(choice) = choice {
            if self.permission_prompt.is_none() {
                return Err("No site asks for a permission".into());
            }
            self.choose(choice, window, cx);
        }
        if command["reset"].as_bool() == Some(true) {
            self.reset_site_permissions(&ResetSitePermissions, window, cx);
        }
        let (camera, microphone) = self.tabs[self.active].capture;
        Ok(serde_json::json!({
            "request": self.permission_prompt.as_ref().map(|prompt| serde_json::json!({
                "origin": prompt.request.origin,
                "wants": permissions::describe(prompt.request.kinds),
            })),
            "site": self.active_origin().and_then(|origin| self.permissions.summary(&origin)),
            "camera": camera,
            "microphone": microphone,
        }))
    }

    /// A tab's camera or microphone icon while its page captures. A click stops the capture.
    pub(crate) fn capture_control(
        &self,
        index: usize,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let tab = &self.tabs[index];
        let (hint, icon) = match tab.capture {
            (true, true) => (
                "Tab uses your camera and microphone · Click to stop",
                brand::icon::CAMERA,
            ),
            (true, false) => ("Tab uses your camera · Click to stop", brand::icon::CAMERA),
            (false, true) => (
                "Tab uses your microphone · Click to stop",
                brand::icon::MICROPHONE,
            ),
            (false, false) => return None,
        };
        let page = tab.page.clone()?;
        Some(
            Self::row_control(("tab-capture", tab.id), p)
                .on_hover(Self::hint_on_hover(hint, cx))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    page.stop_capture();
                })
                .child(brand::icon(icon).size(px(14.)).text_color(rgb(p.primary))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn only_the_allow_choices_grant_and_only_the_lasting_ones_are_stored() {
        assert_eq!(Choice::NotNow.outcome(), (false, None));
        assert_eq!(Choice::Never.outcome(), (false, Some(Decision::Deny)));
        assert_eq!(Choice::Once.outcome(), (true, None));
        assert_eq!(Choice::Always.outcome(), (true, Some(Decision::Allow)));
    }

    #[test]
    fn an_allow_choice_does_not_count_on_a_bar_that_just_appeared() {
        let early = ALLOW_DELAY - Duration::from_millis(1);
        for (choice, _) in CHOICES {
            let grants = choice.outcome().0;
            assert_eq!(choice.counts(Duration::ZERO), !grants, "{choice:?}");
            assert_eq!(choice.counts(early), !grants, "{choice:?}");
            assert!(choice.counts(ALLOW_DELAY), "{choice:?}");
        }
    }

    #[test]
    fn keys_start_at_the_safe_end_and_never_wrap_back_to_an_allow_choice() {
        // No choice is selected at first, so Return confirms nothing.
        assert_eq!(CHOICES[step(None, true)].0, Choice::NotNow);
        assert_eq!(CHOICES[step(None, false)].0, Choice::NotNow);
        assert_eq!(step(Some(0), true), 1);
        assert_eq!(step(Some(2), false), 1);
        // Forward from Always goes to the safe end. Back from the safe end stays there.
        assert_eq!(CHOICES[step(Some(3), true)].0, Choice::NotNow);
        assert_eq!(CHOICES[step(Some(0), false)].0, Choice::NotNow);
        let mut selected = None;
        for _ in 0..10 {
            selected = Some(step(selected, false));
            assert_eq!(selected, Some(0));
        }
    }

    #[test]
    fn the_longest_origin_fits_in_the_narrowest_bar() {
        // The widest glyph of an origin is "m": 11.75 pt in the system font at 13 pt
        // semibold (measured with AppKit). A host is lower case, digits, "-", "." and ":".
        const WIDEST_GLYPH: f32 = 11.75 / 13.;
        let narrowest = crate::WINDOW_MIN_WIDTH - crate::theme::SIDEBAR_MAX - 2. * BAR_PADDING;
        let widest = permissions::ORIGIN_MAX as f32 * WIDEST_GLYPH * ORIGIN_TEXT;
        assert!(widest <= narrowest, "{widest} > {narrowest}");
        // `display_origin` never gives more than `ORIGIN_MAX` characters.
        let long = format!("https://{}.example:8443", "m".repeat(300));
        let shown = permissions::display_origin(&long);
        assert_eq!(shown.chars().count(), permissions::ORIGIN_MAX);
        assert!(shown.ends_with(".example:8443"));
    }

    #[test]
    fn keys_meant_for_the_page_never_allow() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let late = 10 * ALLOW_DELAY.as_millis() as u64;

        // Return alone confirms nothing, at any time.
        let mut keys = Selection::new(start);
        assert_eq!(keys.confirm(at(late)), None);

        // ← ← and a confirm: the selection is the safe end, so the answer is a refusal.
        let mut keys = Selection::new(start);
        keys.step(false, at(late));
        keys.step(false, at(late + 1));
        assert_eq!(keys.confirm(at(late + 2)), Some(Choice::NotNow));

        // A fast run of keys that ends on an allow choice: the confirm comes too early. It
        // clears the selection, so one more confirm does nothing either.
        for steps in [3, 4] {
            let mut keys = Selection::new(start);
            for _ in 0..steps {
                keys.step(true, at(late));
            }
            assert!(CHOICES[keys.index.unwrap()].0.outcome().0);
            assert_eq!(keys.confirm(at(late + 499)), None);
            assert_eq!(keys.index, None);
            assert_eq!(keys.confirm(at(late + 5000)), None);
        }

        // Each key press starts the wait again, also when the bar is old.
        let mut keys = Selection::new(start);
        keys.step(true, at(0));
        keys.step(true, at(0));
        keys.step(true, at(late));
        assert_eq!(keys.confirm(at(late + 499)), None);
        // An early confirm starts the wait again too: it also holds back a click.
        assert_eq!(keys.since, at(late + 499));
    }

    #[test]
    fn a_selected_allow_choice_counts_after_the_wait() {
        let start = Instant::now();
        let mut keys = Selection::new(start);
        for _ in 0..3 {
            keys.step(true, start);
        }
        assert_eq!(keys.confirm(start + ALLOW_DELAY), Some(Choice::Once));
        keys.step(true, start);
        assert_eq!(keys.confirm(start + ALLOW_DELAY), Some(Choice::Always));
        // The refusals count at once.
        let mut keys = Selection::new(start);
        keys.step(true, start);
        keys.step(true, start);
        assert_eq!(keys.confirm(start), Some(Choice::Never));
    }
}
