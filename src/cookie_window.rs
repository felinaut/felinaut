//! The Cookies window: WebKit's stored cookies as a site → host → cookie tree, with details
//! and delete at every level. One window, independently closable. The host calls `init`.
//!
//! The tree and every decision about it live in `cookies.rs`; this file draws its rows and
//! asks `cookie_store.rs` for the real cookies. Cookie values stay hidden until revealed and
//! are never logged or offered to agent control.

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{
    AnyElement, App, Bounds, Context, Div, Entity, FocusHandle, Focusable, Global, KeyBinding,
    ScrollStrategy, SharedString, Stateful, Subscription, Task, TitlebarOptions,
    UniformListScrollHandle, Window, WindowBounds, WindowHandle, WindowOptions, actions, div,
    prelude::*, px, rgb, size, uniform_list,
};

use crate::{
    cookie_store,
    cookies::{self, Cookie, Key, Node, Row, Tree},
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
const ROW_HEIGHT: f32 = 32.;
/// The width of one tree level: a connector column, and the box around a node's dot.
const COLUMN: f32 = 22.;
/// How long WebKit may take to answer before the window says so.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

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

pub struct CookieManager {
    focus: FocusHandle,
    search: Entity<AddressInput>,
    scroll: UniformListScrollHandle,
    /// Everything WebKit reported last.
    tree: Tree,
    /// `tree` through the domain filter: what the rows, the selection and delete work on.
    shown: Tree,
    rows: Rc<[Row]>,
    expanded: HashSet<Node>,
    selection: Node,
    /// A branch whose deletion waits for the user's one confirmation.
    confirming: Option<Node>,
    revealed: bool,
    loaded: bool,
    /// A delete is on its way; its answer is the next refresh.
    deleting: bool,
    error: Option<SharedString>,
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
                |this, _, event: &AddressEvent, window, cx| match event {
                    AddressEvent::Changed => this.rebuild(cx),
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
            scroll: UniformListScrollHandle::new(),
            tree: Tree::default(),
            shown: Tree::default(),
            rows: Rc::from([]),
            expanded: HashSet::from([Node::All]),
            selection: Node::All,
            confirming: None,
            revealed: false,
            loaded: false,
            deleting: false,
            error: None,
            answer: None,
            _subscriptions: subscriptions,
        };
        manager.rebuild(cx);
        manager.refresh(window, cx);
        manager
    }

    /// Asks WebKit for the cookies again. The current list stays until the answer comes.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deleting {
            return;
        }
        let (reply, answer) = mpsc::channel();
        let sent = cookie_store::fetch(reply);
        self.wait(sent, answer, window, cx);
    }

    /// WebKit answers in a native callback, which only fills `answer`. Poll it from GPUI's
    /// side until it has the list, for a bounded time; a newer request replaces this wait.
    fn wait(
        &mut self,
        sent: Result<(), String>,
        answer: mpsc::Receiver<Vec<Cookie>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = sent {
            self.deleting = false;
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        self.answer = Some(cx.spawn_in(window, async move |this, cx| {
            let mut waited = Duration::ZERO;
            let step = Duration::from_millis(40);
            let cookies = loop {
                match answer.try_recv() {
                    Ok(cookies) => break Some(cookies),
                    Err(TryRecvError::Disconnected) => break None,
                    Err(TryRecvError::Empty) if waited >= ANSWER_TIMEOUT => break None,
                    Err(TryRecvError::Empty) => {}
                }
                cx.background_executor().timer(step).await;
                waited += step;
            };
            let _ = this.update(cx, |this, cx| {
                this.deleting = false;
                match cookies {
                    Some(cookies) => this.load(&cookies, cx),
                    None => {
                        this.error = Some("WebKit did not answer. Try Refresh.".into());
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn load(&mut self, cookies: &[Cookie], cx: &mut Context<Self>) {
        self.tree = Tree::build(cookies);
        self.loaded = true;
        self.error = None;
        self.rebuild(cx);
    }

    /// Recomputes what is shown after the cookies, the filter or the expansion changed.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.shown = self.tree.filtered(self.search.read(cx).text());
        let selection = self.shown.nearest(&self.selection);
        if selection != self.selection {
            self.select(selection, cx);
        }
        if self
            .confirming
            .as_ref()
            .is_some_and(|node| !self.shown.contains(node))
        {
            self.confirming = None;
        }
        self.rows = self.shown.rows(&self.expanded).into();
        cx.notify();
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native integration harness")]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether the rows show what WebKit last reported, with no delete on its way.
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native integration harness")]
    pub fn loaded(&self) -> bool {
        self.loaded && !self.deleting
    }

    pub fn select(&mut self, node: Node, cx: &mut Context<Self>) {
        if self.selection != node {
            self.selection = node;
            self.revealed = false;
            self.confirming = None;
        }
        cx.notify();
    }

    pub fn toggle(&mut self, node: &Node, cx: &mut Context<Self>) {
        if matches!(node, Node::Cookie(_)) {
            return;
        }
        if !self.expanded.remove(node) {
            self.expanded.insert(node.clone());
        }
        self.rebuild(cx);
    }

    fn selected_row(&self) -> Option<usize> {
        self.rows.iter().position(|row| row.node == self.selection)
    }

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(index) {
            self.select(row.node.clone(), cx);
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let current = self.selected_row().unwrap_or(0);
        let last = self.rows.len().saturating_sub(1);
        self.select_row(current.saturating_add_signed(by).min(last), cx);
    }

    fn previous(&mut self, _: &Previous, _: &mut Window, cx: &mut Context<Self>) {
        self.step(-1, cx);
    }

    fn next(&mut self, _: &Next, _: &mut Window, cx: &mut Context<Self>) {
        self.step(1, cx);
    }

    /// ← closes an open branch, or else moves to the parent.
    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_row() else {
            return;
        };
        let row = self.rows[index].clone();
        if row.expanded == Some(true) {
            self.toggle(&row.node, cx);
        } else if let Some(parent) = self.rows[..index]
            .iter()
            .rposition(|above| above.depth < row.depth)
        {
            self.select_row(parent, cx);
        }
    }

    /// → opens a closed branch, or else moves to its first child.
    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected_row() else {
            return;
        };
        match self.rows[index].expanded {
            Some(false) => self.toggle(&self.selection.clone(), cx),
            Some(true) => self.select_row(index + 1, cx),
            None => {}
        }
    }

    /// Return confirms a pending delete; otherwise it opens or closes the selected branch.
    fn activate(&mut self, _: &Activate, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming.is_some() {
            self.confirm_delete(window, cx);
        } else {
            self.toggle(&self.selection.clone(), cx);
        }
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.back_out(window, cx);
    }

    /// Escape backs out one step at a time: the delete question, the filter, the window.
    fn back_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming.take().is_some() {
            cx.notify();
        } else if !self.search.read(cx).text().is_empty() {
            self.search
                .update(cx, |search, cx| search.set_text(String::new(), cx));
            self.rebuild(cx);
            window.focus(&self.focus);
        } else {
            window.remove_window();
        }
    }

    fn close(&mut self, _: &CloseCookies, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.remove_window();
    }

    /// Deletes the selected cookie at once; a branch first asks, naming how many cookies go.
    pub fn delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.deleting || self.shown.subtree(&self.selection).is_empty() {
            return;
        }
        if matches!(self.selection, Node::Cookie(_)) {
            self.delete(&self.selection.clone(), window, cx);
        } else {
            self.confirming = Some(self.selection.clone());
            cx.notify();
        }
    }

    pub fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(node) = self.confirming.take() {
            self.delete(&node, window, cx);
        }
    }

    /// Deletes exactly the cookies shown under `node`: with a filter on, a site's hidden
    /// hosts keep theirs.
    fn delete(&mut self, node: &Node, window: &mut Window, cx: &mut Context<Self>) {
        let keys: HashSet<Key> = self
            .shown
            .subtree(node)
            .into_iter()
            .map(Cookie::key)
            .collect();
        if keys.is_empty() || self.deleting {
            return;
        }
        // Show the result at once; WebKit's answer then replaces it with what is stored.
        self.tree.remove_keys(&keys);
        self.deleting = true;
        self.rebuild(cx);
        let (reply, answer) = mpsc::channel();
        let sent = cookie_store::delete(keys, reply);
        self.wait(sent, answer, window, cx);
    }

    fn row(&self, index: usize, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[index];
        // The click acts on the node drawn here, even if a refresh has moved the rows since.
        let node = row.node.clone();
        let selected = row.node == self.selection;
        let color = node_color(row.depth, p);
        let mut line = div()
            .id(("cookie-row", index))
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .cursor_pointer();
        for rail in &row.rails {
            line = line.child(connector(
                if *rail {
                    Connector::Rail
                } else {
                    Connector::None
                },
                p,
            ));
        }
        if row.depth > 0 {
            line = line.child(connector(
                if row.last {
                    Connector::Elbow
                } else {
                    Connector::Tee
                },
                p,
            ));
        }
        let dot = if row.expanded.is_some() { 12. } else { 8. };
        let pill = div()
            .h(px(26.))
            .min_w_0()
            .pl(px(4.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .border_1()
            .border_color(rgb(if selected { p.primary } else { p.border }))
            .bg(rgb(if selected { p.elevated } else { p.surface }))
            .hover(move |style| style.bg(rgb(p.elevated)))
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().size(px(dot)).rounded_full().bg(rgb(color))),
            )
            .when_some(row.expanded, |pill, open| {
                pill.child(
                    div()
                        .flex_shrink_0()
                        .w(px(9.))
                        .text_size(px(9.))
                        .text_color(rgb(p.muted))
                        .child(if open { "▼" } else { "▶" }),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(13.))
                    .when(row.depth < 2, |label| {
                        label.font_weight(gpui::FontWeight::MEDIUM)
                    })
                    .child(SharedString::from(row.label.clone())),
            )
            .when_some(row.note.clone(), |pill, note| {
                pill.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(rgb(p.muted))
                        .child(SharedString::from(note)),
                )
            })
            .when(row.expanded.is_some(), |pill| {
                pill.child(
                    div()
                        .flex_shrink_0()
                        .px(px(6.))
                        .rounded_full()
                        // Outlined, not filled: the level colors are bright in some themes
                        // and dark in others, and the count must stay readable on all.
                        .border_1()
                        .border_color(rgb(color))
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(SharedString::from(row.count.to_string())),
                )
            });
        line.child(
            div()
                .relative()
                .h_full()
                .min_w_0()
                .flex()
                .items_center()
                // The line down to the children starts behind the node's dot.
                .when(row.expanded == Some(true) && row.count > 0, |node| {
                    node.child(
                        div()
                            .absolute()
                            .left(px(COLUMN / 2.))
                            .top(px(ROW_HEIGHT / 2.))
                            .bottom_0()
                            .w(px(1.))
                            .bg(rgb(p.border)),
                    )
                })
                .child(pill),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            window.focus(&this.focus);
            // A click opens a closed branch. An open one closes on a second click, so that
            // selecting it, to read its totals or delete it, leaves it open.
            let reselected = this.selection == node;
            this.select(node.clone(), cx);
            if reselected || !this.expanded.contains(&node) {
                this.toggle(&node, cx);
            }
        }))
        .into_any_element()
    }

    fn details(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let node = &self.selection;
        let totals = self.shown.totals(node);
        let panel = div().flex().flex_col().gap_3();
        let panel = match node {
            Node::Cookie(key) => {
                let Some(cookie) = self.shown.subtree(node).first().copied() else {
                    return panel;
                };
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_secs() as i64);
                let length = cookie.value.chars().count();
                let value = if self.revealed {
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
                                if self.revealed { "Hide" } else { "Reveal" },
                                p,
                            )
                            .mt_1()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.revealed = !this.revealed;
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
        match &self.confirming {
            Some(node) => panel.child(
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
                    .child(SharedString::from(cookies::delete_prompt(
                        node,
                        totals.cookies,
                    )))
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
                                    this.confirming = None;
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

#[derive(Clone, Copy)]
enum Connector {
    None,
    /// An ancestor's line passing this row on its way to a later sibling.
    Rail,
    /// To this node, with more siblings below.
    Tee,
    /// To this node, the last of its siblings.
    Elbow,
}

/// One tree level of connecting line, `COLUMN` wide and exactly one row tall, so that the
/// lines of consecutive rows meet.
fn connector(kind: Connector, p: Palette) -> Div {
    let column = div()
        .relative()
        .w(px(COLUMN))
        .h(px(ROW_HEIGHT))
        .flex_shrink_0();
    let middle = px(COLUMN / 2.);
    let line = || div().absolute().left(middle).bg(rgb(p.border));
    match kind {
        Connector::None => column,
        Connector::Rail => column.child(line().top_0().bottom_0().w(px(1.))),
        Connector::Tee => column
            .child(line().top_0().bottom_0().w(px(1.)))
            .child(line().top(px(ROW_HEIGHT / 2. - 1.)).right_0().h(px(1.))),
        Connector::Elbow => column.child(
            div()
                .absolute()
                .left(middle)
                .top_0()
                .right_0()
                .h(px(ROW_HEIGHT / 2.))
                .border_l_1()
                .border_b_1()
                .border_color(rgb(p.border))
                .rounded_bl(px(8.)),
        ),
    }
}

/// Each level of the graph has its own theme color.
fn node_color(depth: usize, p: Palette) -> u32 {
    match depth {
        0 => p.accent,
        1 => p.primary,
        2 => p.secondary,
        _ => p.highlight,
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let total = self.tree.count();
        let query = self.search.read(cx).text().trim().to_string();
        let summary = if !self.loaded {
            "Loading…".to_string()
        } else if query.is_empty() {
            format!(
                "{} from {}",
                cookies::cookie_count(total),
                match self.tree.sites.len() {
                    1 => "1 site".to_string(),
                    sites => format!("{sites} sites"),
                }
            )
        } else {
            format!(
                "{} of {} match",
                self.shown.count(),
                cookies::cookie_count(total)
            )
        };
        let empty = if !self.loaded {
            None
        } else if total == 0 {
            Some("No cookies yet. Sites you visit and sign in to appear here.".to_string())
        } else if self.shown.count() == 0 {
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
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .p_3()
                            .child(
                                uniform_list(
                                    "cookie-tree",
                                    self.rows.len(),
                                    cx.processor(move |this, range: Range<usize>, _, cx| {
                                        range
                                            .map(|index| this.row(index, p, cx))
                                            .collect::<Vec<_>>()
                                    }),
                                )
                                .track_scroll(self.scroll.clone())
                                .flex_1()
                                .min_h_0(),
                            )
                            .when_some(empty, |tree, message| {
                                tree.child(
                                    div()
                                        .flex_shrink_0()
                                        .p_3()
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
                    .child(match &self.error {
                        Some(error) => error.clone(),
                        None => "Cookies with an expiry keep you signed in across restarts. \
                                 Session cookies end when Felinaut quits."
                            .into(),
                    }),
            )
    }
}
