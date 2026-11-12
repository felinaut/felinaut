//! The panel for a page's JavaScript dialog (`alert`, `confirm`, `prompt`).
//!
//! A GPUI overlay would sit behind Wry's child NSView, and a popup window would cover other
//! tabs. So the panel is a strip of browser chrome above the page: the page moves down and
//! cannot draw in the strip. Each tab has its own panel, drawn only while that tab is on show.

use std::rc::Rc;

use gpui::{
    App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, KeyBinding, Stateful,
    Subscription, Window, actions, div, prelude::*, px, rgb,
};

use crate::{
    Browser, brand,
    dialogs::{Kind, Reply, Shown},
    input::{AddressEvent, AddressInput, Submit},
    theme::{self, Palette},
    webview::NativePage,
};

actions!(dialogs, [Accept, Cancel]);

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
    _input_events: Option<[Subscription; 2]>,
}

impl Panel {
    fn new(page: Rc<NativePage>, shown: Shown, cx: &mut Context<Self>) -> Self {
        let input = (shown.request.kind == Kind::Prompt).then(|| {
            cx.new(|cx| {
                let mut input = AddressInput::new(cx);
                input.set_placeholder("");
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
        Self {
            page,
            shown,
            focus: cx.focus_handle(),
            input,
            suppress: false,
            answered: false,
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
        if self.answered {
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
            // from the page.
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(p.muted))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
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
                            .on_click(cx.listener(|this, _, _, cx| this.answer(true, cx))),
                    ),
            )
    }
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
        let shown = page.as_ref().and_then(|page| page.dialog());
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
        match (&tab.dialog, page) {
            // A dialog of a background tab waits, without the focus, until its tab is selected.
            (Some(panel), _) if active || had_focus => panel.read(cx).take_focus(window, cx),
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
