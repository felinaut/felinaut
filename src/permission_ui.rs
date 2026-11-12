//! The camera and microphone prompt and the capture indicator of a tab.
//!
//! The prompt is a bar between the toolbar and the page. GPUI content drawn over the page
//! would sit behind Wry's child NSView, so the bar takes a row of its own and the page moves
//! down. No key grants by accident: the bar starts with no choice selected.

use std::time::{Duration, Instant};

use gpui::{
    App, Context, Div, Focusable, FontWeight, KeyBinding, Stateful, Window, actions, div,
    prelude::*, px, rgb,
};

use crate::{
    Browser, brand, history,
    permissions::{self, Answer, Decision, native},
    theme::Palette,
};

actions!(
    permissions,
    [NotNow, NextChoice, PreviousChoice, ConfirmChoice]
);

const CONTEXT: &str = "FelinautPermission";
/// A new bar moves the page down, under a pointer that was about to click. An allow choice
/// counts only after this time, so such a click cannot allow by accident.
const ALLOW_DELAY: Duration = Duration::from_millis(500);

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

    /// Whether a click or key counts when the prompt has been on display for `age`.
    fn counts(self, age: Duration) -> bool {
        !self.outcome().0 || age >= ALLOW_DELAY
    }
}

/// The choice that Tab or an arrow key selects next. The first key selects the safest end.
fn step(selected: Option<usize>, forward: bool) -> usize {
    let last = CHOICES.len() - 1;
    match (selected, forward) {
        (None, _) => 0,
        (Some(index), true) => (index + 1) % CHOICES.len(),
        (Some(0), false) => last,
        (Some(index), false) => index - 1,
    }
}

/// The request on display and the choice that Return confirms.
pub struct Prompt {
    request: native::Request,
    selected: Option<usize>,
    shown: Instant,
}

pub fn init(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", NotNow, context),
        KeyBinding::new("tab", NextChoice, context),
        KeyBinding::new("right", NextChoice, context),
        KeyBinding::new("shift-tab", PreviousChoice, context),
        KeyBinding::new("left", PreviousChoice, context),
        KeyBinding::new("enter", ConfirmChoice, context),
        KeyBinding::new("space", ConfirmChoice, context),
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
        let mut next = None;
        for request in native::requests() {
            let tab = self.tabs.iter().enumerate().find(|(_, tab)| {
                tab.page
                    .as_ref()
                    .is_some_and(|page| page.permission_key() == request.page)
            });
            // The tab left the site that asked: the request is void. `tab.url` changes as
            // soon as a load starts, so a prompt never outlives its document's site.
            let Some((index, tab)) = tab.filter(|(_, tab)| {
                permissions::origin_of_url(&tab.url).as_ref() == Some(&request.origin)
            }) else {
                native::answer(request.id, false);
                continue;
            };
            match self.permissions.answer(&request.origin, request.kinds) {
                Answer::Grant => native::answer(request.id, true),
                Answer::Deny => native::answer(request.id, false),
                Answer::Ask => {
                    let on_display =
                        shown.contains(&Some(index)) && !tab.home && tab.load_failure.is_none();
                    if next.is_none() && on_display {
                        next = tab.page.clone().map(|page| (request, page));
                    }
                }
            }
        }
        let current = self
            .permission_prompt
            .as_ref()
            .map(|prompt| prompt.request.id);
        if next.as_ref().map(|(request, _)| request.id) != current {
            changed = true;
            let had_focus = self.permission_focus.is_focused(window);
            match &next {
                // Keys go to the bar, unless the user types an address.
                Some((_, page)) if !self.address.read(cx).focus_handle(cx).is_focused(window) => {
                    let _ = page.focus_parent();
                    window.focus(&self.permission_focus);
                }
                None if had_focus => {
                    window.focus(&self.focus);
                    let tab = &self.tabs[self.active];
                    if let Some(page) = tab.page.as_ref().filter(|_| !tab.home) {
                        let _ = page.focus();
                    }
                }
                _ => {}
            }
            self.permission_prompt = next.map(|(request, _)| Prompt {
                request,
                selected: None,
                shown: Instant::now(),
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

    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Prompt { request, shown, .. }) = &self.permission_prompt else {
            return;
        };
        if !choice.counts(shown.elapsed()) {
            return;
        }
        let (grant, decision) = choice.outcome();
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
            prompt.selected = Some(step(prompt.selected, forward));
            cx.notify();
        }
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
                let selected = prompt.selected == Some(index);
                div()
                    .id(("permission-choice", index))
                    .h(px(28.))
                    .px_3()
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
                    let selected = this.permission_prompt.as_ref().and_then(|p| p.selected);
                    if let Some(index) = selected {
                        this.choose(CHOICES[index].0, window, cx);
                    }
                }))
                .h(px(52.))
                .flex_shrink_0()
                .px_4()
                .flex()
                .items_center()
                .gap_2()
                .bg(rgb(p.surface))
                .border_b_1()
                .border_color(rgb(p.border))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            line()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(permissions::display_origin(&prompt.request.origin)),
                        )
                        .child(line().text_size(px(11.)).text_color(rgb(p.muted)).child(format!(
                            "wants to use your {}  ·  ⇥ choose  ·  ↩ confirm  ·  Esc not now",
                            permissions::describe(prompt.request.kinds)
                        ))),
                )
                .children(buttons),
        )
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
    use super::{ALLOW_DELAY, CHOICES, Choice, Decision, step};
    use std::time::Duration;

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
    fn keys_start_at_the_safe_end_and_wrap() {
        // No choice is selected at first, so Return confirms nothing.
        assert_eq!(CHOICES[step(None, true)].0, Choice::NotNow);
        assert_eq!(CHOICES[step(None, false)].0, Choice::NotNow);
        assert_eq!(step(Some(0), true), 1);
        assert_eq!(step(Some(3), true), 0);
        assert_eq!(step(Some(0), false), 3);
        assert_eq!(step(Some(2), false), 1);
    }
}
