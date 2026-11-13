//! The sign-in prompt for HTTP Basic and Digest authentication.
//!
//! GPUI cannot draw over a native page. So the prompt takes the place of the page in its tab,
//! as the error page does, and the page stays hidden until the user answers. The prompt
//! belongs to one tab: other tabs and other apps stay usable while it waits.

use std::collections::HashMap;

use gpui::{
    App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Global, KeyBinding,
    SharedString, Stateful, Subscription, Window, actions, div, prelude::*, px, rgb,
};

use crate::{
    http_auth::{self, Challenge},
    input::{AddressEvent, AddressInput, Submit},
    theme,
    webview::NativePage,
};

actions!(auth_prompt, [NextField]);

/// The prompt on display for each page, by the page's key, with the id of its question.
#[derive(Default)]
struct Open(HashMap<usize, (u64, Entity<AuthPrompt>)>);
impl Global for Open {}

/// Makes the prompts match the open questions. Call this from the browser's poll for each
/// tab's page; `active` says whether the user looks at that tab, and `browser` is the focus of
/// the browser itself. Returns true when what a tab shows changed.
pub fn sync(
    page: &NativePage,
    active: bool,
    browser: &FocusHandle,
    window: &Window,
    cx: &mut App,
) -> bool {
    let Some(key) = page.key() else {
        return false;
    };
    // When its prompt closes, the page takes the keys back only from the prompt, the browser
    // or nothing. A prompt can close while the user types in the address bar (the hidden page
    // loads another address, or its load fails), and the text must stay there.
    let keys_free = window.focused(cx).is_none_or(|focused| {
        focused == *browser
            || view(page, cx).is_some_and(|prompt| prompt.read(cx).has_keys(&focused, cx))
    });
    if !cx.has_global::<Open>() {
        cx.set_global(Open::default());
        let context = Some("AuthPrompt");
        cx.bind_keys([
            KeyBinding::new("tab", NextField, context),
            KeyBinding::new("shift-tab", NextField, context),
        ]);
    }
    let open = &mut cx.global_mut::<Open>().0;
    let (count, shown) = (open.len(), open.contains_key(&key));
    // An answered question, a closed tab or a new document ends a prompt. Its text fields,
    // and the password in them, go away with it.
    open.retain(|_, (id, _)| http_auth::is_waiting(*id));
    let mut changed = open.len() != count;
    if shown && !open.contains_key(&key) && active && keys_free {
        let _ = page.focus();
    }
    if !open.contains_key(&key)
        && let Some((id, challenge)) = http_auth::waiting(key)
    {
        let _ = page.set_visible(false);
        if active {
            // Keys go to GPUI only while the page is not AppKit's first responder.
            let _ = page.focus_parent();
        }
        let prompt = cx.new(|cx| AuthPrompt::new(id, challenge, cx));
        cx.global_mut::<Open>().0.insert(key, (id, prompt));
        changed = true;
    }
    changed
}

/// The prompt to draw in place of a page, if the page waits for a sign-in.
pub fn view(page: &NativePage, cx: &App) -> Option<Entity<AuthPrompt>> {
    let (_, prompt) = cx.try_global::<Open>()?.0.get(&page.key()?)?;
    Some(prompt.clone())
}

/// Gives the keys to the prompt of a page, if the page has one: the user name field takes the
/// focus. The browser calls this for the active tab, and only while no other input of the
/// chrome (the address bar, a menu) has the focus.
pub fn focus(page: &NativePage, window: &mut Window, cx: &mut App) {
    if let Some(prompt) = view(page, cx) {
        // Keys go to GPUI only while the page is not AppKit's first responder.
        let _ = page.focus_parent();
        window.focus(&prompt.read(cx).user.focus_handle(cx));
    }
}

pub struct AuthPrompt {
    id: u64,
    challenge: Challenge,
    user: Entity<AddressInput>,
    password: Entity<AddressInput>,
    answered: bool,
    _subscriptions: Vec<Subscription>,
}

impl AuthPrompt {
    fn new(id: u64, challenge: Challenge, cx: &mut Context<Self>) -> Self {
        let user = cx.new(|cx| AddressInput::field("User name", false, cx));
        let password = cx.new(|cx| AddressInput::field("Password", true, cx));
        let mut subscriptions = Vec::new();
        for field in [&user, &password] {
            // Enter in a field signs in; Escape cancels.
            subscriptions
                .push(cx.subscribe(field, |this, _, _: &Submit, cx| this.answer(true, cx)));
            subscriptions.push(cx.subscribe(field, |this, _, event: &AddressEvent, cx| {
                if matches!(event, AddressEvent::Escape) {
                    this.answer(false, cx);
                }
            }));
        }
        Self {
            id,
            challenge,
            user,
            password,
            answered: false,
            _subscriptions: subscriptions,
        }
    }

    /// Sends the answer to WebKit. The browser's next poll removes the prompt.
    fn answer(&mut self, sign_in: bool, cx: &mut Context<Self>) {
        if self.answered {
            return;
        }
        self.answered = true;
        let user = self.user.read(cx).text().to_owned();
        let password = self.password.read(cx).text().to_owned();
        http_auth::answer(self.id, sign_in.then_some((&user, &password)));
        self.password
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        cx.notify();
    }

    /// Whether `focused` is one of the fields of this prompt.
    fn has_keys(&self, focused: &FocusHandle, cx: &App) -> bool {
        [&self.user, &self.password]
            .iter()
            .any(|field| field.focus_handle(cx) == *focused)
    }

    fn next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        let next = if self.user.focus_handle(cx).is_focused(window) {
            &self.password
        } else {
            &self.user
        };
        window.focus(&next.focus_handle(cx));
    }
}

fn button(
    id: &'static str,
    label: &'static str,
    p: theme::Palette,
    primary: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_lg()
        .cursor_pointer()
        .font_weight(FontWeight::SEMIBOLD)
        .map(|button| {
            if primary {
                button.bg(rgb(p.primary)).text_color(rgb(p.background))
            } else {
                button
                    .border_1()
                    .border_color(rgb(p.border))
                    .text_color(rgb(p.text))
            }
        })
        .child(label)
}

impl Render for AuthPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let page = div().size_full().bg(rgb(p.background));
        if self.answered {
            return page.into_any_element();
        }
        let note = |text: SharedString| div().text_sm().text_color(rgb(p.muted)).child(text);
        let warning = |text: &'static str| {
            div()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(p.highlight))
                .text_sm()
                .text_color(rgb(p.text))
                .child(text)
        };
        let field = |label: &'static str, input: &Entity<AddressInput>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(note(label.into()))
                .child(input.clone())
        };
        let form = div()
            .w(px(420.))
            .max_w_full()
            .p_5()
            .rounded_xl()
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.border))
            .text_color(rgb(p.text))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child("Sign in"),
            )
            .child(note(
                "This site asks for a user name and a password:".into(),
            ))
            .child(
                // A long origin takes more lines. The end of a host name tells who owns it,
                // so the line must not cut it off.
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.challenge.origin()),
            )
            .when(!self.challenge.realm.is_empty(), |form| {
                // The realm is the site's own text. The quotes show that Felinaut does not say it.
                form.child(note(
                    format!("The site says: “{}”", self.challenge.realm).into(),
                ))
            })
            .when(!self.challenge.secure, |form| {
                form.child(warning(
                    "⚠ This connection is not secure. Other people on the network can read \
                     what you send to this site.",
                ))
            })
            .when(self.challenge.retry, |form| {
                form.child(warning(
                    "The site did not accept that user name and password.",
                ))
            })
            .child(field("User name", &self.user))
            .child(field("Password", &self.password))
            .child(
                div()
                    .mt_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child("⇥ next field  ·  esc cancel"),
                    )
                    .child(
                        button("auth-cancel", "Cancel", p, false)
                            .on_click(cx.listener(|this, _, _, cx| this.answer(false, cx))),
                    )
                    .child(
                        button("auth-sign-in", "Sign In  ↩", p, true)
                            .on_click(cx.listener(|this, _, _, cx| this.answer(true, cx))),
                    ),
            );
        page.id(("auth-prompt", self.id))
            .key_context("AuthPrompt")
            .on_action(cx.listener(Self::next_field))
            .overflow_y_scroll()
            .font_family(".AppleSystemUIFont")
            .text_size(px(14.))
            .flex()
            .flex_col()
            .items_center()
            .child(div().h(px(80.)).flex_shrink_0())
            .child(form)
            .into_any_element()
    }
}
