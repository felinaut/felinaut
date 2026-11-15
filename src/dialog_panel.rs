//! The panel for a page's JavaScript dialog (`alert`, `confirm`, `prompt`).
//!
//! A GPUI overlay would sit behind Wry's child NSView, and a popup window would cover other
//! tabs. So the panel is a strip of browser chrome above the page: the page moves down and
//! cannot draw in the strip. Each tab has its own panel, drawn only while that tab is on show.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, KeyBinding, Stateful,
    Subscription, Task, Timer, Window, actions, div, prelude::*, px, rgb,
};

use crate::{
    Browser, auth_prompt, brand,
    dialogs::{Kind, Reply, Shown},
    input::{AddressEvent, AddressInput, Submit},
    theme::{self, Palette},
    webview::NativePage,
};

actions!(dialogs, [Accept, Cancel]);

/// How long OK takes no click and no `↩` after a panel shows. The page picks the moment its
/// dialog opens, and the panel moves the page down: without the wait, a click or a `↩` meant
/// for the page could answer the dialog. Cancel and `Esc` work at once.
const ACCEPT_DELAY: Duration = Duration::from_millis(500);

/// Whether OK works at `now` for a panel that showed at `shown_at`.
fn accepts(shown_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(shown_at) >= ACCEPT_DELAY
}

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Accept, Some("JsDialog")),
        KeyBinding::new("escape", Cancel, Some("JsDialog")),
    ]);
}

pub struct Panel {
    page: Rc<NativePage>,
    shown: Shown,
    focus: FocusHandle,
    /// The text field of `prompt()`.
    input: Option<Entity<AddressInput>>,
    /// "Do not let this page show more dialogs" is checked.
    suppress: bool,
    /// The answer is sent. The panel draws nothing until the browser's poll removes it.
    answered: bool,
    /// When the panel showed; OK works `ACCEPT_DELAY` later.
    shown_at: Instant,
    /// Draws OK again when it starts to work.
    _ready: Task<()>,
    _input_events: Option<[Subscription; 2]>,
}

impl Panel {
    fn new(page: Rc<NativePage>, shown: Shown, cx: &mut Context<Self>) -> Self {
        let input = (shown.request.kind == Kind::Prompt).then(|| {
            cx.new(|cx| {
                let mut input = AddressInput::field("", false, cx);
                input.set_text(shown.request.default_text.clone(), cx);
                input.select_all_text(cx);
                input
            })
        });
        // The field takes Enter and Escape itself and reports them.
        let input_events = input.as_ref().map(|input| {
            [
                cx.subscribe(input, |this, _, _: &Submit, cx| this.answer(true, cx)),
                cx.subscribe(input, |this, _, event: &AddressEvent, cx| {
                    if matches!(event, AddressEvent::Escape) {
                        this.answer(false, cx);
                    }
                }),
            ]
        });
        let ready = cx.spawn(async move |panel, cx| {
            Timer::after(ACCEPT_DELAY).await;
            let _ = panel.update(cx, |_, cx| cx.notify());
        });
        Self {
            page,
            shown,
            focus: cx.focus_handle(),
            input,
            suppress: false,
            answered: false,
            shown_at: Instant::now(),
            _ready: ready,
            _input_events: input_events,
        }
    }

    /// Gives the keys to the panel: Enter is OK and Escape is Cancel.
    fn take_focus(&self, window: &mut Window, cx: &App) {
        // Otherwise the native page keeps the keys.
        let _ = self.page.focus_parent();
        match &self.input {
            Some(input) => window.focus(&input.read(cx).focus_handle(cx)),
            None => window.focus(&self.focus),
        }
    }

    fn answer(&mut self, accepted: bool, cx: &mut Context<Self>) {
        // Every way to OK comes here: the button, `↩` on the panel and `↩` in the field.
        if self.answered || (accepted && !accepts(self.shown_at, Instant::now())) {
            return;
        }
        self.answered = true;
        let text = self
            .input
            .as_ref()
            .map(|input| input.read(cx).text().to_owned())
            .unwrap_or_default();
        self.page
            .answer_dialog(self.shown.id, Reply { accepted, text }, self.suppress);
        cx.notify();
    }
}

fn button(id: &'static str, label: &'static str, primary: bool, p: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .flex_shrink_0()
        .rounded_lg()
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .border_1()
        .border_color(rgb(if primary { p.primary } else { p.border }))
        .bg(rgb(if primary { p.primary } else { p.elevated }))
        .text_color(rgb(if primary { p.background } else { p.text }))
        .hover(|style| style.opacity(0.85))
        .child(label)
}

impl Render for Panel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.answered {
            return div().id("js-dialog");
        }
        let p = theme::palette(cx);
        let request = &self.shown.request;
        let suppress = self.suppress;
        let ready = accepts(self.shown_at, Instant::now());
        div()
            .id("js-dialog")
            .key_context("JsDialog")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Accept, _, cx| this.answer(true, cx)))
            .on_action(cx.listener(|this, _: &Cancel, _, cx| this.answer(false, cx)))
            .w_full()
            .flex_shrink_0()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(p.surface))
            .border_b_2()
            .border_color(rgb(p.primary))
            // The title is the browser's text: the origin of the frame, and nothing else
            // from the page. It wraps, because the end of a long host names its owner and
            // an ellipsis at the end of one line hid exactly that part.
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(p.muted))
                    .child(request.title()),
            )
            .when(!request.message.is_empty(), |panel| {
                panel.child(
                    div()
                        .id("js-dialog-message")
                        .max_h(px(160.))
                        .overflow_y_scroll()
                        .text_sm()
                        .text_color(rgb(p.text))
                        .child(request.message.clone()),
                )
            })
            .children(self.input.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.shown.can_suppress, |row| {
                        row.child(
                            div()
                                .id("js-dialog-suppress")
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .text_sm()
                                .text_color(rgb(p.muted))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.suppress = !this.suppress;
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .size(px(14.))
                                        .flex_shrink_0()
                                        .rounded(px(4.))
                                        .border_1()
                                        .border_color(rgb(if suppress {
                                            p.primary
                                        } else {
                                            p.muted
                                        }))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .when(suppress, |mark| {
                                            mark.bg(rgb(p.primary)).child(
                                                brand::icon(brand::icon::CHECK)
                                                    .size(px(10.))
                                                    .text_color(rgb(p.background)),
                                            )
                                        }),
                                )
                                .child("Do not let this page show more dialogs"),
                        )
                    })
                    .child(div().flex_1())
                    .when(request.kind != Kind::Alert, |row| {
                        row.child(
                            button("js-dialog-cancel", "Cancel  esc", false, p)
                                .on_click(cx.listener(|this, _, _, cx| this.answer(false, cx))),
                        )
                    })
                    .child(
                        button("js-dialog-ok", "OK  ↩", true, p)
                            .when(!ready, |button| button.opacity(0.5).cursor_default())
                            .on_click(cx.listener(|this, _, _, cx| this.answer(true, cx))),
                    ),
            )
    }
}

/// Whether a new panel takes the keys. A dialog of a background tab waits without them. A
/// panel never takes them from a field of the browser, such as the address bar: the page
/// decides when its dialog opens, and the text the user types there must not go to the page.
fn takes_focus(active: bool, panel_had_focus: bool, chrome_has_focus: bool) -> bool {
    panel_had_focus || (active && !chrome_has_focus)
}

/// One question for each tab at a time, and the sign-in prompt comes first: it is about a load
/// in progress, and it covers the page. The dialog waits in its queue, with no panel, until the
/// sign-in prompt goes away. So a panel that is not drawn never has the keys, and the wait
/// before OK works starts when the panel really shows.
fn after_sign_in<T>(dialog: Option<T>, sign_in_open: bool) -> Option<T> {
    dialog.filter(|_| !sign_in_open)
}

/// Makes each tab's panel match the dialog its page shows now. The browser's poll calls this,
/// outside rendering. Gives whether a panel came or went.
pub fn sync(browser: &mut Browser, window: &mut Window, cx: &mut Context<Browser>) -> bool {
    let mut changed = false;
    for index in 0..browser.tabs.len() {
        let active = index == browser.active;
        let tab = &mut browser.tabs[index];
        let page = tab.page.clone();
        // Nobody can answer a page that is under the New Tab page or an error page.
        if let Some(page) = page
            .as_ref()
            .filter(|_| tab.home || tab.load_failure.is_some())
        {
            page.dismiss_dialogs();
        }
        let shown = page
            .as_ref()
            .and_then(|page| after_sign_in(page.dialog(), auth_prompt::view(page, cx).is_some()));
        let current = tab.dialog.as_ref().map(|panel| panel.read(cx));
        let same = match (&shown, current, &page) {
            (None, None, _) => true,
            (Some(shown), Some(panel), Some(page)) => {
                shown.id == panel.shown.id && Rc::ptr_eq(page, &panel.page)
            }
            _ => false,
        };
        if same {
            continue;
        }
        changed = true;
        let had_focus = tab
            .dialog
            .take()
            .is_some_and(|panel| panel.read(cx).focus.contains_focused(window, cx));
        tab.dialog = page
            .clone()
            .zip(shown)
            .map(|(page, shown)| cx.new(|cx| Panel::new(page, shown, cx)));
        // The browser's own handle has the focus when the page has the keys, or nothing has.
        let chrome_has_focus = window
            .focused(cx)
            .is_some_and(|focused| focused != browser.focus);
        match (&tab.dialog, page) {
            (Some(panel), _) if takes_focus(active, had_focus, chrome_has_focus) => {
                panel.read(cx).take_focus(window, cx)
            }
            // The answer is given: the keys go back to the page.
            (None, Some(page)) if had_focus => {
                window.focus(&browser.focus);
                let _ = page.focus();
            }
            (None, None) if had_focus => window.focus(&browser.focus),
            _ => {}
        }
    }
    changed
}

/// Gives the keys to the active tab's dialog, if it shows one. Call when a tab is selected.
pub fn focus(browser: &Browser, window: &mut Window, cx: &App) {
    if let Some(panel) = &browser.tabs[browser.active].dialog {
        panel.read(cx).take_focus(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{ACCEPT_DELAY, accepts, after_sign_in, takes_focus};
    use std::time::{Duration, Instant};

    #[test]
    fn ok_works_only_after_the_panel_has_shown_for_a_while() {
        let shown_at = Instant::now();
        let after = |delay: Duration| accepts(shown_at, shown_at + delay);
        // A click or `↩` meant for the page, at the moment the panel shows.
        assert!(!after(Duration::ZERO));
        assert!(!after(ACCEPT_DELAY - Duration::from_millis(1)));
        assert!(after(ACCEPT_DELAY));
        assert!(after(Duration::from_secs(60)));
        // A clock reading from before the panel showed never counts as a wait.
        assert!(!accepts(shown_at + ACCEPT_DELAY, shown_at));
    }

    #[test]
    fn a_dialog_waits_for_the_sign_in_prompt_of_its_tab() {
        assert_eq!(after_sign_in(Some("dialog"), true), None);
        assert_eq!(after_sign_in(Some("dialog"), false), Some("dialog"));
        assert_eq!(after_sign_in(None::<&str>, false), None);
        assert_eq!(after_sign_in(None::<&str>, true), None);
    }

    #[test]
    fn a_panel_takes_the_keys_only_from_the_page_or_from_another_panel() {
        // The active tab's page, or nothing, has the keys.
        assert!(takes_focus(true, false, false));
        // The user types in the address bar: the panel shows without the keys.
        assert!(!takes_focus(true, false, true));
        // A dialog of a background tab waits, whatever has the keys.
        assert!(!takes_focus(false, false, false));
        assert!(!takes_focus(false, false, true));
        // The next dialog of a queue follows a panel that had the keys. That panel is not
        // the browser's own handle, so it counts as a field of the browser too.
        assert!(takes_focus(true, true, true));
        assert!(takes_focus(false, true, true));
    }
}
