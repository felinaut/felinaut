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

#[derive(Default)]
struct PopupWindow(Option<WindowHandle<SuggestionList>>);
impl Global for PopupWindow {}

/// Open the dropdown beneath the address text (window-relative `anchor`) unless it already is.
/// The list re-renders by observing the browser, so updates need no further calls.
pub fn open(
    browser: WeakEntity<Browser>,
    anchor: Bounds<Pixels>,
    parent: &mut Window,
    cx: &mut App,
) {
    if !cx.has_global::<PopupWindow>() {
        cx.set_global(PopupWindow::default());
    }
    if let Some(handle) = cx.global::<PopupWindow>().0
        && handle.update(cx, |_, _, _| ()).is_ok()
    {
        return;
    }
    let Some(entity) = browser.upgrade() else {
        return;
    };
    let parent_handle = parent.window_handle();
    let parent_bounds = parent.bounds();
    // ponytail: fixed height for MAX_ROWS; the unused remainder is transparent. GPUI's resize
    // keeps the bottom edge, so growing in place would move the top.
    let dimensions = size(
        anchor.size.width + px(28.),
        px(ROW * MAX_ROWS as f32 + 2. * PADDING + 4.),
    );
    let mut origin = parent_bounds.origin + point(anchor.origin.x - px(14.), px(112.));
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
            .w_full()
            .flex()
            .flex_col()
            .p(px(PADDING))
            .rounded_xl()
            .bg(rgb(p.surface))
            .border_2()
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
