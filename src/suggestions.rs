//! Address-bar history dropdown. Like the theme and volume popups it is a native popup window,
//! because a GPUI overlay would sit behind Wry's child NSView. Unlike them it opens without
//! focus: typing and the ↑/↓/Enter/Esc keys stay in the address input, which owns the state.

use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, Entity, FontWeight, Global, HighlightStyle,
    Pixels, StyledText, Subscription, WeakEntity, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, div, point, prelude::*, px, rgb, size,
};

use crate::{
    Browser,
    history::Suggestion,
    theme::{self, ThemeSettings},
};

pub const MAX_ROWS: usize = 8;
const ROW: f32 = 50.;
const PADDING: f32 = 6.;
const BORDER: f32 = 2.;
/// The address text sits this far inside the field's outer edges (`input.rs`: 12 pt padding
/// plus a 1 pt border horizontally, 4 pt padding plus the border vertically).
const FIELD_INSET_X: f32 = 13.;
const FIELD_INSET_Y: f32 = 5.;
/// Space between the bottom of the address field and the top of the list.
const GAP: f32 = 4.;

/// Window height for `rows` rows: exactly the list, with no empty remainder.
fn height(rows: usize) -> Pixels {
    px(ROW * rows.clamp(1, MAX_ROWS) as f32 + 2. * (PADDING + BORDER))
}

/// The dropdown's frame relative to the browser window: as wide as the address field, its top
/// `GAP` below the field. `anchor` is the window-relative bounds of the address text.
fn frame(anchor: Bounds<Pixels>, rows: usize) -> Bounds<Pixels> {
    Bounds::new(
        point(
            anchor.origin.x - px(FIELD_INSET_X),
            anchor.origin.y + anchor.size.height + px(FIELD_INSET_Y + GAP),
        ),
        size(anchor.size.width + px(2. * FIELD_INSET_X), height(rows)),
    )
}

#[derive(Default)]
struct PopupWindow(Option<WindowHandle<SuggestionList>>);
impl Global for PopupWindow {}

/// Open the dropdown beneath the address field, or fit an open one to `rows`. `anchor` is the
/// window-relative bounds of the address text. The list re-renders by observing the browser.
pub fn open(
    browser: WeakEntity<Browser>,
    anchor: Bounds<Pixels>,
    rows: usize,
    parent: &mut Window,
    cx: &mut App,
) {
    if !cx.has_global::<PopupWindow>() {
        cx.set_global(PopupWindow::default());
    }
    if let Some(handle) = cx.global::<PopupWindow>().0
        && handle
            .update(cx, |_, window, _| {
                set_height_keeping_top(window, height(rows))
            })
            .is_ok()
    {
        return;
    }
    let Some(entity) = browser.upgrade() else {
        return;
    };
    let parent_handle = parent.window_handle();
    let parent_bounds = parent.bounds();
    let frame = frame(anchor, rows);
    let dimensions = frame.size;
    let mut origin = parent_bounds.origin + frame.origin;
    let display = parent.display(cx);
    if let Some(display) = &display {
        let screen = display.bounds();
        origin.x = origin
            .x
            .max(screen.origin.x)
            .min((screen.origin.x + screen.size.width - dimensions.width).max(screen.origin.x));
    }
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, dimensions))),
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
        |_, cx| cx.new(|cx| SuggestionList::new(entity, parent_handle, cx)),
    ) {
        Ok(handle) => cx.global_mut::<PopupWindow>().0 = Some(handle),
        Err(error) => eprintln!("Could not open address suggestions: {error}"),
    }
}

/// Resize the open dropdown to `height`, keeping its top edge under the address field. GPUI's
/// `Window::resize` keeps the bottom edge instead (AppKit frames grow from the bottom-left).
#[cfg(target_os = "macos")]
fn set_height_keeping_top(window: &Window, height: Pixels) {
    use objc2_app_kit::NSView;
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // Fully qualified: `Window` has an inherent `window_handle` returning GPUI's own handle.
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    // SAFETY: GPUI hands out its live content view, which outlives this borrow of `window`,
    // and GPUI only calls window updates on the main thread, as AppKit requires.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let Some(ns_window) = view.window() else {
        return;
    };
    let mut frame = ns_window.frame();
    let height = f64::from(f32::from(height));
    if (frame.size.height - height).abs() < 0.5 {
        return;
    }
    frame.origin.y += frame.size.height - height;
    frame.size.height = height;
    ns_window.setFrame_display(frame, true);
}
#[cfg(not(target_os = "macos"))]
fn set_height_keeping_top(window: &mut Window, height: Pixels) {
    window.resize(size(window.bounds().size.width, height));
}

pub fn close(cx: &mut App) {
    if cx.has_global::<PopupWindow>()
        && let Some(handle) = cx.global_mut::<PopupWindow>().0.take()
    {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

/// Whether the dropdown is the key window, which happens when the user clicks a row.
pub fn is_active(cx: &mut App) -> bool {
    if !cx.has_global::<PopupWindow>() {
        return false;
    }
    cx.global::<PopupWindow>().0.is_some_and(|handle| {
        handle
            .update(cx, |_, window, _| window.is_window_active())
            .unwrap_or(false)
    })
}

struct SuggestionList {
    browser: WeakEntity<Browser>,
    parent: AnyWindowHandle,
    _browser_observer: Subscription,
    _theme_observer: Subscription,
}

impl SuggestionList {
    fn new(browser: Entity<Browser>, parent: AnyWindowHandle, cx: &mut Context<Self>) -> Self {
        Self {
            browser: browser.downgrade(),
            parent,
            _browser_observer: cx.observe(&browser, |_, _, cx| cx.notify()),
            _theme_observer: cx.observe_global::<ThemeSettings>(|_, cx| cx.notify()),
        }
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        let browser = self.browser.clone();
        let parent = self.parent;
        // The click made this panel key; hand focus back to the browser before navigating.
        cx.defer(move |cx| {
            let _ = parent.update(cx, |_, window, cx| {
                window.activate_window();
                let _ =
                    browser.update(cx, |browser, cx| browser.open_suggestion(index, window, cx));
            });
        });
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        let browser = self.browser.clone();
        let parent = self.parent;
        cx.defer(move |cx| {
            let _ = parent.update(cx, |_, window, cx| {
                window.activate_window();
                let _ = browser.update(cx, |browser, cx| {
                    browser.remove_suggestion(index, window, cx)
                });
            });
        });
    }
}

fn highlighted(text: &str, ranges: &[std::ops::Range<usize>], color: u32) -> StyledText {
    let style = HighlightStyle {
        color: Some(rgb(color).into()),
        font_weight: Some(FontWeight::SEMIBOLD),
        ..Default::default()
    };
    StyledText::new(text.to_owned()).with_highlights(ranges.iter().map(|r| (r.clone(), style)))
}

impl Render for SuggestionList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let (rows, selected, icons): (Vec<Suggestion>, Option<usize>, Vec<AnyElement>) = self
            .browser
            .upgrade()
            .map(|browser| {
                let browser = browser.read(cx);
                let icons = browser
                    .suggestions
                    .iter()
                    .map(|row| {
                        browser.site_icon(&crate::host(&row.url).unwrap_or_default(), 20., p)
                    })
                    .collect();
                (browser.suggestions.clone(), browser.selected, icons)
            })
            .unwrap_or_default();
        div()
            .id("suggestions")
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .p(px(PADDING))
            .rounded_xl()
            .bg(rgb(p.surface))
            .border(px(BORDER))
            .border_color(rgb(p.primary))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .text_color(rgb(p.text))
            .children(
                rows.iter()
                    .zip(icons)
                    .enumerate()
                    .map(|(index, (row, icon))| {
                        let active = selected == Some(index);
                        div()
                            .id(("suggestion", index))
                            .group("suggestion")
                            .h(px(ROW))
                            .flex_shrink_0()
                            .px_3()
                            .rounded_lg()
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap_3()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .bg(rgb(if active { p.elevated } else { p.surface }))
                            .hover(move |style| style.bg(rgb(p.elevated)))
                            .on_click(cx.listener(move |this, _, _, cx| this.choose(index, cx)))
                            .child(div().flex_shrink_0().child(icon))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(div().text_ellipsis().child(highlighted(
                                        &row.title,
                                        &row.title_ranges,
                                        p.primary,
                                    )))
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(rgb(p.muted))
                                            .text_ellipsis()
                                            .child(highlighted(
                                                &row.display_url,
                                                &row.url_ranges,
                                                p.primary,
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .id(("forget", index))
                                    .flex_shrink_0()
                                    .size(px(22.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .text_size(px(15.))
                                    .text_color(rgb(p.muted))
                                    .opacity(0.)
                                    .group_hover("suggestion", |style| style.opacity(1.))
                                    .hover(move |style| {
                                        style.bg(rgb(p.surface)).text_color(rgb(p.text))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.remove(index, cx);
                                    }))
                                    .child("×"),
                            )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address text as painted in the sidebar layout: 34 pt field, toolbar starting at y 12.
    fn anchor() -> Bounds<Pixels> {
        Bounds::new(point(px(431.), px(17.)), size(px(642.), px(24.)))
    }

    #[test]
    fn list_starts_just_below_the_address_field() {
        let field_bottom = px(17. + 24. + FIELD_INSET_Y);
        let frame = frame(anchor(), 3);
        assert_eq!(frame.origin.y, field_bottom + px(GAP));
        // The list follows the field wherever the toolbar puts it, not a fixed offset.
        let lower = Bounds::new(point(px(431.), px(80.)), anchor().size);
        assert_eq!(super::frame(lower, 3).origin.y - frame.origin.y, px(63.));
    }

    #[test]
    fn list_is_exactly_as_wide_as_the_address_field() {
        let frame = frame(anchor(), 3);
        assert_eq!(frame.origin.x, px(431. - 13.));
        assert_eq!(frame.size.width, px(642. + 26.));
    }

    #[test]
    fn window_fits_its_rows_without_an_empty_panel() {
        let chrome = 2. * (PADDING + BORDER);
        assert_eq!(height(1), px(ROW + chrome));
        assert_eq!(height(2), px(2. * ROW + chrome));
        assert!(height(2) < height(MAX_ROWS));
        assert_eq!(frame(anchor(), 2).size.height, height(2));
    }

    #[test]
    fn row_count_is_clamped() {
        assert_eq!(height(0), height(1));
        assert_eq!(height(MAX_ROWS + 20), height(MAX_ROWS));
    }
}
