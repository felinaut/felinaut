//! Per-tab volume popover. Like the theme dropdown it is a native popup window, because a
//! GPUI-painted overlay would sit behind Wry's child NSView.

use std::{cell::Cell, rc::Rc};

use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight, Global,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, ScrollWheelEvent,
    SharedString, Subscription, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, actions, canvas, div, linear_color_stop,
    linear_gradient, point, prelude::*, px, relative, rgb, size,
};

use crate::{
    Browser,
    audio::{self, Preset, TabAudio},
    brand::{self, icon},
    theme::{self, Palette, ThemeSettings},
};

actions!(
    volume,
    [CloseVolume, VolumeUp, VolumeDown, ToggleMute, ResetVolume]
);

const WIDTH: f32 = 348.;
const HEIGHT: f32 = 600.;
const KNOB: f32 = 30.;

/// One tab as the popover shows it.
#[derive(Clone)]
pub struct TabSound {
    pub id: u64,
    pub title: SharedString,
    pub audio: TabAudio,
    pub audible: bool,
    pub limited: bool,
    pub active: bool,
}

/// The active tab plus every tab that is playing sound or has non-default settings.
pub struct Overview {
    pub current: TabSound,
    pub tabs: Vec<TabSound>,
}

pub fn speaker_icon(audio: TabAudio) -> &'static str {
    match audio.level() {
        0 => icon::SPEAKER_MUTED,
        1 => icon::SPEAKER_LOW,
        2 => icon::SPEAKER,
        _ => icon::SPEAKER_BOOST,
    }
}

fn preset_icon(preset: Preset) -> &'static str {
    match preset {
        Preset::Flat => icon::NATURAL,
        Preset::Voice => icon::VOICE,
        Preset::Bass => icon::BASS,
    }
}

#[derive(Default)]
struct PopupWindow(Option<WindowHandle<VolumePanel>>);
impl Global for PopupWindow {}

/// Toggle the popover. `anchor_x` is the window-relative x of the toolbar button, if known.
pub fn toggle(
    browser: WeakEntity<Browser>,
    anchor_x: Option<Pixels>,
    parent: &mut Window,
    cx: &mut App,
) {
    if !cx.has_global::<PopupWindow>() {
        cx.set_global(PopupWindow::default());
        let context = Some("FelifoxVolume");
        cx.bind_keys([
            KeyBinding::new("escape", CloseVolume, context),
            KeyBinding::new("right", VolumeUp, context),
            KeyBinding::new("up", VolumeUp, context),
            KeyBinding::new("left", VolumeDown, context),
            KeyBinding::new("down", VolumeDown, context),
            KeyBinding::new("m", ToggleMute, context),
            KeyBinding::new("0", ResetVolume, context),
        ]);
    }
    if let Some(handle) = cx.global_mut::<PopupWindow>().0.take()
        && handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
    {
        return;
    }
    let Some(entity) = browser.upgrade() else {
        return;
    };

    let parent_handle = parent.window_handle();
    let parent_bounds = parent.bounds();
    let dimensions = size(px(WIDTH), px(HEIGHT));
    // Right-align the popover's speaker with the button when we know where it is.
    let anchor = anchor_x
        .map(|x| x + px(56.) - dimensions.width)
        .unwrap_or(parent_bounds.size.width - dimensions.width - px(120.))
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
        |window, cx| cx.new(|cx| VolumePanel::new(entity, parent_handle, window, cx)),
    ) {
        Ok(handle) => cx.global_mut::<PopupWindow>().0 = Some(handle),
        Err(error) => eprintln!("Could not open volume controls: {error}"),
    }
}

struct VolumePanel {
    browser: WeakEntity<Browser>,
    parent: AnyWindowHandle,
    focus: FocusHandle,
    was_active: bool,
    dragging: bool,
    scroll: f32,
    track: Rc<Cell<Bounds<Pixels>>>,
    _browser_observer: Subscription,
    _theme_observer: Subscription,
    _activation_observer: Subscription,
}

impl VolumePanel {
    fn new(
        browser: Entity<Browser>,
        parent: AnyWindowHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let browser_observer = cx.observe(&browser, |_, _, cx| cx.notify());
        let theme_observer = cx.observe_global::<ThemeSettings>(|_, cx| cx.notify());
        let activation_observer = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.was_active = true;
            } else if this.was_active {
                this.close(window, cx, false);
            }
        });
        Self {
            browser: browser.downgrade(),
            parent,
            focus,
            was_active: window.is_window_active(),
            dragging: false,
            scroll: 0.,
            track: Rc::new(Cell::new(Bounds::default())),
            _browser_observer: browser_observer,
            _theme_observer: theme_observer,
            _activation_observer: activation_observer,
        }
    }

    fn overview(&self, cx: &App) -> Option<Overview> {
        Some(self.browser.upgrade()?.read(cx).sound_overview())
    }

    fn current(&self, cx: &App) -> Option<TabSound> {
        self.overview(cx).map(|overview| overview.current)
    }

    fn set(&mut self, id: u64, audio: TabAudio, cx: &mut Context<Self>) {
        if let Some(browser) = self.browser.upgrade() {
            browser.update(cx, |browser, cx| browser.set_audio(id, audio, cx));
        }
    }

    fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(TabAudio) -> TabAudio) {
        if let Some(current) = self.current(cx) {
            self.set(current.id, edit(current.audio), cx);
        }
    }

    fn set_percent(&mut self, percent: u16, cx: &mut Context<Self>) {
        // Moving the slider is an obvious request to hear the tab again.
        self.change(cx, |audio| TabAudio {
            muted: false,
            ..audio.with_percent(percent)
        });
    }

    fn step(&mut self, up: bool, cx: &mut Context<Self>) {
        if let Some(current) = self.current(cx) {
            self.set_percent(audio::step_percent(current.audio.percent, up), cx);
        }
    }

    fn drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let track = self.track.get();
        if track.size.width <= px(0.) {
            return;
        }
        let position = (x - track.origin.x) / track.size.width;
        self.set_percent(audio::position_to_percent(position), cx);
    }

    fn select_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let browser = self.browser.clone();
        let parent = self.parent;
        self.close(window, cx, false);
        cx.defer(move |cx| {
            let _ = parent.update(cx, |_, window, cx| {
                window.activate_window();
                let _ = browser.update(cx, |browser, cx| browser.activate_tab_id(id, window, cx));
            });
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
            cx.defer(move |cx| {
                let _ = parent.update(cx, |_, window, _| window.activate_window());
            });
        }
    }

    fn header(
        &self,
        current: &TabSound,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let audio = current.audio;
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
                        brand::icon(speaker_icon(audio))
                            .size(px(22.))
                            .text_color(rgb(if audio.muted { p.muted } else { p.primary })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Tab Volume"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(current.title.clone()),
                    ),
            )
            .child(
                div()
                    .id("mute")
                    .h(px(32.))
                    .px_3()
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if audio.muted { p.primary } else { p.border }))
                    .bg(rgb(if audio.muted { p.elevated } else { p.surface }))
                    .text_color(rgb(if audio.muted { p.primary } else { p.text }))
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(move |style| style.border_color(rgb(p.primary)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.change(cx, |audio| TabAudio {
                            muted: !audio.muted,
                            ..audio
                        })
                    }))
                    .child(
                        brand::icon(if audio.muted {
                            icon::SPEAKER
                        } else {
                            icon::SPEAKER_MUTED
                        })
                        .size(px(14.))
                        .text_color(rgb(if audio.muted {
                            p.primary
                        } else {
                            p.muted
                        })),
                    )
                    .child(if audio.muted { "Unmute" } else { "Mute" }),
            )
    }

    fn readout(&self, audio: TabAudio, p: Palette) -> impl IntoElement + use<> {
        let (label, background, foreground) = if audio.muted {
            ("Muted", p.elevated, p.muted)
        } else if audio.boosted() {
            ("Boosted", p.accent, 0x2b0a54)
        } else if audio.percent < 100 {
            ("Quieter", p.elevated, p.text)
        } else {
            ("Natural level", p.elevated, p.muted)
        };
        div()
            .flex()
            .items_end()
            .justify_between()
            .child(
                div()
                    .font_family("Georgia")
                    .italic()
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(48.))
                    .line_height(px(52.))
                    .text_color(rgb(if audio.muted { p.muted } else { p.primary }))
                    .child(format!("{}%", audio.percent)),
            )
            .child(
                div()
                    .mb_2()
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .bg(rgb(background))
                    .text_color(rgb(foreground))
                    .text_size(px(11.))
                    .font_weight(FontWeight::BOLD)
                    .child(label.to_uppercase()),
            )
    }

    fn slider(
        &self,
        audio: TabAudio,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let position = audio::percent_to_position(audio.percent);
        let (normal, hot) = if audio.muted {
            (p.border, p.border)
        } else {
            (p.primary, p.accent)
        };
        let track = self.track.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                // Horizontal padding keeps the knob inside the window at 0 % and 600 %.
                div()
                    .id("volume-slider")
                    .h(px(40.))
                    .px(px(KNOB / 2.))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.dragging = true;
                            window.focus(&this.focus);
                            this.drag_to(event.position.x, cx);
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        let delta = event.delta.pixel_delta(px(16.));
                        // Trackpads scroll in both axes; follow whichever moved more.
                        let amount = if delta.x.abs() > delta.y.abs() {
                            f32::from(delta.x)
                        } else {
                            -f32::from(delta.y)
                        };
                        this.scroll += amount;
                        while this.scroll.abs() >= 18. {
                            let up = this.scroll > 0.;
                            this.scroll -= 18f32.copysign(this.scroll);
                            this.step(up, cx);
                        }
                    }))
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .child(
                                canvas(move |bounds, _, _| track.set(bounds), |_, _, _, _| {})
                                    .absolute()
                                    .inset_0(),
                            )
                            // Track: natural half and the "flame" boost half.
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top(px(15.))
                                    .h(px(10.))
                                    .rounded_full()
                                    .overflow_hidden()
                                    .bg(rgb(p.elevated))
                                    .child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .left(relative(0.5))
                                            .right_0()
                                            .bg(linear_gradient(
                                                90.,
                                                linear_color_stop(
                                                    gpui::Hsla::from(rgb(p.accent)).opacity(0.18),
                                                    0.,
                                                ),
                                                linear_color_stop(
                                                    gpui::Hsla::from(rgb(p.primary)).opacity(0.28),
                                                    1.,
                                                ),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .bottom_0()
                                            .left_0()
                                            .w(relative(position.min(0.5)))
                                            .bg(rgb(normal)),
                                    )
                                    .when(position > 0.5, |track| {
                                        track.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .bottom_0()
                                                .left(relative(0.5))
                                                .w(relative(position - 0.5))
                                                .bg(linear_gradient(
                                                    90.,
                                                    linear_color_stop(rgb(normal), 0.),
                                                    linear_color_stop(rgb(hot), 1.),
                                                )),
                                        )
                                    }),
                            )
                            // The natural-level notch.
                            .child(
                                div().absolute().left(relative(0.5)).top(px(9.)).child(
                                    div()
                                        .absolute()
                                        .left(px(-1.5))
                                        .w(px(3.))
                                        .h(px(22.))
                                        .rounded_full()
                                        .bg(rgb(p.text)),
                                ),
                            )
                            // The knob is the flaming-yarn mark.
                            .child(
                                div().absolute().left(relative(position)).top(px(5.)).child(
                                    div()
                                        .absolute()
                                        .left(px(-KNOB / 2.))
                                        .size(px(KNOB))
                                        .rounded_full()
                                        .bg(rgb(p.surface))
                                        .border_2()
                                        .border_color(rgb(if audio.muted {
                                            p.border
                                        } else {
                                            p.primary
                                        }))
                                        .shadow_md()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            brand::symbol().size(px(22.)).opacity(if audio.muted {
                                                0.45
                                            } else {
                                                1.
                                            }),
                                        ),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .px(px(KNOB / 2.))
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child(div().flex_1().child("0%"))
                    .child(div().flex_1().flex().justify_center().child("100%"))
                    .child(div().flex_1().flex().justify_end().child("600%")),
            )
    }

    fn steps(
        &self,
        audio: TabAudio,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let chip = move |id: &'static str, label: &'static str, enabled: bool| {
            div()
                .id(id)
                .h(px(32.))
                .min_w(px(40.))
                .px_3()
                .rounded_lg()
                .border_1()
                .border_color(rgb(p.border))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .when(enabled, |chip| {
                    chip.cursor_pointer()
                        .hover(move |style| style.bg(rgb(p.elevated)).border_color(rgb(p.primary)))
                })
                .when(!enabled, |chip| chip.opacity(0.4))
                .child(label)
        };
        div()
            .flex()
            .gap_2()
            .child(
                chip("quieter", "−", audio.percent > 0)
                    .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                chip("louder", "+", audio.percent < audio::MAX_PERCENT)
                    .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(div().flex_1())
            .children([200u16, 400].map(|percent| {
                let active = !audio.muted && audio.percent == percent;
                chip(
                    if percent == 200 {
                        "boost-200"
                    } else {
                        "boost-400"
                    },
                    if percent == 200 { "200%" } else { "400%" },
                    !active,
                )
                .when(active, |chip| chip.opacity(1.).border_color(rgb(p.primary)))
                .on_click(cx.listener(move |this, _, _, cx| this.set_percent(percent, cx)))
            }))
            .child(
                chip("reset", "Reset", !audio.is_default()).on_click(
                    cx.listener(|this, _, _, cx| this.change(cx, |_| TabAudio::default())),
                ),
            )
    }

    fn presets(
        &self,
        audio: TabAudio,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .gap_2()
            .children(Preset::ALL.into_iter().enumerate().map(|(index, preset)| {
                let active = audio.preset == preset;
                div()
                    .id(("preset", index))
                    .flex_1()
                    .h(px(76.))
                    .rounded_xl()
                    .border_2()
                    .border_color(rgb(if active { p.primary } else { p.border }))
                    .bg(rgb(if active { p.elevated } else { p.surface }))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(p.elevated)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change(cx, |audio| TabAudio { preset, ..audio })
                    }))
                    .child(
                        brand::icon(preset_icon(preset))
                            .size(px(20.))
                            .text_color(rgb(if active { p.primary } else { p.muted })),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(preset.name()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(p.muted))
                            .child(preset.hint()),
                    )
            }))
    }

    fn tab_row(
        &self,
        tab: TabSound,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = tab.id;
        let audio = tab.audio;
        div()
            .id(("sound-tab", id))
            .h(px(44.))
            .flex_shrink_0()
            .px_2()
            .rounded_lg()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(if tab.active { p.elevated } else { p.surface }))
            .when(!tab.active, |row| {
                row.cursor_pointer()
                    .hover(move |style| style.bg(rgb(p.elevated)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_tab(id, window, cx)
                    }))
            })
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(rgb(
                if tab.audible && !audio.muted {
                    p.highlight
                } else {
                    p.border
                },
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(13.))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(tab.title.clone()),
                    )
                    .child(div().text_size(px(11.)).text_color(rgb(p.muted)).child(
                        match (tab.active, tab.audible) {
                            (true, true) => "This tab · playing",
                            (true, false) => "This tab",
                            (false, true) => "Playing · click to switch",
                            (false, false) => "Click to switch",
                        },
                    )),
            )
            .when_some(audio.badge(), |row, badge| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(if audio.boosted() { p.primary } else { p.muted }))
                        .child(badge),
                )
            })
            .child(
                div()
                    .id(("sound-tab-mute", id))
                    .size(px(30.))
                    .flex_shrink_0()
                    .rounded_lg()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(p.background)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.set(
                            id,
                            TabAudio {
                                muted: !audio.muted,
                                ..audio
                            },
                            cx,
                        );
                    }))
                    .child(
                        brand::icon(speaker_icon(audio))
                            .size(px(16.))
                            .text_color(rgb(if audio.muted { p.muted } else { p.primary })),
                    ),
            )
    }
}

fn section(label: &'static str, p: Palette) -> gpui::Div {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(p.muted))
        .child(label)
}

impl Render for VolumePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let root = div()
            .id("volume")
            .key_context("FelifoxVolume")
            .track_focus(&self.focus)
            .size_full()
            .rounded_xl()
            .overflow_hidden()
            .bg(rgb(p.surface))
            .border_2()
            .border_color(rgb(p.primary))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .text_color(rgb(p.text));
        let Some(Overview { current, tabs }) = self.overview(cx) else {
            return root;
        };
        let audio = current.audio;
        let limited = current.limited;
        let tab_count = tabs.len();
        let empty = tabs.is_empty();
        let rows: Vec<_> = tabs
            .into_iter()
            .map(|tab| self.tab_row(tab, p, cx))
            .collect();
        root.on_action(cx.listener(|this, _: &CloseVolume, window, cx| {
            cx.stop_propagation();
            this.close(window, cx, true);
        }))
        .on_action(cx.listener(|this, _: &VolumeUp, _, cx| this.step(true, cx)))
        .on_action(cx.listener(|this, _: &VolumeDown, _, cx| this.step(false, cx)))
        .on_action(cx.listener(|this, _: &ToggleMute, _, cx| {
            this.change(cx, |audio| TabAudio {
                muted: !audio.muted,
                ..audio
            })
        }))
        .on_action(
            cx.listener(|this, _: &ResetVolume, _, cx| this.change(cx, |_| TabAudio::default())),
        )
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
            if !this.dragging {
                return;
            }
            if event.pressed_button == Some(MouseButton::Left) {
                this.drag_to(event.position.x, cx);
            } else {
                this.dragging = false;
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, _| this.dragging = false),
        )
        .flex()
        .flex_col()
        .p_4()
        .gap_3()
        .child(self.header(&current, p, cx))
        .child(self.readout(audio, p))
        .child(self.slider(audio, p, cx))
        .child(self.steps(audio, p, cx))
        .child(section("SOUND", p).mt_1())
        .child(self.presets(audio, p, cx))
        .child(
            div()
                .mt_1()
                .flex()
                .justify_between()
                .child(section("TABS WITH SOUND", p))
                .when(tab_count > 0, |row| {
                    row.child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(tab_count.to_string()),
                    )
                }),
        )
        .child(
            div()
                .id("sound-tabs")
                .flex_1()
                .min_h(px(48.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .when(empty, |list| {
                    list.child(
                        div()
                            .h(px(48.))
                            .px_2()
                            .rounded_lg()
                            .bg(rgb(p.background))
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .child(brand::symbol().size(px(22.)).opacity(0.8))
                            .child("Quiet as a cat. Tabs playing sound appear here."),
                    )
                })
                .children(rows),
        )
        .when(limited && !audio.muted, |panel| {
            panel.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(rgb(p.background))
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child(
                        "Some media here comes from another site, so it can be lowered or \
                         muted but not boosted or tuned.",
                    ),
            )
        })
        .child(
            div()
                .flex()
                .justify_center()
                .text_size(px(11.))
                .text_color(rgb(p.muted))
                .child("← →  adjust   ·   M  mute   ·   0  reset   ·   esc  close"),
        )
    }
}
