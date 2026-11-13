//! The proxy popover: pick a saved SOCKS5 proxy for every tab, add one, or go direct. Like the
//! volume popover it is a native popup window, so it can float above Wry's page view.

use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight, Global,
    KeyBinding, Pixels, SharedString, Subscription, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, actions, div, point, prelude::*, px,
    rgb, size,
};

use crate::{
    brand::{self, icon},
    input::{AddressEvent, AddressInput, Submit},
    load_error::Kind,
    proxy::{self, Draft, Proxies, Status},
    theme::{self, Palette, ThemeSettings},
};

actions!(proxy_panel, [CloseProxy, NextField, PreviousField]);

const WIDTH: f32 = 372.;
/// The popover without saved proxies, and each saved proxy's row with its gap.
const BASE_HEIGHT: f32 = 432.;
const ROW_HEIGHT: f32 = 44.;
const MAX_HEIGHT: f32 = 640.;

/// Tall enough for the saved proxies plus one more, so adding one does not need a scroll.
fn height(saved: usize) -> f32 {
    (BASE_HEIGHT + ROW_HEIGHT * (saved + 1) as f32).min(MAX_HEIGHT)
}

#[derive(Default)]
struct PopupWindow(Option<WindowHandle<ProxyPanel>>);
impl Global for PopupWindow {}

/// What the header says about the current state, and whether it is a warning.
pub fn summary(state: &Proxies) -> (String, bool) {
    let name = state.list.active_proxy().map(|proxy| proxy.name.as_str());
    match (state.status, name) {
        (Status::Unsupported, _) => (
            "Proxies need macOS 14 or later. Pages connect directly.".into(),
            true,
        ),
        (Status::Direct, _) | (_, None) => ("Pages connect directly.".into(), false),
        (Status::Checking, Some(name)) => (format!("Connecting to {name}…"), false),
        (Status::Ready, Some(name)) => (format!("All tabs go through {name}."), false),
        (Status::Problem(message), Some(_)) => (message.into(), true),
    }
}

/// A line for the error page when a proxy may be why a page did not load.
pub fn failure_hint(state: &Proxies, kind: Kind) -> Option<String> {
    let proxy = state.list.active_proxy().filter(|_| state.supported)?;
    matches!(
        kind,
        Kind::CannotConnect
            | Kind::TimedOut
            | Kind::ConnectionLost
            | Kind::HostNotFound
            // A proxy that rejects the password fails pages with "authentication required".
            | Kind::Other
    )
    .then(|| {
        format!(
            "Felinaut sends pages through the proxy “{}”. Check that it is running, or choose \
             Direct connection under the shield in the toolbar.",
            proxy.name
        )
    })
}

/// The toolbar button's tint: off, working, or needs attention.
pub fn button_color(state: &Proxies, p: Palette) -> u32 {
    match state.status {
        Status::Direct => p.muted,
        Status::Checking | Status::Ready => p.primary,
        Status::Problem(_) | Status::Unsupported => p.accent,
    }
}

/// Toggle the popover. `anchor_x` is the window-relative x of the toolbar button, if known.
pub fn toggle(anchor_x: Option<Pixels>, parent: &mut Window, cx: &mut App) {
    if !cx.has_global::<PopupWindow>() {
        cx.set_global(PopupWindow::default());
        let context = Some("FelinautProxy");
        cx.bind_keys([
            KeyBinding::new("escape", CloseProxy, context),
            KeyBinding::new("tab", NextField, context),
            KeyBinding::new("shift-tab", PreviousField, context),
        ]);
    }
    if let Some(handle) = cx.global_mut::<PopupWindow>().0.take()
        && handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
    {
        return;
    }

    let parent_handle = parent.window_handle();
    let parent_bounds = parent.bounds();
    let state = cx.global::<Proxies>();
    let dimensions = size(px(WIDTH), px(height(state.list.proxies.len())));
    // The status may be stale if the proxy stopped since it was last checked.
    if state.list.active.is_some() && state.status != Status::Checking {
        proxy::recheck(cx);
    }
    let anchor = anchor_x
        .map(|x| x + px(40.) - dimensions.width)
        .unwrap_or(parent_bounds.size.width - dimensions.width - px(160.))
        .max(px(8.))
        .min((parent_bounds.size.width - dimensions.width - px(8.)).max(px(8.)));
    let mut origin = parent_bounds.origin + point(anchor, px(116.));
    let display = parent.display(cx);
    if let Some(display) = &display {
        let screen = display.bounds();
        origin.x = origin
            .x
            .max(screen.origin.x)
            .min((screen.origin.x + screen.size.width - dimensions.width).max(screen.origin.x));
        origin.y = origin
            .y
            .max(screen.origin.y)
            .min((screen.origin.y + screen.size.height - dimensions.height).max(screen.origin.y));
    }
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, dimensions))),
            titlebar: None,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            focus: true,
            display_id: display.map(|display| display.id()),
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| cx.new(|cx| ProxyPanel::new(parent_handle, window, cx)),
    ) {
        Ok(handle) => cx.global_mut::<PopupWindow>().0 = Some(handle),
        Err(error) => eprintln!("Could not open the proxy controls: {error}"),
    }
}

/// The Add Proxy form's fields, in Tab order.
struct Fields {
    name: Entity<AddressInput>,
    host: Entity<AddressInput>,
    port: Entity<AddressInput>,
    username: Entity<AddressInput>,
    password: Entity<AddressInput>,
}

impl Fields {
    fn all(&self) -> [&Entity<AddressInput>; 5] {
        [
            &self.name,
            &self.host,
            &self.port,
            &self.username,
            &self.password,
        ]
    }
}

struct ProxyPanel {
    parent: AnyWindowHandle,
    focus: FocusHandle,
    was_active: bool,
    fields: Fields,
    error: Option<&'static str>,
    _subscriptions: Vec<Subscription>,
}

impl ProxyPanel {
    fn new(parent: AnyWindowHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = |placeholder: &str, masked: bool, cx: &mut Context<Self>| {
            cx.new(|cx| AddressInput::field(placeholder, masked, cx))
        };
        let fields = Fields {
            name: field("Name, e.g. Frankfurt (optional)", false, cx),
            host: field("Host or socks5://host:port", false, cx),
            port: field("1080", false, cx),
            username: field("User name (optional)", false, cx),
            password: field("Password", true, cx),
        };
        let mut subscriptions = vec![
            cx.observe_global::<ThemeSettings>(|_, cx| cx.notify()),
            cx.observe_global::<Proxies>(|_, cx| cx.notify()),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.was_active = true;
                } else if this.was_active {
                    this.close(window, cx, false);
                }
            }),
        ];
        for input in fields.all() {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, _: &Submit, window, cx| this.submit(window, cx),
            ));
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &AddressEvent, window, cx| match event {
                    AddressEvent::Escape => this.close(window, cx, true),
                    AddressEvent::Changed if this.error.is_some() => {
                        this.error = None;
                        cx.notify();
                    }
                    _ => {}
                },
            ));
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            parent,
            focus,
            was_active: window.is_window_active(),
            fields,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>, restore_focus: bool) {
        let popup = window.window_handle().window_id();
        if cx
            .global::<PopupWindow>()
            .0
            .is_some_and(|handle| handle.window_id() == popup)
        {
            cx.global_mut::<PopupWindow>().0 = None;
        }
        window.remove_window();
        if restore_focus {
            let parent = self.parent;
            cx.defer(move |cx| {
                let _ = parent.update(cx, |_, window, _| window.activate_window());
            });
        }
    }

    fn close_action(&mut self, _: &CloseProxy, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx, true);
    }

    fn move_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let handles: Vec<FocusHandle> = self
            .fields
            .all()
            .iter()
            .map(|input| input.read(cx).focus_handle(cx))
            .collect();
        let next = match handles.iter().position(|handle| handle.is_focused(window)) {
            Some(index) if forward => (index + 1) % handles.len(),
            Some(index) => (index + handles.len() - 1) % handles.len(),
            None if forward => 0,
            None => handles.len() - 1,
        };
        window.focus(&handles[next]);
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = |input: &Entity<AddressInput>| input.read(cx).text().to_owned();
        let (name, host, port, username, password) = (
            text(&self.fields.name),
            text(&self.fields.host),
            text(&self.fields.port),
            text(&self.fields.username),
            text(&self.fields.password),
        );
        let draft = Draft {
            name: &name,
            host: &host,
            port: &port,
            username: &username,
            password: &password,
        };
        match proxy::add(&draft, cx) {
            Ok(()) => {
                self.error = None;
                for input in self.fields.all() {
                    input.update(cx, |input, cx| input.set_text(String::new(), cx));
                }
                window.focus(&self.focus);
            }
            Err(error) => self.error = Some(error.message()),
        }
        cx.notify();
    }

    fn header(&self, state: &Proxies, p: Palette) -> impl IntoElement + use<> {
        let (summary, warning) = summary(state);
        let on = state.list.active.is_some() && state.supported;
        let tint = button_color(state, p);
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .size(px(40.))
                    .flex_shrink_0()
                    .rounded_xl()
                    .bg(rgb(p.elevated))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        brand::icon(icon::SHIELD)
                            .size(px(22.))
                            .text_color(rgb(tint)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("SOCKS5 Proxy"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(if warning { p.accent } else { p.muted }))
                            .child(summary),
                    ),
            )
            .child(
                div()
                    .h(px(24.))
                    .px_2()
                    .flex_shrink_0()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .bg(rgb(if on { p.primary } else { p.elevated }))
                    .text_color(rgb(if on { p.background } else { p.muted }))
                    .child(if on { "ON" } else { "OFF" }),
            )
    }

    fn row(
        &self,
        index: Option<usize>,
        title: SharedString,
        detail: Option<SharedString>,
        active: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = index.map_or(0, |index| index + 1);
        div()
            .id(("proxy-row", id))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(40.))
            .rounded_lg()
            .cursor_pointer()
            .when(active, |row| row.bg(rgb(p.elevated)))
            .hover(move |style| style.bg(rgb(p.elevated)))
            .on_click(cx.listener(move |_, _, _, cx| {
                // Clicking the proxy in use checks it again, for example after starting it.
                if active && index.is_some() {
                    proxy::recheck(cx);
                } else {
                    proxy::connect(index, cx);
                }
            }))
            .child(
                div()
                    .size(px(16.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(p.primary))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().size(px(8.)).rounded_full().bg(rgb(if active {
                        p.primary
                    } else {
                        p.surface
                    }))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .children(detail.map(|detail| {
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(detail)
                    })),
            )
            .children(index.map(|index| {
                div()
                    .id(("proxy-remove", index))
                    .size(px(26.))
                    .flex_shrink_0()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(p.muted))
                    .hover(move |style| style.bg(rgb(p.surface)).text_color(rgb(p.text)))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        proxy::remove(index, cx);
                    }))
                    .child("×")
            }))
    }
}

fn section(p: Palette, title: &'static str) -> gpui::Div {
    div().flex().flex_col().gap_1().child(
        div()
            .px_2()
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(p.muted))
            .child(title),
    )
}

impl Render for ProxyPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let state = cx.global::<Proxies>();
        let header = self.header(state, p);
        let supported = state.supported;
        let active = state.list.active;
        let saved: Vec<(SharedString, SharedString)> = state
            .list
            .proxies
            .iter()
            .map(|proxy| {
                let detail = match &proxy.username {
                    Some(user) => format!("{user} @ {}", proxy.address()),
                    None => proxy.address(),
                };
                (proxy.name.clone().into(), detail.into())
            })
            .collect();

        let mut list = section(p, "CONNECTION").child(self.row(
            None,
            "Direct connection".into(),
            Some("No proxy".into()),
            active.is_none(),
            p,
            cx,
        ));
        for (index, (name, detail)) in saved.into_iter().enumerate() {
            list = list.child(self.row(
                Some(index),
                name,
                Some(detail),
                active == Some(index),
                p,
                cx,
            ));
        }

        let form = if supported {
            let fields = &self.fields;
            section(p, "ADD A SOCKS5 PROXY")
                .gap_2()
                .child(fields.name.clone())
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(fields.host.clone()))
                        .child(div().w(px(84.)).flex_shrink_0().child(fields.port.clone())),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(fields.username.clone()))
                        .child(div().flex_1().min_w_0().child(fields.password.clone())),
                )
                .children(self.error.map(|error| {
                    div()
                        .px_1()
                        .text_size(px(12.))
                        .text_color(rgb(p.accent))
                        .child(error)
                }))
                .child(
                    div()
                        .id("proxy-add")
                        .h(px(34.))
                        .rounded_lg()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(p.primary))
                        .text_color(rgb(p.background))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.9))
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                        .child("Save and connect"),
                )
                .into_any_element()
        } else {
            div()
                .px_2()
                .text_size(px(12.))
                .text_color(rgb(p.muted))
                .child("Felinaut can send pages through a proxy on macOS 14 or later.")
                .into_any_element()
        };

        div()
            .id("felinaut-proxy")
            .key_context("FelinautProxy")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close_action))
            .on_action(
                cx.listener(|this, _: &NextField, window, cx| this.move_focus(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousField, window, cx| {
                    this.move_focus(false, window, cx)
                }),
            )
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .rounded_xl()
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.border))
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .child(header)
            .child(list)
            .child(form)
            .child(
                div()
                    .px_2()
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child(
                        "Every tab, download and live folder uses the chosen proxy. If it stops \
                         answering, pages fail to load instead of going around it. Passwords stay \
                         in your Keychain.",
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{Proxy, ProxyList};

    fn state(status: Status, active: bool) -> Proxies {
        Proxies::for_test(
            ProxyList {
                proxies: vec![Proxy {
                    name: "Frankfurt".into(),
                    host: "de.example".into(),
                    port: 1080,
                    username: None,
                }],
                active: active.then_some(0),
            },
            status,
        )
    }

    #[test]
    fn summary_names_the_proxy_and_flags_problems() {
        assert_eq!(
            summary(&state(Status::Ready, true)),
            ("All tabs go through Frankfurt.".into(), false)
        );
        assert_eq!(
            summary(&state(Status::Checking, true)),
            ("Connecting to Frankfurt…".into(), false)
        );
        assert_eq!(
            summary(&state(Status::Problem("It broke."), true)),
            ("It broke.".into(), true)
        );
        assert_eq!(
            summary(&state(Status::Direct, false)),
            ("Pages connect directly.".into(), false)
        );
        let (text, warning) = summary(&state(Status::Unsupported, true));
        assert!(warning && text.contains("macOS 14"));
        // A status left over from a proxy that is gone never names it.
        assert_eq!(
            summary(&state(Status::Ready, false)),
            ("Pages connect directly.".into(), false)
        );
    }

    #[test]
    fn popover_grows_with_saved_proxies_up_to_a_limit() {
        assert_eq!(height(0), BASE_HEIGHT + ROW_HEIGHT);
        assert_eq!(height(2), BASE_HEIGHT + 3. * ROW_HEIGHT);
        assert_eq!(height(32), MAX_HEIGHT);
        assert!((0..40).all(|n| height(n) <= height(n + 1)));
    }

    #[test]
    fn error_page_blames_the_proxy_only_for_connection_failures() {
        let on = state(Status::Ready, true);
        for kind in [
            Kind::CannotConnect,
            Kind::TimedOut,
            Kind::ConnectionLost,
            Kind::HostNotFound,
        ] {
            assert!(
                failure_hint(&on, kind).unwrap().contains("“Frankfurt”"),
                "{kind:?}"
            );
        }
        for kind in [
            Kind::Offline,
            Kind::Secure,
            Kind::Redirects,
            Kind::BlockedPort,
        ] {
            assert_eq!(failure_hint(&on, kind), None, "{kind:?}");
        }
        assert_eq!(
            failure_hint(&state(Status::Direct, false), Kind::CannotConnect),
            None
        );
        let mut old_mac = state(Status::Unsupported, true);
        old_mac.supported = false;
        assert_eq!(failure_hint(&old_mac, Kind::CannotConnect), None);
    }

    #[test]
    fn button_tint_follows_the_status() {
        let p = theme::Theme::MilkyWay.palette();
        assert_eq!(button_color(&state(Status::Direct, false), p), p.muted);
        assert_eq!(button_color(&state(Status::Checking, true), p), p.primary);
        assert_eq!(button_color(&state(Status::Ready, true), p), p.primary);
        assert_eq!(
            button_color(&state(Status::Problem("x"), true), p),
            p.accent
        );
        assert_eq!(button_color(&state(Status::Unsupported, true), p), p.accent);
    }
}
