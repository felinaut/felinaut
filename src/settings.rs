//! A single, independently closable Settings window (⌘,). The host registers `open`.

use gpui::{
    App, Bounds, Context, FocusHandle, Global, KeyBinding, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, actions, div, prelude::*, px, rgb, size,
};

use crate::{
    engine::SearchEngine,
    language::WebLanguage,
    theme::{self, Palette, ThemeSettings},
};

actions!(settings, [CloseSettings]);

#[derive(Default)]
struct SettingsWindow {
    // GPUI window handles do not keep their window (or root entity) alive.
    handle: Option<WindowHandle<Settings>>,
}
impl Global for SettingsWindow {}

/// Open Settings, or bring its existing window forward.
pub fn open(cx: &mut App) {
    if !cx.has_global::<SettingsWindow>() {
        cx.set_global(SettingsWindow::default());
        cx.bind_keys([
            KeyBinding::new("escape", CloseSettings, Some("FelinautSettings")),
            KeyBinding::new("cmd-w", CloseSettings, Some("FelinautSettings")),
        ]);
    }

    if let Some(handle) = cx.global::<SettingsWindow>().handle {
        if handle
            .update(cx, |settings, window, _| {
                window.activate_window();
                window.focus(&settings.focus);
            })
            .is_ok()
        {
            cx.activate(true);
            return;
        }
        cx.global_mut::<SettingsWindow>().handle = None;
    }

    let bounds = Bounds::centered(None, size(px(460.), px(620.)), cx);
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(400.), px(360.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Settings".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Settings::new(window, cx)),
    ) {
        Ok(handle) => {
            cx.global_mut::<SettingsWindow>().handle = Some(handle);
            cx.activate(true);
        }
        Err(error) => eprintln!("Could not open Settings: {error}"),
    }
}

/// One row per choice in display order, each with whether it is the current choice.
fn choice_rows<T: Copy + PartialEq>(all: &[T], current: T) -> Vec<(T, bool)> {
    all.iter()
        .map(|&choice| (choice, choice == current))
        .collect()
}

/// Make `engine` the search engine for address-bar searches and save it.
fn choose_search(engine: SearchEngine, cx: &mut App) {
    cx.update_global::<ThemeSettings, _>(|settings, _| {
        if settings.search != engine {
            settings.search = engine;
            settings.save();
        }
    });
}

/// Make `language` the website language and save it. Searches use it at once; websites
/// receive it after a restart (see `language::apply`).
fn choose_language(language: WebLanguage, cx: &mut App) {
    cx.update_global::<ThemeSettings, _>(|settings, _| {
        if settings.language != language {
            settings.language = language;
            settings.save();
        }
    });
}

/// The website language Felinaut started with, which is what websites receive until a restart.
pub struct LaunchLanguage(pub WebLanguage);
impl Global for LaunchLanguage {}

/// Whether websites still receive an older choice, so Settings should ask for a restart.
fn needs_restart(launch: WebLanguage, chosen: WebLanguage) -> bool {
    launch != chosen
}

struct Settings {
    focus: FocusHandle,
    _theme_observer: Subscription,
}

impl Settings {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        // Redraw for theme changes and for choices made elsewhere.
        let theme_observer = cx.observe_global::<ThemeSettings>(|_, cx| cx.notify());
        Self {
            focus,
            _theme_observer: theme_observer,
        }
    }

    fn close(&mut self, _: &CloseSettings, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.remove_window();
    }
}

fn section(p: Palette, title: &'static str, hint: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .p_3()
        .gap_1()
        .rounded_xl()
        .bg(rgb(p.surface))
        .border_1()
        .border_color(rgb(p.border))
        .child(
            div()
                .h(px(22.))
                .px_2()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(p.muted))
                .child(title),
        )
        .child(
            div()
                .px_2()
                .pb_1()
                .text_size(px(12.))
                .text_color(rgb(p.muted))
                .child(hint),
        )
}

fn choice_row(
    p: Palette,
    id: &'static str,
    index: usize,
    label: &'static str,
    active: bool,
    choose: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    div()
        .id((id, index))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(30.))
        .rounded_lg()
        .cursor_pointer()
        .when(active, |row| row.bg(rgb(p.elevated)))
        .hover(move |style| style.bg(rgb(p.elevated)))
        .on_click(move |_, _, cx| choose(cx))
        .child(
            div()
                .size(px(16.))
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
        .child(label)
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let settings = cx.global::<ThemeSettings>();
        let (search, language) = (settings.search, settings.language);
        let launch = cx
            .try_global::<LaunchLanguage>()
            .map_or(language, |launch| launch.0);
        div()
            .id("felinaut-settings")
            .key_context("FelinautSettings")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close))
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_6()
            .bg(rgb(p.background))
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Settings"),
            )
            .child(
                section(
                    p,
                    "SEARCH ENGINE",
                    "Used when you type a search in the address bar.",
                )
                .children(
                    choice_rows(&SearchEngine::ALL, search)
                        .into_iter()
                        .enumerate()
                        .map(|(index, (engine, active))| {
                            choice_row(p, "search-engine", index, engine.name(), active, move |cx| {
                                choose_search(engine, cx)
                            })
                        }),
                ),
            )
            .child(
                section(
                    p,
                    "WEBSITE LANGUAGE",
                    "The language Felinaut asks websites and searches for.",
                )
                .children(
                    choice_rows(&WebLanguage::ALL, language)
                        .into_iter()
                        .enumerate()
                        .map(|(index, (choice, active))| {
                            choice_row(p, "web-language", index, choice.name(), active, move |cx| {
                                choose_language(choice, cx)
                            })
                        }),
                )
                .when(needs_restart(launch, language), |section| {
                    section.child(
                        div()
                            .px_2()
                            .pt_1()
                            .text_size(px(12.))
                            .text_color(rgb(p.accent))
                            .child(
                                "Searches use it now. Restart Felinaut to apply it to other websites.",
                            ),
                    )
                }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_rows_list_every_engine_once_and_mark_only_the_current_one() {
        for current in SearchEngine::ALL {
            let rows = choice_rows(&SearchEngine::ALL, current);
            assert_eq!(
                rows.iter().map(|(engine, _)| *engine).collect::<Vec<_>>(),
                SearchEngine::ALL.to_vec()
            );
            assert_eq!(
                rows.iter()
                    .filter(|(_, active)| *active)
                    .map(|(engine, _)| *engine)
                    .collect::<Vec<_>>(),
                vec![current]
            );
        }
    }

    #[test]
    fn search_engine_names_are_distinct_and_readable() {
        let mut names: Vec<_> = SearchEngine::ALL.iter().map(|e| e.name()).collect();
        assert!(names.iter().all(|name| !name.trim().is_empty()));
        assert_eq!(SearchEngine::DuckDuckGo.name(), "DuckDuckGo");
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SearchEngine::ALL.len());
    }

    #[test]
    fn language_rows_mark_only_the_current_choice() {
        for current in WebLanguage::ALL {
            let rows = choice_rows(&WebLanguage::ALL, current);
            assert_eq!(rows.len(), WebLanguage::ALL.len());
            assert_eq!(
                rows.iter().filter(|(_, active)| *active).count(),
                1,
                "{current:?}"
            );
            assert!(rows.contains(&(current, true)));
        }
        assert!(choice_rows::<WebLanguage>(&[], WebLanguage::System).is_empty());
    }

    #[test]
    fn restart_is_asked_only_when_websites_still_get_another_language() {
        for launch in WebLanguage::ALL {
            for chosen in WebLanguage::ALL {
                assert_eq!(needs_restart(launch, chosen), launch != chosen);
            }
        }
    }
}
