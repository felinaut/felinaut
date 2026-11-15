//! The small "Link copied" pop-up after ⌘⇧C. Like the address suggestions it is a native popup
//! window, because a GPUI overlay would sit behind Wry's child NSView, and it opens without
//! focus so typing and shortcuts stay where they were. It closes itself after `SHOWN_FOR`.

use std::time::Duration;

use gpui::{
    App, Bounds, Context, Global, Pixels, Point, Size, Timer, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, size,
};

use crate::clean_link::CleanLink;
use crate::theme::{self, ThemeSettings};

/// How long the pop-up stays.
pub const SHOWN_FOR: Duration = Duration::from_millis(1600);
const WIDTH: f32 = 300.;
const HEIGHT: f32 = 44.;
/// Gap between the toolbar and the pop-up.
const GAP: f32 = 10.;
/// Transparent space around the pop-up inside its window. Without it the window edge clips the
/// rounded corners, the border and the shadow.
const MARGIN: f32 = 28.;

/// What ⌘⇧C did, for the pop-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Copied {
    /// The link was copied and tracking was removed from it.
    Cleaned,
    /// The link was copied; it had nothing to remove.
    Unchanged,
    /// The page (New Tab, an empty tab) has no web address to copy.
    NoLink,
}

impl Copied {
    /// What the pop-up says for a copied link (`None` when nothing was copied).
    pub fn from(link: Option<&CleanLink>) -> Self {
        match link {
            None => Self::NoLink,
            Some(link) if link.removed => Self::Cleaned,
            Some(_) => Self::Unchanged,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Cleaned | Self::Unchanged => "Link copied",
            Self::NoLink => "No link to copy",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Cleaned => "Tracking removed",
            Self::Unchanged => "No tracking found",
            Self::NoLink => "Not a web page",
        }
    }
}

/// Where the pop-up goes in window coordinates: centred over the page (right of the sidebar),
/// just below the toolbar.
pub fn frame(window: Size<Pixels>, sidebar: f32, toolbar: f32) -> Bounds<Pixels> {
    let window_width = f32::from(window.width);
    let sidebar = sidebar.clamp(0., window_width);
    let page = (window_width - sidebar).max(0.);
    let width = WIDTH.min(window_width);
    let x = (sidebar + (page - width) / 2.).clamp(0., (window_width - width).max(0.));
    Bounds::new(point(px(x), px(toolbar + GAP)), size(px(width), px(HEIGHT)))
}

/// The native window for a pop-up at `pill`: the pop-up plus `MARGIN` on every side.
pub fn window_frame(pill: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        pill.origin - point(px(MARGIN), px(MARGIN)),
        pill.size + size(px(2. * MARGIN), px(2. * MARGIN)),
    )
}

#[derive(Default)]
struct ToastWindow(Option<WindowHandle<Toast>>);
impl Global for ToastWindow {}

/// Show the pop-up for `copied` over `parent`, replacing one that is still showing.
pub fn show(copied: Copied, parent: &Window, sidebar: f32, toolbar: f32, cx: &mut App) {
    if !cx.has_global::<ToastWindow>() {
        cx.set_global(ToastWindow::default());
    }
    if let Some(handle) = cx.global_mut::<ToastWindow>().0.take() {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
    let parent_bounds = parent.bounds();
    let frame = window_frame(frame(parent_bounds.size, sidebar, toolbar));
    let origin: Point<Pixels> = parent_bounds.origin + frame.origin;
    let display = parent.display(cx);
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, frame.size))),
            titlebar: None,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            focus: false,
            display_id: display.map(|display| display.id()),
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Toast::new(copied, window, cx)),
    ) {
        Ok(handle) => cx.global_mut::<ToastWindow>().0 = Some(handle),
        Err(error) => eprintln!("Could not show the copied-link pop-up: {error}"),
    }
}

struct Toast {
    copied: Copied,
}

impl Toast {
    fn new(copied: Copied, window: &mut Window, cx: &mut Context<Self>) -> Self {
        make_borderless(window);
        // Each pop-up closes itself; a newer one has already replaced it if this one is gone.
        cx.spawn_in(window, async move |_, cx| {
            Timer::after(SHOWN_FOR).await;
            let _ = cx.update(|window, _| window.remove_window());
        })
        .detach();
        cx.observe_global::<ThemeSettings>(|_, cx| cx.notify())
            .detach();
        Self { copied }
    }
}

/// GPUI gives title-less popups a titled, full-size-content window, and macOS draws its own
/// rounded panel and shadow for those behind the transparent content. A borderless panel shows
/// only what the pop-up draws: its rounded box and shadow inside `MARGIN`, without AppKit's.
#[cfg(target_os = "macos")]
fn make_borderless(window: &Window) {
    use objc2_app_kit::{NSView, NSWindowStyleMask};
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // Fully qualified: `Window` has an inherent `window_handle` returning GPUI's own handle.
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: GPUI hands out its live content view, which outlives this borrow of `window`,
    // and GPUI only builds windows on the main thread, as AppKit requires.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let Some(ns_window) = view.window() else {
        return;
    };
    ns_window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    // The pop-up draws its own soft shadow; AppKit's would trace the rounded edge a second time.
    ns_window.setHasShadow(false);
    // It only confirms; clicks, including on its transparent margin, go to the page below.
    ns_window.setIgnoresMouseEvents(true);
}
#[cfg(not(target_os = "macos"))]
fn make_borderless(_: &Window) {}

impl Render for Toast {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let ok = self.copied != Copied::NoLink;
        // The margin stays transparent; only the rounded pop-up inside it is drawn.
        div().size_full().p(px(MARGIN)).child(
            div()
                .id("link-toast")
                .size_full()
                .shadow_lg()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .rounded_xl()
                .bg(rgb(p.elevated))
                .border_1()
                .border_color(rgb(if ok { p.primary } else { p.border }))
                .text_color(rgb(p.text))
                .font_family(".AppleSystemUIFont")
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(22.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(if ok { p.primary } else { p.muted }))
                        .text_color(rgb(p.background))
                        .text_size(px(13.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(if ok { "✓" } else { "!" }),
                )
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(13.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(self.copied.title()),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(p.muted))
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(self.copied.detail()),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_whether_tracking_was_removed() {
        // Driven by what `clean` reports, not by comparing strings: a link that only lost an
        // empty `?` or `&&` has no tracking removed.
        let copied = |address: &str| Copied::from(crate::clean_link::clean(address).as_ref());
        assert_eq!(copied("https://a.example/?utm_source=x"), Copied::Cleaned);
        assert_eq!(copied("https://a.example/#:~:text=hi"), Copied::Cleaned);
        assert_eq!(copied("https://a.example/?id=1"), Copied::Unchanged);
        assert_eq!(copied("https://a.example/path?"), Copied::Unchanged);
        assert_eq!(copied("https://a.example/?a=1&&b=2"), Copied::Unchanged);
        assert_eq!(copied("https://a.example"), Copied::Unchanged);
        assert_eq!(copied("felinaut://newtab"), Copied::NoLink);
        assert_eq!(copied(""), Copied::NoLink);
        for copied in [Copied::Cleaned, Copied::Unchanged, Copied::NoLink] {
            assert!(!copied.title().is_empty() && !copied.detail().is_empty());
        }
        assert_eq!(Copied::Cleaned.title(), Copied::Unchanged.title());
        assert_ne!(Copied::Cleaned.detail(), Copied::Unchanged.detail());
    }

    #[test]
    fn sits_centred_over_the_page_below_the_toolbar() {
        let window = size(px(1280.), px(900.));
        // Sidebar shown: centred in the 1040 pt to its right.
        let frame = frame(window, 240., 50.);
        assert_eq!(
            frame.origin,
            point(px(240. + (1040. - WIDTH) / 2.), px(60.))
        );
        assert_eq!(frame.size, size(px(WIDTH), px(HEIGHT)));
        // Sidebar hidden: centred in the window.
        assert_eq!(frame_x(window, 0.), (1280. - WIDTH) / 2.);
    }

    #[test]
    fn stays_inside_small_windows_and_odd_sidebar_widths() {
        for (width, sidebar) in [
            (200., 0.),
            (200., 400.),
            (320., 300.),
            (1000., -50.),
            (1000., f32::INFINITY),
            (0., 0.),
        ] {
            let window = size(px(width), px(700.));
            let frame = frame(window, sidebar, 50.);
            let x = f32::from(frame.origin.x);
            let w = f32::from(frame.size.width);
            assert!(x >= 0., "{width} {sidebar}: x {x}");
            assert!(
                x + w <= width.max(0.) + 0.001,
                "{width} {sidebar}: right {}",
                x + w
            );
        }
    }

    #[test]
    fn its_window_leaves_room_for_the_corners_border_and_shadow() {
        let pill = frame(size(px(1280.), px(900.)), 240., 50.);
        let window = window_frame(pill);
        assert_eq!(window.origin.x + px(MARGIN), pill.origin.x);
        assert_eq!(window.origin.y + px(MARGIN), pill.origin.y);
        assert_eq!(window.size.width, pill.size.width + px(2. * MARGIN));
        assert_eq!(window.size.height, pill.size.height + px(2. * MARGIN));
        // shadow_lg reaches about 25 pt below the pop-up; a smaller margin cuts it off square.
        const { assert!(MARGIN >= 25., "the shadow needs space outside the pop-up") };
    }

    fn frame_x(window: Size<Pixels>, sidebar: f32) -> f32 {
        f32::from(frame(window, sidebar, 50.).origin.x)
    }
}
