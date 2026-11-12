//! A single, independently closable About window. The host registers `open`.

use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, Context, FocusHandle, Global, KeyBinding, Subscription, Task, TitlebarOptions,
    Window, WindowBounds, WindowHandle, WindowOptions, actions, div, prelude::*, px, rgb, size,
};

use crate::{
    DISPLAY_VERSION, art, brand,
    theme::{self, Palette, ThemeSettings},
};

actions!(about, [CloseAbout]);

#[derive(Default)]
struct AboutWindow {
    // GPUI window handles do not keep their window (or root entity) alive.
    handle: Option<WindowHandle<About>>,
}
impl Global for AboutWindow {}

/// Open About Felifox, or bring its existing window forward.
pub fn open(cx: &mut App) {
    if !cx.has_global::<AboutWindow>() {
        cx.set_global(AboutWindow::default());
        cx.bind_keys([
            KeyBinding::new("escape", CloseAbout, Some("FelifoxAbout")),
            KeyBinding::new("cmd-w", CloseAbout, Some("FelifoxAbout")),
        ]);
    }

    if let Some(handle) = cx.global::<AboutWindow>().handle {
        if handle
            .update(cx, |about, window, _| {
                window.activate_window();
                window.focus(&about.focus);
            })
            .is_ok()
        {
            cx.activate(true);
            return;
        }
        cx.global_mut::<AboutWindow>().handle = None;
    }

    let bounds = Bounds::centered(None, size(px(600.), px(700.)), cx);
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(600.), px(700.))),
            titlebar: Some(TitlebarOptions {
                title: Some("About Felifox".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| About::new(window, cx)),
    ) {
        Ok(handle) => {
            cx.global_mut::<AboutWindow>().handle = Some(handle);
            cx.activate(true);
        }
        Err(error) => eprintln!("Could not open About Felifox: {error}"),
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "Native integration test verifies the real animation timer"
)]
pub fn animation_phase_for_test(cx: &mut App) -> f32 {
    cx.global::<AboutWindow>()
        .handle
        .expect("About is open")
        .update(cx, |about, _, _| about.phase)
        .expect("live About window")
}

struct About {
    focus: FocusHandle,
    phase: f32,
    _theme_observer: Subscription,
    _animation: Task<()>,
}

impl About {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let theme_observer = cx.observe_global::<ThemeSettings>(|_, cx| cx.notify());
        let animation = cx.spawn_in(window, async move |about, cx| {
            let mut previous = Instant::now();
            let mut was_running = false;
            let mut delay = Duration::from_millis(200);
            loop {
                cx.background_executor().timer(delay).await;
                let now = Instant::now();
                let elapsed = now.duration_since(previous).as_secs_f32();
                previous = now;
                let result = about.update_in(cx, |about, window, cx| {
                    let running = window.is_window_active() && theme::motion_allowed(cx);
                    // Never count a background interval when resuming the window.
                    if running && was_running {
                        about.phase = (about.phase + elapsed) % 3600.;
                        cx.notify();
                    }
                    running
                });
                let Ok(running) = result else { break };
                was_running = running;
                delay = Duration::from_millis(if running { 33 } else { 200 });
            }
        });
        Self {
            focus,
            phase: 0.,
            _theme_observer: theme_observer,
            _animation: animation,
        }
    }

    fn close(&mut self, _: &CloseAbout, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.remove_window();
    }
}

fn chip(label: &'static str, color: u32, p: Palette) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .rounded_full()
        .bg(rgb(p.surface))
        .border_1()
        .border_color(rgb(p.border))
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(div().size(px(5.)).rounded_full().bg(rgb(color)))
        .child(label)
}

impl Render for About {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let phase = if theme::motion_allowed(cx) {
            self.phase
        } else {
            0.
        };
        div()
            .id("about-felifox")
            .key_context("FelifoxAbout")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(p.background))
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .child(art::backdrop(p, phase).absolute().inset_0())
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .px_8()
                    .py_2()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(10.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(p.muted))
                            .child(brand::symbol().size(px(18.)))
                            .child("A LITTLE CURIOSITY GOES A LONG WAY"),
                    )
                    .child(
                        brand::mascot()
                            .w(px(300.))
                            .h(px(225.))
                            .flex_shrink_0()
                            .relative()
                            .top(px((phase * 1.2).sin() * 6.)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .font_family("Georgia")
                                    .italic()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_size(px(48.))
                                    .line_height(px(52.))
                                    .child("Felifox"),
                            )
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .text_color(rgb(p.muted))
                                    .child("Curiosity, with a little mischief."),
                            ),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(rgb(p.elevated))
                            .border_1()
                            .border_color(rgb(p.border))
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(version_label()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(chip("Rust", p.accent, p))
                            .child(chip("GPUI", p.primary, p))
                            .child(chip("WebKit", p.highlight, p)),
                    )
                    .child(
                        div()
                            .w(px(420.))
                            .px_3()
                            .py_2()
                            .rounded_xl()
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.border))
                            .text_center()
                            .text_size(px(12.))
                            .line_height(px(19.))
                            .child("A small native browser, still finding its paws.")
                            .child(
                                div().text_color(rgb(p.muted)).child(
                                    "An early beta: expect missing features and rough edges.",
                                ),
                            ),
                    )
                    .child(
                        div()
                            .id("close-about")
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(112.))
                            .h(px(34.))
                            .rounded_lg()
                            .bg(rgb(p.elevated))
                            .border_1()
                            .border_color(rgb(p.border))
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .hover(move |style| style.bg(rgb(p.input)).border_color(rgb(p.primary)))
                            .on_click(|_, window, cx| {
                                cx.stop_propagation();
                                window.remove_window();
                            })
                            .child("Close"),
                    ),
            )
    }
}

fn version_label() -> String {
    format!(
        "{} · {} ({})",
        DISPLAY_VERSION,
        env!("CARGO_PKG_VERSION"),
        env!("FELIFOX_GIT_COMMIT")
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_label_is_the_public_beta_version() {
        assert_eq!(crate::DISPLAY_VERSION, "0.1 beta");
    }

    #[test]
    fn about_identifies_the_package_version_and_built_commit() {
        let revision = env!("FELIFOX_GIT_COMMIT");
        assert!(
            revision == "unknown"
                || (revision.len() == 12 && revision.bytes().all(|b| b.is_ascii_hexdigit()))
        );
        assert_eq!(
            super::version_label(),
            format!("0.1 beta · {} ({revision})", env!("CARGO_PKG_VERSION"))
        );
    }
}
