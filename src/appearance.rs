//! Native dropdown: a GPUI-painted overlay would sit behind Wry's child NSView.

use gpui::{
    AnyWindowHandle, App, Bounds, Context, FocusHandle, Global, KeyBinding, Subscription, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, actions,
    div, point, prelude::*, px, rgb, size,
};

use crate::theme::{self, AddressDisplay, Theme, ThemeSettings};

actions!(appearance, [CloseAppearance]);

const THEME_ROW_HEIGHT: f32 = 44.;
const ROW_GAP: f32 = 4.;

fn popup_height(theme_count: usize, reduced_motion: bool, available: f32) -> f32 {
    // Header, address choices, motion control, padding and borders, plus room for
    // the system motion explanation. Short displays scroll instead of clipping.
    let fixed = 28. // Outer padding and borders.
        + 30. // Heading.
        + 137. // Address section: margin, padding, border, label and three rows.
        + 51. // Motion section: margin, padding, border and checkbox row.
        + 2. * ROW_GAP;
    let preferred = fixed
        + theme_count as f32 * (THEME_ROW_HEIGHT + ROW_GAP)
        + if reduced_motion { 32. } else { 0. };
    preferred.min(if available.is_finite() {
        available.max(1.)
    } else {
        preferred
    })
}

#[derive(Default)]
struct PopupWindow(Option<WindowHandle<Appearance>>);
impl Global for PopupWindow {}

/// Toggle the single native appearance dropdown beneath the parent's toolbar.
pub fn open(parent: &mut Window, cx: &mut App) {
    if !cx.has_global::<PopupWindow>() {
        cx.set_global(PopupWindow::default());
        cx.bind_keys([KeyBinding::new(
            "escape",
            CloseAppearance,
            Some("FelinautAppearance"),
        )]);
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
    let display = parent.display(cx);
    let available = display.as_ref().map_or(f32::INFINITY, |display| {
        f32::from(display.bounds().size.height)
    });
    let dimensions = size(
        px(280.),
        px(popup_height(
            Theme::ALL.len(),
            theme::system_reduced_motion(),
            available,
        )),
    );
    let mut origin = parent_bounds.origin + point(parent_bounds.size.width - px(290.), px(116.));
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
        |window, cx| cx.new(|cx| Appearance::new(parent_handle, window, cx)),
    ) {
        Ok(handle) => cx.global_mut::<PopupWindow>().0 = Some(handle),
        Err(error) => eprintln!("Could not open appearance dropdown: {error}"),
    }
}

#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native integration harness")]
pub fn select_theme_for_test(theme: Theme, cx: &mut App) {
    let handle = cx
        .global::<PopupWindow>()
        .0
        .expect("appearance popup is open");
    handle
        .update(cx, |this, window, cx| this.choose_theme(theme, window, cx))
        .expect("live appearance popup");
}

#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native integration harness")]
pub fn toggle_motion_for_test(cx: &mut App) {
    let handle = cx
        .global::<PopupWindow>()
        .0
        .expect("appearance popup is open");
    handle
        .update(cx, |this, _, cx| this.toggle_motion(cx))
        .expect("live appearance popup");
}

struct Appearance {
    parent: AnyWindowHandle,
    focus: FocusHandle,
    was_active: bool,
    _theme_observer: Subscription,
    _activation_observer: Subscription,
}

impl Appearance {
    fn new(parent: AnyWindowHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let theme_observer = cx.observe_global::<ThemeSettings>(|_, cx| cx.notify());
        let activation_observer = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.was_active = true;
            } else if this.was_active {
                // Never steal focus back when dismissing because another window became active.
                this.close(window, cx, false);
            }
        });
        Self {
            parent,
            focus,
            was_active: window.is_window_active(),
            _theme_observer: theme_observer,
            _activation_observer: activation_observer,
        }
    }

    fn choose_theme(&mut self, theme: Theme, window: &mut Window, cx: &mut Context<Self>) {
        cx.update_global::<ThemeSettings, _>(|settings, _| {
            settings.theme = theme;
            settings.save();
        });
        self.close(window, cx, true);
    }

    fn toggle_motion(&mut self, cx: &mut Context<Self>) {
        cx.update_global::<ThemeSettings, _>(|settings, _| {
            settings.animate = !settings.animate;
            settings.save();
        });
    }

    fn choose_address(&mut self, display: AddressDisplay, cx: &mut Context<Self>) {
        cx.update_global::<ThemeSettings, _>(|settings, _| {
            settings.address = display;
            settings.save();
        });
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
            // Defer until the popup has been removed (and the parent isn't being updated).
            cx.defer(move |cx| {
                let _ = parent.update(cx, |_, window, _| window.activate_window());
            });
        }
    }
}

impl Render for Appearance {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let settings = cx.global::<ThemeSettings>();
        let selected = settings.theme;
        let animate = settings.animate;
        let address = settings.address;
        div()
            .id("appearance")
            .key_context("FelinautAppearance")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseAppearance, window, cx| {
                cx.stop_propagation();
                this.close(window, cx, true);
            }))
            .size_full()
            .flex()
            .flex_col()
            .p_3()
            .gap(px(ROW_GAP))
            .rounded_xl()
            .overflow_y_scroll()
            .bg(rgb(p.surface))
            .border_2()
            .border_color(rgb(p.primary))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .text_color(rgb(p.text))
            .child(
                div()
                    .h(px(30.))
                    .flex_shrink_0()
                    .px_2()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Appearance"),
            )
            .children(Theme::ALL.into_iter().enumerate().map(|(index, theme)| {
                let colors = theme.palette();
                let active = theme == selected;
                div()
                    .id(("theme", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(THEME_ROW_HEIGHT))
                    .flex_shrink_0()
                    .rounded_lg()
                    .cursor_pointer()
                    .bg(rgb(if active { p.elevated } else { p.surface }))
                    .hover(move |style| style.bg(rgb(p.elevated)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_theme(theme, window, cx);
                    }))
                    .child(
                        div().flex().gap_1().children(
                            [colors.primary, colors.secondary, colors.accent]
                                .map(|color| div().size(px(10.)).rounded_full().bg(rgb(color))),
                        ),
                    )
                    .child(div().flex_1().child(theme.name()))
                    .child(
                        div()
                            .w(px(16.))
                            .text_color(rgb(p.primary))
                            .child(if active { "✓" } else { "" }),
                    )
            }))
            .child(
                div()
                    .flex_shrink_0()
                    .mt_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(p.border))
                    .child(
                        div()
                            .h(px(26.))
                            .px_2()
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(p.muted))
                            .child("ADDRESS BAR SHOWS"),
                    )
                    .children(AddressDisplay::ALL.into_iter().enumerate().map(
                        |(index, display)| {
                            let active = display == address;
                            div()
                                .id(("address-display", index))
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .h(px(30.))
                                .rounded_lg()
                                .cursor_pointer()
                                .hover(move |style| style.bg(rgb(p.elevated)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.choose_address(display, cx)
                                }))
                                .child(
                                    div()
                                        .size(px(16.))
                                        .rounded_full()
                                        .border_1()
                                        .border_color(rgb(p.primary))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            div().size(px(8.)).rounded_full().bg(rgb(if active {
                                                p.primary
                                            } else {
                                                p.surface
                                            })),
                                        ),
                                )
                                .child(display.name())
                        },
                    )),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .mt_2()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(p.border))
                    .child(
                        div()
                            .id("animate-artwork")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .h(px(30.))
                            .rounded_lg()
                            .cursor_pointer()
                            .hover(move |style| style.bg(rgb(p.elevated)))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_motion(cx)))
                            .child(
                                div()
                                    .size(px(18.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(rgb(p.primary))
                                    .text_color(rgb(p.primary))
                                    .child(if animate { "✓" } else { "" }),
                            )
                            .child("Animate artwork"),
                    )
                    .when(theme::system_reduced_motion(), |view| {
                        view.child(
                            div()
                                .px_2()
                                .pt_1()
                                .text_size(px(11.))
                                .text_color(rgb(p.muted))
                                .child("Paused by macOS Reduce Motion"),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_grows_by_one_complete_theme_row() {
        let five = popup_height(5, false, 1000.);
        let six = popup_height(Theme::ALL.len(), false, 1000.);
        assert_eq!(six - five, THEME_ROW_HEIGHT + ROW_GAP);
        assert_eq!(six, 542.);
        assert_eq!(popup_height(6, true, 1000.), six + 32.);
    }

    #[test]
    fn popup_fits_short_displays_and_handles_invalid_available_height() {
        for reduced_motion in [false, true] {
            for available in [1., 200., 480.] {
                assert_eq!(popup_height(6, reduced_motion, available), available);
            }
            for available in [-100., 0.] {
                assert_eq!(popup_height(6, reduced_motion, available), 1.);
            }
            for available in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert_eq!(
                    popup_height(6, reduced_motion, available),
                    popup_height(6, reduced_motion, 1000.)
                );
            }
        }
        assert!(popup_height(0, false, 1000.) > 0.);
    }
}
