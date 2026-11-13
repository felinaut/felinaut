//! The Cookies window: WebKit's stored cookies as a site → host → cookie graph, with details
//! and delete at every level. One window, independently closable. The host calls `init`.
//!
//! The tree and every decision about it live in `cookies.rs` (`WindowState`); `cookie_graph.rs` draws its
//! visible rows as a graph, and this file asks `cookie_store.rs` for the real cookies. Cookie values stay hidden until revealed and
//! are never logged or offered to agent control.

use std::collections::HashSet;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Bounds, Context, Div, Entity, FocusHandle, Focusable, Global, KeyBinding, SharedString,
    Stateful, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowOptions, actions, div, prelude::*, px, rgb, size,
};

use crate::{
    cookie_graph::CookieGraph,
    cookie_store,
    cookies::{self, BackOut, Cookie, Follow, Key, Node, WindowState},
    input::{AddressEvent, AddressInput},
    theme::{self, Palette, ThemeSettings},
};

actions!(
    cookies,
    [
        ShowCookies,
        CloseCookies,
        Dismiss,
        RefreshCookies,
        FindCookies,
        DeleteSelection,
        Previous,
        Next,
        Collapse,
        Expand,
        Activate,
    ]
);

const CONTEXT: &str = "FelinautCookies";

#[derive(Default)]
struct CookiesWindow {
    // GPUI window handles do not keep their window (or root entity) alive.
    handle: Option<WindowHandle<CookieManager>>,
}
impl Global for CookiesWindow {}

/// Registers the menu action, its shortcut and the window's own keys.
pub fn init(cx: &mut App) {
    cx.set_global(CookiesWindow::default());
    cx.bind_keys([
        KeyBinding::new("cmd-alt-k", ShowCookies, None),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        KeyBinding::new("cmd-w", CloseCookies, Some(CONTEXT)),
        KeyBinding::new("cmd-r", RefreshCookies, Some(CONTEXT)),
        KeyBinding::new("cmd-f", FindCookies, Some(CONTEXT)),
        KeyBinding::new("up", Previous, Some(CONTEXT)),
        KeyBinding::new("down", Next, Some(CONTEXT)),
        KeyBinding::new("left", Collapse, Some(CONTEXT)),
        KeyBinding::new("right", Expand, Some(CONTEXT)),
        KeyBinding::new("enter", Activate, Some(CONTEXT)),
        // As in Finder, deleting takes the chord: a stray Backspace must not sign anyone out.
        // The filter field binds the same chord itself, one level deeper, while it has focus.
        KeyBinding::new("cmd-backspace", DeleteSelection, Some(CONTEXT)),
    ]);
    cx.on_action(|_: &ShowCookies, cx| open(cx));
}

/// Open the Cookies window, or bring its existing window forward and refresh it.
pub fn open(cx: &mut App) {
    // A menu action runs inside the active window's update. When that window is the Cookies
    // window, it cannot be updated again until the action returns, and would be mistaken for
    // closed. So do the work right after.
    cx.defer(show);
}

fn show(cx: &mut App) {
    if let Some(handle) = cx.global::<CookiesWindow>().handle {
        if handle
            .update(cx, |manager, window, cx| {
                window.activate_window();
                window.focus(&manager.focus);
                manager.refresh(window, cx);
            })
            .is_ok()
        {
            cx.activate(true);
            return;
        }
        cx.global_mut::<CookiesWindow>().handle = None;
    }

    let bounds = Bounds::centered(None, size(px(900.), px(620.)), cx);
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(720.), px(460.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Cookies".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| CookieManager::new(window, cx)),
    ) {
        Ok(handle) => {
            cx.global_mut::<CookiesWindow>().handle = Some(handle);
            cx.activate(true);
        }
        Err(error) => eprintln!("Could not open the Cookies window: {error}"),
    }
}

/// Runs `update` on the open Cookies window.
#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native integration harness")]
pub fn update_for_test<R>(
    cx: &mut App,
    update: impl FnOnce(&mut CookieManager, &mut Window, &mut Context<CookieManager>) -> R,
) -> R {
    cx.global::<CookiesWindow>()
        .handle
        .expect("Cookies is open")
        .update(cx, update)
        .expect("live Cookies window")
}

/// Runs `update` on the open Cookies window without leasing its view, so that events
/// dispatched there can reach the view's own handlers.
#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native integration harness")]
pub fn window_for_test<R>(cx: &mut App, update: impl FnOnce(&mut Window, &mut App) -> R) -> R {
    let handle: gpui::AnyWindowHandle = cx
        .global::<CookiesWindow>()
        .handle
        .expect("Cookies is open")
        .into();
    handle
        .update(cx, |_, window, cx| update(window, cx))
        .expect("live Cookies window")
}

pub struct CookieManager {
    focus: FocusHandle,
    search: Entity<AddressInput>,
    graph: CookieGraph,
    /// The cookies, the filter, the selection and the delete question: see `cookies.rs`.
    state: WindowState,
    answer: Option<Task<()>>,
    _subscriptions: [Subscription; 3],
}

impl CookieManager {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let search = cx.new(|cx| AddressInput::new(cx).with_placeholder("Filter by domain"));
        let subscriptions = [
            cx.observe_global::<ThemeSettings>(|_, cx| cx.notify()),
            cx.subscribe_in(
                &search,
                window,
                |this, search, event: &AddressEvent, window, cx| match event {
                    AddressEvent::Changed => {
                        let query = search.read(cx).text().to_string();
                        this.state.set_query(&query);
                        this.changed(cx);
                    }
                    AddressEvent::Up => this.step(-1, cx),
                    AddressEvent::Down => this.step(1, cx),
                    AddressEvent::Escape => this.back_out(window, cx),
                    AddressEvent::ResetZoom => {}
                },
            ),
            // Cookies change while the user browses: show the current ones on return.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.refresh(window, cx);
                }
            }),
        ];
        let mut manager = Self {
            focus,
            search,
            graph: CookieGraph::default(),
            state: WindowState::default(),
            answer: None,
            _subscriptions: subscriptions,
        };
        manager.changed(cx);
        manager.refresh(window, cx);
        manager
    }

    /// The graph follows the state's rows and selection; then draw again.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.graph.sync(self.state.rows(), self.state.selection());
        cx.notify();
    }

    /// Asks WebKit for the cookies again. The current list stays until the answer comes.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.may_fetch() {
            return;
        }
        let (reply, answer) = mpsc::channel();
        let sent = cookie_store::fetch(reply);
        self.wait(sent, answer, cookies::answer_timeout(0), window, cx);
    }

    /// WebKit answers in a native callback, which only fills `answer`. Poll it from GPUI's
    /// side until it has the list, for at most `timeout`; a newer request replaces this wait.
    fn wait(
        &mut self,
        sent: Result<(), String>,
        answer: mpsc::Receiver<Vec<Cookie>>,
        timeout: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = sent {
            self.state.send_failed(error);
            self.changed(cx);
            return;
        }
        self.answer = Some(cx.spawn_in(window, async move |this, cx| {
            let mut waited = Duration::ZERO;
            let step = Duration::from_millis(40);
            let cookies = loop {
                match answer.try_recv() {
                    Ok(cookies) => break Some(cookies),
                    Err(TryRecvError::Disconnected) => break None,
                    Err(TryRecvError::Empty) if waited >= timeout => break None,
                    Err(TryRecvError::Empty) => {}
                }
                cx.background_executor().timer(step).await;
                waited += step;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let follow = this.state.answered(cookies.as_deref());
                this.changed(cx);
                if follow == Follow::Fetch {
                    this.refresh(window, cx);
                }
            });
        }));
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native integration harness")]
    pub fn rows(&self) -> &[cookies::Row] {
        self.state.rows()
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native integration harness")]
    pub fn graph_probe(&self) -> crate::egui_host::Probe {
        self.graph.probe()
    }

    /// Whether the rows show what WebKit last reported, with no delete on its way.
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native integration harness")]
    pub fn loaded(&self) -> bool {
        self.state.loaded() && self.state.may_fetch()
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native integration harness")]
    pub fn select(&mut self, node: Node, cx: &mut Context<Self>) {
        self.state.select(node);
        self.changed(cx);
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native integration harness")]
    pub fn toggle(&mut self, node: &Node, cx: &mut Context<Self>) {
        self.state.toggle(node);
        self.changed(cx);
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        self.state.step(by);
        self.changed(cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        self.state.collapse();
        self.changed(cx);
    }

    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        self.state.expand();
        self.changed(cx);
    }

    fn activate(&mut self, _: &Activate, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.state.activate();
        self.send_delete(keys, window, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.back_out(window, cx);
    }

    /// Escape backs out one step at a time: the delete question, the filter, the window.
    fn back_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.state.back_out() {
            BackOut::Cancelled => self.changed(cx),
            BackOut::ClearedFilter => {
                self.search
                    .update(cx, |search, cx| search.set_text(String::new(), cx));
                self.changed(cx);
                window.focus(&self.focus);
            }
            BackOut::Close => window.remove_window(),
        }
    }

    fn close(&mut self, _: &CloseCookies, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.remove_window();
    }

    /// Deletes the selected cookie at once; a branch first asks, naming how many cookies go.
    pub fn delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.state.request_delete();
        self.send_delete(keys, window, cx);
    }

    pub fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let keys = self.state.confirm();
        self.send_delete(keys, window, cx);
    }

    /// Sends the cookies the state chose to WebKit; the answer is the next list.
    fn send_delete(
        &mut self,
        keys: Option<HashSet<Key>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.changed(cx);
        let Some(keys) = keys else {
            return;
        };
        let timeout = cookies::answer_timeout(keys.len());
        let (reply, answer) = mpsc::channel();
        let sent = cookie_store::delete(keys, reply);
        self.wait(sent, answer, timeout, window, cx);
    }

    fn click(&mut self, node: &Node, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        self.state.click(node);
        self.changed(cx);
    }

    fn details(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let node = self.state.selection();
        let totals = self.state.totals();
        let panel = div().flex().flex_col().gap_3();
        let panel = match node {
            Node::Cookie(key) => {
                let Some(cookie) = self.state.selected_cookie() else {
                    return panel;
                };
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs() as i64);
                let length = cookie.value.chars().count();
                let value = if self.state.revealed() {
                    cookies::display(&cookie.value, 600)
                } else {
                    format!("•••••••• ({length} characters)")
                };
                panel
                    .child(heading("COOKIE", &cookies::display(&key.name, 200), p))
                    .child(field(
                        "Host",
                        &if cookie.includes_subdomains() {
                            format!("{} and its subdomains", cookie.host())
                        } else {
                            format!("{} only", cookie.host())
                        },
                        p,
                    ))
                    .child(field("Path", &cookies::display(&cookie.path, 200), p))
                    .child(field("Expires", &cookies::expiry(cookie.expires, now), p))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(flag("Secure", cookie.secure, p))
                            .child(flag("HttpOnly", cookie.http_only, p))
                            .child(flag(
                                &format!("SameSite: {}", cookie.same_site.name()),
                                cookie.same_site != cookies::SameSite::None,
                                p,
                            )),
                    )
                    .child(
                        field("Value", &value, p).child(
                            button(
                                "reveal-value",
                                if self.state.revealed() {
                                    "Hide"
                                } else {
                                    "Reveal"
                                },
                                p,
                            )
                            .mt_1()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.toggle_revealed();
                                cx.notify();
                            })),
                        ),
                    )
            }
            branch => {
                let (kind, name) = match branch {
                    Node::Site(name) => ("SITE", name.as_str()),
                    Node::Host(name) => ("HOST", name.as_str()),
                    _ => ("EVERYTHING", "All cookies"),
                };
                panel
                    .child(heading(kind, name, p))
                    .child(field(
                        "Cookies",
                        &match cookies::span(totals, branch) {
                            Some(span) => {
                                format!("{} · {span}", cookies::cookie_count(totals.cookies))
                            }
                            None => cookies::cookie_count(totals.cookies),
                        },
                        p,
                    ))
                    .child(field(
                        "Lifetime",
                        &format!(
                            "{} kept across restarts · {} session",
                            totals.cookies - totals.session,
                            totals.session
                        ),
                        p,
                    ))
                    .child(field(
                        "Flags",
                        &format!("{} Secure · {} HttpOnly", totals.secure, totals.http_only),
                        p,
                    ))
            }
        };
        if totals.cookies == 0 {
            return panel;
        }
        match self.state.confirming() {
            Some(pending) => panel.child(
                div()
                    .mt_2()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(p.primary))
                    .bg(rgb(p.elevated))
                    .text_size(px(13.))
                    .child(SharedString::from(pending.prompt()))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button("confirm-delete", "Delete", p)
                                    .border_2()
                                    .border_color(rgb(p.primary))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.confirm_delete(window, cx);
                                    })),
                            )
                            .child(button("cancel-delete", "Cancel", p).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.state.cancel();
                                    cx.notify();
                                },
                            ))),
                    ),
            ),
            None => panel.child(
                button(
                    "delete-selection",
                    SharedString::from(match node {
                        Node::Cookie(_) => "Delete Cookie".to_string(),
                        _ => format!("Delete {}…", cookies::cookie_count(totals.cookies)),
                    }),
                    p,
                )
                .mt_2()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.delete_selection(window, cx);
                })),
            ),
        }
    }
}

fn heading(kind: &'static str, name: &str, p: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(p.muted))
                .child(kind),
        )
        .child(
            div()
                .text_size(px(16.))
                .line_height(px(21.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(SharedString::from(name.to_string())),
        )
}

fn field(label: &'static str, value: &str, p: Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(p.muted))
                .child(label),
        )
        .child(
            div()
                .text_size(px(13.))
                .line_height(px(19.))
                .child(SharedString::from(value.to_string())),
        )
}

fn flag(label: &str, on: bool, p: Palette) -> Div {
    div()
        .px_2()
        .rounded_full()
        .border_1()
        .border_color(rgb(if on { p.secondary } else { p.border }))
        .text_size(px(11.))
        .line_height(px(18.))
        .text_color(rgb(if on { p.text } else { p.muted }))
        .child(SharedString::from(if on {
            format!("✓ {label}")
        } else {
            label.to_string()
        }))
}

fn button(id: &'static str, label: impl Into<SharedString>, p: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(30.))
        .px_3()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_lg()
        .bg(rgb(p.elevated))
        .border_1()
        .border_color(rgb(p.border))
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(move |style| style.border_color(rgb(p.primary)))
        .child(label.into())
}

impl Render for CookieManager {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        // The graph first: a click in it changes what the rest of this frame shows.
        let active = window.is_window_active();
        let graph = self.graph.show(p, theme::motion_allowed(cx), active);
        if let Some(node) = &graph.clicked {
            self.click(node, window, cx);
        }
        // The layout animates only while its window is active.
        if graph.animating && active {
            window.request_animation_frame();
        }
        let total = self.state.total();
        let query = self.state.query().trim().to_string();
        let summary = if !self.state.loaded() {
            "Loading…".to_string()
        } else if query.is_empty() {
            format!(
                "{} from {}",
                cookies::cookie_count(total),
                match self.state.tree().sites.len() {
                    1 => "1 site".to_string(),
                    sites => format!("{sites} sites"),
                }
            )
        } else {
            format!(
                "{} of {} match",
                self.state.shown_count(),
                cookies::cookie_count(total)
            )
        };
        let empty = if !self.state.loaded() {
            None
        } else if total == 0 {
            Some("No cookies yet. Sites you visit and sign in to appear here.".to_string())
        } else if self.state.shown_count() == 0 {
            Some(format!(
                "No site or host matches “{}”.",
                cookies::display(&query, 40)
            ))
        } else {
            None
        };
        let search_focus = self.search.read(cx).focus_handle(cx);
        div()
            .id("cookies")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::activate))
            .on_action(cx.listener(|this, _: &RefreshCookies, window, cx| {
                this.refresh(window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelection, window, cx| {
                this.delete_selection(window, cx);
            }))
            .on_action(cx.listener(move |_, _: &FindCookies, window, _| {
                window.focus(&search_focus);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.background))
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(rgb(p.border))
                    .bg(rgb(p.surface))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Cookies"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(p.muted))
                                    .child(SharedString::from(summary)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(div().w(px(260.)).child(self.search.clone()))
                    .child(button("refresh-cookies", "Refresh", p).h(px(34.)).on_click(
                        cx.listener(|this, _, window, cx| {
                            this.refresh(window, cx);
                        }),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.graph.element(graph.frame, cx.entity_id()).size_full())
                            .child(
                                button("fit-graph", "Fit", p)
                                    .absolute()
                                    .top_3()
                                    .right_3()
                                    .h(px(26.))
                                    .px_2()
                                    .text_size(px(12.))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.graph.fit();
                                        cx.notify();
                                    })),
                            )
                            .when_some(empty, |panel, message| {
                                panel.child(
                                    div()
                                        .absolute()
                                        .left_4()
                                        .bottom_3()
                                        .text_size(px(13.))
                                        .text_color(rgb(p.muted))
                                        .child(SharedString::from(message)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("cookie-details")
                            .w(px(300.))
                            .flex_shrink_0()
                            .h_full()
                            .p_4()
                            .overflow_y_scroll()
                            .border_l_1()
                            .border_color(rgb(p.border))
                            .bg(rgb(p.surface))
                            .child(self.details(p, cx)),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(p.border))
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child(match self.state.error().or(self.state.notice()) {
                        Some(message) => SharedString::from(message.to_string()),
                        None => "Click a node to open it · drag to move it or the view · scroll \
                                 to zoom. Session cookies end when Felinaut quits."
                            .into(),
                    }),
            )
    }
}
