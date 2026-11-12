//! The downloads list and its toolbar button. Like the volume popover the list is a native
//! popup window, because a GPUI-painted overlay would sit behind Wry's child NSView.

use gpui::{
    AnyWindowHandle, App, Bounds, Context, Div, Entity, FocusHandle, FontWeight, Global,
    KeyBinding, SharedString, Stateful, Subscription, WeakEntity, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, actions,
    div, point, prelude::*, px, relative, rgb, size,
};

use crate::{
    Browser, brand,
    downloads::{self, Activity, Download, Downloads, State},
    theme::{self, Palette, ThemeSettings},
};

actions!(
    downloads,
    [
        CloseDownloads,
        SelectPrevious,
        SelectNext,
        OpenSelected,
        RevealSelected,
        RemoveSelected
    ]
);

const WIDTH: f32 = 400.;
const ROW: f32 = 66.;
/// Padding, heading, key hints and the gaps between them.
const FRAME: f32 = 104.;

/// Tall enough for up to five downloads; more scroll.
fn height(rows: usize) -> f32 {
    FRAME + rows.clamp(1, 5) as f32 * ROW
}

/// The toolbar button: an arrow, with a bar under it while something is downloading. The bar
/// fills with the overall progress, or stays half-lit when a size is unknown.
pub fn button(downloads: &Downloads, p: Palette) -> Stateful<Div> {
    let activity = downloads.activity();
    let active = activity != Activity::Idle;
    div()
        .id("downloads")
        .relative()
        .h(px(34.))
        .min_w(px(34.))
        .px_2()
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(if active { p.primary } else { p.muted }))
        .cursor_pointer()
        .when(active, |button| button.bg(rgb(p.elevated)))
        .hover(move |style| style.bg(rgb(p.elevated)).text_color(rgb(p.text)))
        .child("↓")
        .when(active, |button| {
            button.child(
                div()
                    .absolute()
                    .left(px(7.))
                    .right(px(7.))
                    .bottom(px(4.))
                    .child(bar(activity, p)),
            )
        })
}

fn bar(activity: Activity, p: Palette) -> Div {
    let fill = div().h_full().rounded_full().bg(rgb(p.primary));
    div()
        .h(px(3.))
        .rounded_full()
        .overflow_hidden()
        .bg(rgb(p.border))
        .child(match activity {
            Activity::Fraction(fraction) => fill.w(relative(fraction.clamp(0.03, 1.))),
            _ => fill.w_full().opacity(0.45),
        })
}

/// Stops a running download: the list shows it as cancelled and its partial file is deleted.
pub fn cancel(downloads: &mut Downloads, id: u64) -> bool {
    downloads
        .cancel(id)
        .map(downloads::native::cancel)
        .is_some()
}

#[derive(Default)]
struct PopupWindow(Option<WindowHandle<DownloadPanel>>);
impl Global for PopupWindow {}

/// Toggle the list beneath the parent's toolbar. It reads the browser as soon as it draws, so
/// call this deferred, never from inside one of the browser's own updates.
pub fn toggle(browser: WeakEntity<Browser>, parent: &mut Window, cx: &mut App) {
    if !cx.has_global::<PopupWindow>() {
        cx.set_global(PopupWindow::default());
        let context = Some("FelinautDownloads");
        cx.bind_keys([
            KeyBinding::new("escape", CloseDownloads, context),
            KeyBinding::new("up", SelectPrevious, context),
            KeyBinding::new("down", SelectNext, context),
            KeyBinding::new("enter", OpenSelected, context),
            KeyBinding::new("cmd-enter", RevealSelected, context),
            KeyBinding::new("backspace", RemoveSelected, context),
            KeyBinding::new("delete", RemoveSelected, context),
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
    let rows = entity.read(cx).downloads.items().len();
    let dimensions = size(px(WIDTH), px(height(rows)));
    let mut origin = parent_bounds.origin
        + point(
            parent_bounds.size.width - dimensions.width - px(10.),
            px(116.),
        );
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
        |window, cx| cx.new(|cx| DownloadPanel::new(entity, parent_handle, window, cx)),
    ) {
        Ok(handle) => cx.global_mut::<PopupWindow>().0 = Some(handle),
        Err(error) => eprintln!("Could not open downloads: {error}"),
    }
}

struct DownloadPanel {
    browser: WeakEntity<Browser>,
    parent: AnyWindowHandle,
    focus: FocusHandle,
    was_active: bool,
    /// The download the keys act on, by id.
    selected: Option<u64>,
    _browser_observer: Subscription,
    _theme_observer: Subscription,
    _activation_observer: Subscription,
}

impl DownloadPanel {
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
                // Never steal focus back when dismissing because another window became active.
                this.close(window, cx, false);
            }
        });
        Self {
            // Return opens the newest download straight away.
            selected: browser
                .read(cx)
                .downloads
                .items()
                .last()
                .map(|item| item.id),
            browser: browser.downgrade(),
            parent,
            focus,
            was_active: window.is_window_active(),
            _browser_observer: browser_observer,
            _theme_observer: theme_observer,
            _activation_observer: activation_observer,
        }
    }

    /// Newest first, as the list shows them.
    fn items(&self, cx: &App) -> Vec<Download> {
        self.browser.upgrade().map_or_else(Vec::new, |browser| {
            browser
                .read(cx)
                .downloads
                .items()
                .iter()
                .rev()
                .cloned()
                .collect()
        })
    }

    fn selected(&self, cx: &App) -> Option<Download> {
        let selected = self.selected?;
        self.items(cx).into_iter().find(|item| item.id == selected)
    }

    fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Downloads)) {
        if let Some(browser) = self.browser.upgrade() {
            browser.update(cx, |browser, cx| {
                change(&mut browser.downloads);
                cx.notify();
            });
        }
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let items = self.items(cx);
        let current = items.iter().position(|item| Some(item.id) == self.selected);
        let next = match (current, down) {
            (None, _) => 0,
            (Some(index), true) => (index + 1).min(items.len().saturating_sub(1)),
            (Some(index), false) => index.saturating_sub(1),
        };
        self.selected = items.get(next).map(|item| item.id);
        cx.notify();
    }

    fn cancel(&mut self, id: u64, cx: &mut Context<Self>) {
        self.edit(cx, |downloads| {
            cancel(downloads, id);
        });
    }

    /// Cancels a running download; forgets any other, leaving its file alone.
    fn remove(&mut self, item: &Download, cx: &mut Context<Self>) {
        if item.state == State::Running {
            self.cancel(item.id, cx);
        } else {
            let id = item.id;
            self.edit(cx, |downloads| {
                downloads.remove(id);
            });
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

    fn row(&self, item: &Download, p: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = item.id;
        let selected = self.selected == Some(id);
        let running = item.state == State::Running;
        let detail = if item.host.is_empty() {
            item.status()
        } else {
            format!("{} · {}", item.host, item.status())
        };
        let action = |name: &'static str, label: &'static str| {
            div()
                .id((name, id))
                .h(px(22.))
                .px(px(6.))
                .flex_shrink_0()
                .rounded(px(6.))
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(p.primary))
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(p.background)))
                .child(label)
        };
        let path = item.path.clone();
        let reveal = action("download-reveal", "Show in Finder")
            .on_click(move |_, _, cx| cx.reveal_path(&path));
        let path = item.path.clone();
        let open =
            action("download-open", "Open").on_click(move |_, _, cx| cx.open_with_system(&path));
        let cancel = action("download-cancel", "Cancel")
            .on_click(cx.listener(move |this, _, _, cx| this.cancel(id, cx)));
        let removed = item.clone();
        let remove = action("download-remove", "Remove")
            .on_click(cx.listener(move |this, _, _, cx| this.remove(&removed, cx)));
        div()
            .id(("download", id))
            .h(px(ROW - 4.))
            .flex_shrink_0()
            .px_2()
            .rounded_lg()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(3.))
            .bg(rgb(if selected { p.elevated } else { p.surface }))
            .hover(move |style| style.bg(rgb(p.elevated)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(id);
                cx.notify();
            }))
            .child(
                div()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(item.name()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(detail),
                    )
                    .map(|actions| match item.state {
                        State::Running => actions.child(reveal).child(cancel),
                        State::Done => actions.child(open).child(reveal).child(remove),
                        _ => actions.child(remove),
                    }),
            )
            .when(running, |row| {
                row.child(bar(
                    item.fraction()
                        .map_or(Activity::Indeterminate, Activity::Fraction),
                    p,
                ))
            })
    }
}

impl Render for DownloadPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let items = self.items(cx);
        let finished = items.iter().any(|item| item.state != State::Running);
        let running = items.len()
            - items
                .iter()
                .filter(|item| item.state != State::Running)
                .count();
        let heading: SharedString = match running {
            0 => "Downloads".into(),
            1 => "Downloads · 1 in progress".into(),
            count => format!("Downloads · {count} in progress").into(),
        };
        let rows: Vec<_> = items.iter().map(|item| self.row(item, p, cx)).collect();
        div()
            .id("downloads")
            .key_context("FelinautDownloads")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseDownloads, window, cx| {
                cx.stop_propagation();
                this.close(window, cx, true);
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                if let Some(item) = this.selected(cx).filter(|item| item.state == State::Done) {
                    cx.open_with_system(&item.path);
                }
            }))
            .on_action(cx.listener(|this, _: &RevealSelected, _, cx| {
                if let Some(item) = this.selected(cx) {
                    cx.reveal_path(&item.path);
                }
            }))
            .on_action(cx.listener(|this, _: &RemoveSelected, _, cx| {
                if let Some(item) = this.selected(cx) {
                    // Keep a row selected so the keys carry on down the list.
                    this.step(true, cx);
                    if this.selected == Some(item.id) {
                        this.step(false, cx);
                    }
                    this.remove(&item, cx);
                }
            }))
            .size_full()
            .rounded_xl()
            .overflow_hidden()
            .bg(rgb(p.surface))
            .border_2()
            .border_color(rgb(p.primary))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .text_color(rgb(p.text))
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .child(
                div()
                    .h(px(26.))
                    .flex_shrink_0()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(heading))
                    .when(finished, |header| {
                        header.child(
                            div()
                                .id("downloads-clear")
                                .h(px(24.))
                                .px_2()
                                .rounded(px(6.))
                                .flex()
                                .items_center()
                                .text_size(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(p.muted))
                                .cursor_pointer()
                                .hover(move |style| {
                                    style.bg(rgb(p.elevated)).text_color(rgb(p.text))
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.edit(cx, |downloads| {
                                        downloads.clear_finished();
                                    });
                                }))
                                .child("Clear finished"),
                        )
                    }),
            )
            .child(
                div()
                    .id("download-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .when(rows.is_empty(), |list| {
                        list.child(
                            div()
                                .h(px(ROW - 4.))
                                .px_2()
                                .rounded_lg()
                                .bg(rgb(p.background))
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.))
                                .text_color(rgb(p.muted))
                                .child(brand::symbol().size(px(22.)).opacity(0.8))
                                .child(
                                    "Nothing fetched yet. Files you download land in Downloads.",
                                ),
                        )
                    })
                    .children(rows),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child("↑ ↓  select   ·   ↩  open   ·   ⌘↩  in Finder   ·   ⌫  remove"),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_list_grows_to_five_rows_then_scrolls() {
        use super::{FRAME, ROW, height};
        assert_eq!(height(0), height(1), "room for the empty note");
        assert_eq!(height(3), FRAME + 3. * ROW);
        assert_eq!(height(5), height(500));
    }
}
