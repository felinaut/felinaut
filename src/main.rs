mod about;
mod appearance;
mod art;
mod audio;
mod brand;
mod engine;
mod input;
mod theme;
mod volume;
mod webview;

use audio::{AudioReport, PageSound, TabAudio};
use gpui::{prelude::*, *};
use input::{AddressInput, Submit};
use std::{
    rc::Rc,
    time::{Duration, Instant},
};
use theme::{Palette, ThemeSettings};
use webview::{Event, NativePage};

const HOME: &str = "felifox://newtab";
pub const DISPLAY_VERSION: &str = "0.1 beta";

actions!(
    felifox,
    [
        NewTab,
        CloseTab,
        FocusAddress,
        Back,
        Forward,
        Reload,
        Home,
        Quit,
        AboutFelifox,
        VolumeControls,
        MuteTab
    ]
);

struct Tab {
    id: u64,
    title: String,
    url: String,
    home: bool,
    loading: bool,
    error: Option<String>,
    hovered_link: Option<String>,
    page: Option<Rc<NativePage>>,
    /// The user's settings for this tab; they persist across its navigations.
    audio: TabAudio,
    /// What the current document reports about its playback.
    sound: PageSound,
}
impl Tab {
    fn new(id: u64) -> Self {
        Self {
            id,
            title: "New Tab".into(),
            url: HOME.into(),
            home: true,
            loading: false,
            error: None,
            hovered_link: None,
            page: None,
            audio: TabAudio::default(),
            sound: PageSound::default(),
        }
    }
    fn display_title(&self) -> String {
        if self.home {
            "New Tab".into()
        } else {
            self.title.clone()
        }
    }
    fn sound(&self, active: bool) -> volume::TabSound {
        volume::TabSound {
            id: self.id,
            title: self.display_title().into(),
            audio: self.audio,
            audible: self.sound.audible(),
            limited: self.sound.limited(),
            active,
        }
    }
    fn apply_audio(&self) {
        if let Some(page) = &self.page
            && let Err(error) = page.apply_audio(self.audio)
        {
            eprintln!("Could not apply tab audio: {error}");
        }
    }
    fn url(&self) -> &str {
        if self.home { HOME } else { &self.url }
    }
    fn can_go_back(&self) -> bool {
        !self.home && self.page.is_some()
    }
    fn can_go_forward(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| self.home || page.can_go_forward())
    }
}

struct Browser {
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    address: Entity<AddressInput>,
    focus: FocusHandle,
    _submit: Subscription,
    _theme_observer: Subscription,
    art_phase: f32,
}
impl Browser {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme_observer = cx.observe_global::<ThemeSettings>(|_, cx| cx.notify());
        let address = cx.new(AddressInput::new);
        cx.spawn_in(window, async move |this, cx| {
            let mut previous = Instant::now();
            let mut was_running = false;
            let mut delay = Duration::from_millis(200);
            loop {
                Timer::after(delay).await;
                let now = Instant::now();
                let elapsed = now.duration_since(previous).as_secs_f32();
                previous = now;
                let result = this.update_in(cx, |this, window, cx| {
                    let running = this.tabs[this.active].home
                        && window.is_window_active()
                        && theme::motion_allowed(cx);
                    if running && was_running {
                        this.art_phase = (this.art_phase + elapsed) % 3600.;
                        cx.notify();
                    }
                    running
                });
                let Ok(running) = result else {
                    break;
                };
                was_running = running;
                delay = Duration::from_millis(if running { 33 } else { 200 });
            }
        })
        .detach();
        let subscription =
            cx.subscribe_in(&address, window, |this, _, event: &Submit, window, cx| {
                this.navigate(&event.0, window, cx);
                let tab = &this.tabs[this.active];
                if !tab.home
                    && tab.error.is_none()
                    && let Some(page) = &tab.page
                {
                    window.focus(&this.focus);
                    let _ = page.focus();
                }
            });
        // Native callbacks enqueue events, avoiding reentrant updates while GPUI renders.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(100)).await;
                if this
                    .update_in(cx, |this, window, cx| this.poll_pages(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |this, _| {
                for tab in &mut this.tabs {
                    tab.page.take();
                }
            });
            true
        });
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            tabs: vec![Tab::new(1)],
            active: 0,
            next_id: 2,
            address,
            focus,
            _submit: subscription,
            _theme_observer: theme_observer,
            art_phase: 0.,
        }
    }
    fn sync_address(&self, cx: &mut Context<Self>) {
        let url = self.tabs[self.active].url();
        let text = if url == HOME {
            String::new()
        } else {
            url.into()
        };
        self.address
            .update(cx, |input, cx| input.set_text(text, cx));
    }
    fn poll_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut address_changed = false;
        let mut open = Vec::new();
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(page) = &tab.page else {
                continue;
            };
            for event in page.drain_events() {
                changed = true;
                match event {
                    Event::Started(url) => {
                        tab.sound.clear();
                        tab.hovered_link = None;
                        tab.loading = true;
                        tab.error = None;
                        tab.url = url;
                    }
                    Event::Finished(url) => {
                        tab.loading = false;
                        tab.url = url;
                        if !tab.audio.is_default() {
                            tab.apply_audio();
                        }
                    }
                    Event::Title(title) => {
                        tab.title = title;
                    }
                    Event::Url(url) => {
                        tab.url = url;
                        address_changed |= index == self.active;
                    }
                    Event::Open(url) => open.push((url, true)),
                    Event::OpenInBackground(url) => open.push((url, false)),
                    Event::Hover(url) => tab.hovered_link = url,
                    // A new document asks for the tab's settings; default ones need no script.
                    Event::Audio(AudioReport::Hello) => {
                        if !tab.audio.is_default() {
                            tab.apply_audio();
                        }
                    }
                    Event::Audio(AudioReport::Frame {
                        id,
                        audible,
                        limited,
                    }) => tab.sound.report(&id, audible, limited),
                }
            }
        }
        if address_changed && !self.address.read(cx).focus_handle(cx).is_focused(window) {
            self.sync_address(cx);
        }
        for (url, activate) in open {
            self.open_link_tab(&url, activate, window, cx);
        }
        if changed {
            cx.notify();
        }
    }
    fn navigate(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = match engine::resolve_input(input) {
            Ok(url) => url,
            Err(error) => {
                self.tabs[self.active].error = Some(error);
                cx.notify();
                return;
            }
        };
        if url == HOME {
            self.home(&Home, window, cx);
            return;
        }
        let tab = &mut self.tabs[self.active];
        tab.hovered_link = None;
        let result = if let Some(page) = &tab.page {
            page.navigate(&url)
        } else {
            NativePage::new(window, &url).map(|page| {
                tab.page = Some(Rc::new(page));
            })
        };
        match result {
            Ok(()) => {
                tab.url = url;
                tab.home = false;
                tab.loading = true;
                tab.error = None;
                tab.title = url::Url::parse(&tab.url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_owned))
                    .unwrap_or_else(|| "Loading…".into());
            }
            Err(error) => tab.error = Some(error),
        }
        self.sync_address(cx);
        cx.notify();
    }
    fn open_link_tab(
        &mut self,
        url: &str,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match NativePage::new(window, url) {
            Ok(page) => {
                let page = Rc::new(page);
                let mut tab = Tab::new(self.next_id);
                self.next_id += 1;
                tab.url = url.into();
                tab.home = false;
                tab.loading = true;
                tab.title = url::Url::parse(url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_owned))
                    .unwrap_or_else(|| "Loading…".into());
                tab.page = Some(page.clone());
                self.tabs.push(tab);
                if activate {
                    self.select_tab(self.tabs.len() - 1, window, cx);
                    window.on_next_frame(move |_, _| {
                        let _ = page.focus();
                    });
                }
            }
            Err(error) => self.tabs[self.active].error = Some(error),
        }
        cx.notify();
    }
    /// Called by the volume popover and the tab strip.
    fn set_audio(&mut self, id: u64, audio: TabAudio, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id)
            && tab.audio != audio
        {
            tab.audio = audio;
            tab.apply_audio();
            cx.notify();
        }
    }
    fn toggle_mute(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) {
            let audio = TabAudio {
                muted: !tab.audio.muted,
                ..tab.audio
            };
            self.set_audio(id, audio, cx);
        }
    }
    fn mute_tab(&mut self, _: &MuteTab, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mute(self.tabs[self.active].id, cx);
    }
    fn volume_controls(&mut self, _: &VolumeControls, window: &mut Window, cx: &mut Context<Self>) {
        volume::toggle(cx.weak_entity(), None, window, cx);
    }
    fn activate_tab_id(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            self.select_tab(index, window, cx);
        }
    }
    fn sound_overview(&self) -> volume::Overview {
        let mut tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.sound.audible() || !tab.audio.is_default())
            .map(|(index, tab)| tab.sound(index == self.active))
            .collect();
        tabs.sort_by_key(|tab| (!tab.audible, !tab.active));
        volume::Overview {
            current: self.tabs[self.active].sound(true),
            tabs,
        }
    }
    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs[self.active].hovered_link = None;
        if let Some(page) = &self.tabs[self.active].page {
            let _ = page.set_visible(false);
            let _ = page.focus_parent();
        }
        self.active = index;
        self.tabs[index].hovered_link = None;
        window.focus(&self.focus);
        self.sync_address(cx);
        cx.notify();
    }
    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(Tab::new(self.next_id));
        self.next_id += 1;
        self.select_tab(self.tabs.len() - 1, window, cx);
        self.focus_address(&FocusAddress, window, cx);
    }
    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, window, cx);
    }
    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index == self.active {
            if let Some(page) = &self.tabs[index].page {
                let _ = page.focus_parent();
            }
            window.focus(&self.focus);
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(self.next_id));
            self.next_id += 1;
        }
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.sync_address(cx);
        cx.notify();
    }
    fn focus_address(&mut self, _: &FocusAddress, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = &self.tabs[self.active].page {
            let _ = page.focus_parent();
        }
        self.address.update(cx, |input, cx| {
            window.focus(&input.focus_handle(cx));
            input.select_all_text(cx);
        });
    }
    fn back(&mut self, _: &Back, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if tab.home {
            return;
        }
        if let Some(page) = &tab.page {
            if page.can_go_back() {
                tab.error = page.back().err();
            } else {
                self.home(&Home, window, cx);
                return;
            }
        }
        cx.notify();
    }
    fn forward(&mut self, _: &Forward, _: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if let Some(page) = &tab.page {
            if tab.home {
                tab.home = false;
                tab.error = None;
            } else if page.can_go_forward() {
                tab.error = page.forward().err();
            }
        }
        self.sync_address(cx);
        cx.notify();
    }
    fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if !tab.home
            && let Some(page) = &tab.page
        {
            tab.error = page.reload().err();
        }
        cx.notify();
    }
    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if let Some(page) = &tab.page {
            let _ = page.set_visible(false);
            let _ = page.focus_parent();
        }
        tab.home = true;
        tab.error = None;
        tab.hovered_link = None;
        window.focus(&self.focus);
        self.sync_address(cx);
        cx.notify();
    }

    fn button(&self, id: &'static str, label: &'static str, p: Palette) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(34.))
            .min_w(px(34.))
            .px_2()
            .rounded_lg()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(p.muted))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(p.elevated)).text_color(rgb(p.text)))
            .child(label)
    }
    fn volume_button(&self, p: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let tab = &self.tabs[self.active];
        let audio = tab.audio;
        let color = if audio.muted {
            p.muted
        } else if audio.boosted() || tab.sound.audible() {
            p.primary
        } else {
            p.muted
        };
        div()
            .id("volume")
            .h(px(34.))
            .min_w(px(34.))
            .px_2()
            .gap_1()
            .rounded_lg()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(color))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .when(!audio.is_default(), |button| button.bg(rgb(p.elevated)))
            .hover(move |s| s.bg(rgb(p.elevated)).text_color(rgb(p.text)))
            .on_click(cx.listener(|_, event: &ClickEvent, window, cx| {
                let anchor = (!event.is_keyboard()).then(|| event.position().x);
                volume::toggle(cx.weak_entity(), anchor, window, cx);
            }))
            .child(
                brand::icon(volume::speaker_icon(audio))
                    .size(px(18.))
                    .text_color(rgb(color)),
            )
            .when_some(audio.badge(), |button, badge| button.child(badge))
    }
    fn home_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = theme::palette(cx);
        let phase = if theme::motion_allowed(cx) {
            self.art_phase
        } else {
            0.
        };
        let links = [
            (
                "W",
                "Wikipedia",
                "https://en.wikipedia.org/wiki/Main_Page",
                p.highlight,
            ),
            ("R", "Rust", "https://www.rust-lang.org", p.accent),
            (
                "Y",
                "Hacker News",
                "https://news.ycombinator.com",
                p.primary,
            ),
            ("G", "GitHub", "https://github.com", p.secondary),
        ];
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(p.background))
            .child(art::backdrop(p, phase).absolute().inset_0())
            .child(
                div()
                    .id("home-content")
                    .relative()
                    .size_full()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .min_h_full()
                            .w_full()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .py_5()
                            .px_6()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(p.muted))
                                    .child("SMALL BROWSER. BIG CURIOSITY."),
                            )
                            .child(
                                brand::mascot()
                                    .w(px(280.))
                                    .h(px(210.))
                                    .flex_shrink_0()
                                    .relative()
                                    .top(px((phase * 1.2).sin() * 6.)),
                            )
                            .child(
                                div()
                                    .font_family("Georgia")
                                    .italic()
                                    .text_size(px(48.))
                                    .line_height(px(52.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(p.text))
                                    .child("Felifox"),
                            )
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .text_color(rgb(p.muted))
                                    .child("Follow the thread. Find something wonderful."),
                            )
                            .child(
                                div()
                                    .id("home-search")
                                    .w(px(520.))
                                    .max_w_full()
                                    .h(px(48.))
                                    .flex_shrink_0()
                                    .px_5()
                                    .rounded_2xl()
                                    .bg(rgb(p.surface))
                                    .border_1()
                                    .border_color(rgb(p.border))
                                    .cursor_text()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .text_color(rgb(p.muted))
                                    .hover(move |s| s.border_color(rgb(p.primary)))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.focus_address(&FocusAddress, window, cx)
                                    }))
                                    .child("Search the web or enter an address")
                                    .child(div().text_xs().child("⌘ L")),
                            )
                            .child(div().flex().gap_3().mt_1().children(links.into_iter().map(
                                |(icon, name, url, color)| {
                                    div()
                                        .id(name)
                                        .w(px(120.))
                                        .h(px(84.))
                                        .flex_shrink_0()
                                        .rounded_xl()
                                        .bg(rgb(p.surface))
                                        .border_1()
                                        .border_color(rgb(p.border))
                                        .cursor_pointer()
                                        .hover(move |s| {
                                            s.bg(rgb(p.elevated)).border_color(rgb(color))
                                        })
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .gap_2()
                                        .on_hover(cx.listener(
                                            move |this, hovered: &bool, _, cx| {
                                                this.tabs[this.active].hovered_link =
                                                    hovered.then(|| url.to_owned());
                                                cx.notify();
                                            },
                                        ))
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, window, cx| {
                                                let modifiers = event.modifiers();
                                                if modifiers.platform {
                                                    this.open_link_tab(
                                                        url,
                                                        modifiers.shift,
                                                        window,
                                                        cx,
                                                    );
                                                } else {
                                                    this.navigate(url, window, cx);
                                                }
                                            },
                                        ))
                                        .child(
                                            div()
                                                .size(px(30.))
                                                .rounded_lg()
                                                .bg(rgb(p.elevated))
                                                .text_color(rgb(p.text))
                                                .border_1()
                                                .border_color(rgb(color))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .font_weight(FontWeight::BOLD)
                                                .child(icon),
                                        )
                                        .child(div().text_xs().text_color(rgb(p.text)).child(name))
                                },
                            )))
                            .child(
                                div()
                                    .mt_2()
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .text_xs()
                                    .text_color(rgb(p.muted))
                                    .child("⌘ T  New tab   ·   ⌘ L  Search")
                                    .child(
                                        div()
                                            .id("home-about")
                                            .cursor_pointer()
                                            .text_color(rgb(p.primary))
                                            .on_click(|_, _, cx| about::open(cx))
                                            .child(format!("About Felifox · {DISPLAY_VERSION}")),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
    fn content(&self, cx: &mut Context<Self>) -> AnyElement {
        let tab = &self.tabs[self.active];
        if tab.home {
            return self.home_page(cx);
        }
        if let Some(page) = &tab.page {
            let page = page.clone();
            return canvas(
                move |bounds, _, _| {
                    if let Err(error) = page.set_bounds(bounds).and_then(|_| page.set_visible(true))
                    {
                        eprintln!("Could not position native webview: {error}");
                    }
                },
                |_, _, _, _| {},
            )
            .size_full()
            .into_any_element();
        }
        div().size_full().into_any_element()
    }
}

impl Render for Browser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let tab = &self.tabs[self.active];
        let back_enabled = tab.can_go_back();
        let forward_enabled = tab.can_go_forward();
        let status = tab
            .hovered_link
            .as_deref()
            .or(tab.error.as_deref())
            .unwrap_or(if tab.loading && !tab.home {
                "Loading · WebKit"
            } else {
                "WebKit · JavaScript enabled"
            });
        div()
            .id("felifox")
            .track_focus(&self.focus)
            .key_context("Browser")
            .size_full()
            .bg(rgb(p.background))
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::focus_address))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::forward))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::mute_tab))
            .on_action(cx.listener(Self::volume_controls))
            .child(
                div()
                    .h(px(52.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl(px(88.))
                    .pr_4()
                    .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                        let selected = self.active == index;
                        div()
                            .id(("tab", tab.id))
                            .h(px(34.))
                            .w(px(195.))
                            .min_w(px(70.))
                            .px_3()
                            .rounded_lg()
                            .bg(rgb(if selected { p.surface } else { p.background }))
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.elevated)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_tab(index, window, cx);
                            }))
                            .child(
                                div()
                                    .text_color(rgb(if selected { p.primary } else { p.muted }))
                                    .child(if tab.loading && !tab.home {
                                        div().child("◌").into_any_element()
                                    } else {
                                        brand::symbol().size(px(18.)).into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_sm()
                                    .child(tab.display_title()),
                            )
                            .when(tab.sound.audible() || tab.audio.muted, |row| {
                                let id = tab.id;
                                let muted = tab.audio.muted;
                                row.child(
                                    div()
                                        .id(("tab-sound", id))
                                        .p_1()
                                        .rounded_md()
                                        .hover(move |s| s.bg(rgb(p.elevated)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_mute(id, cx);
                                        }))
                                        .child(
                                            brand::icon(volume::speaker_icon(tab.audio))
                                                .size(px(14.))
                                                .text_color(rgb(if muted {
                                                    p.muted
                                                } else {
                                                    p.primary
                                                })),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id(("close", tab.id))
                                    .text_color(rgb(p.muted))
                                    .px_1()
                                    .rounded_md()
                                    .hover(move |s| {
                                        s.bg(rgb(p.elevated)).text_color(rgb(p.primary))
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_tab_at(index, window, cx);
                                    }))
                                    .child("×"),
                            )
                    }))
                    .child(self.button("new-tab", "+", p).on_click(
                        cx.listener(|this, _, window, cx| this.new_tab(&NewTab, window, cx)),
                    )),
            )
            .child(
                div()
                    .h(px(64.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px_4()
                    .gap_2()
                    .bg(rgb(p.surface))
                    .child(
                        self.button("back", "←", p)
                            .opacity(if back_enabled { 1. } else { 0.35 })
                            .on_click(
                                cx.listener(|this, _, window, cx| this.back(&Back, window, cx)),
                            ),
                    )
                    .child(
                        self.button("forward", "→", p)
                            .opacity(if forward_enabled { 1. } else { 0.35 })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.forward(&Forward, window, cx)
                            })),
                    )
                    .child(self.button("reload", "↻", p).on_click(
                        cx.listener(|this, _, window, cx| this.reload(&Reload, window, cx)),
                    ))
                    .child(
                        self.button("home", "⌂", p).on_click(
                            cx.listener(|this, _, window, cx| this.home(&Home, window, cx)),
                        ),
                    )
                    .child(div().flex_1().min_w_0().child(self.address.clone()))
                    .child(self.volume_button(p, cx))
                    .child(
                        self.button("appearance", "◈ Theme ▾", p)
                            .text_size(px(12.))
                            .on_click(|_, window, cx| appearance::open(window, cx)),
                    )
                    .child(
                        self.button("about", "ⓘ", p)
                            .on_click(|_, _, cx| about::open(cx)),
                    ),
            )
            .child(div().h(px(2.)).flex_shrink_0().bg(rgb(if tab.loading {
                p.primary
            } else {
                p.border
            })))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.content(cx)),
            )
            .child(
                div()
                    .h(px(30.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(status.to_owned()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .child(format!("FELIFOX  /  {DISPLAY_VERSION}")),
                    ),
            )
    }
}
fn main() {
    Application::new()
        .with_assets(brand::BrandAssets)
        .run(|cx: &mut App| {
            cx.set_global(ThemeSettings {
                theme: theme::Theme::default(),
                animate: true,
            });
            input::init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-t", NewTab, None),
                KeyBinding::new("cmd-w", CloseTab, None),
                KeyBinding::new("cmd-l", FocusAddress, None),
                KeyBinding::new("cmd-r", Reload, None),
                KeyBinding::new("cmd-[", Back, None),
                KeyBinding::new("cmd-]", Forward, None),
                KeyBinding::new("cmd-shift-h", Home, None),
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-shift-m", MuteTab, None),
            ]);
            // Native menu equivalents remain available while WKWebView owns keyboard focus.
            cx.set_menus(vec![
                Menu {
                    name: "Felifox".into(),
                    items: vec![
                        MenuItem::action("About Felifox", AboutFelifox),
                        MenuItem::separator(),
                        MenuItem::action("Quit Felifox", Quit),
                    ],
                },
                Menu {
                    name: "File".into(),
                    items: vec![
                        MenuItem::action("New Tab", NewTab),
                        MenuItem::action("Close Tab", CloseTab),
                    ],
                },
                Menu {
                    name: "Edit".into(),
                    items: vec![
                        MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                        MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                        MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                        MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
                    ],
                },
                Menu {
                    name: "Navigate".into(),
                    items: vec![
                        MenuItem::action("Open Location", FocusAddress),
                        MenuItem::action("Back", Back),
                        MenuItem::action("Forward", Forward),
                        MenuItem::action("Reload", Reload),
                        MenuItem::action("Home", Home),
                    ],
                },
                Menu {
                    name: "Sound".into(),
                    items: vec![
                        MenuItem::action("Volume Controls…", VolumeControls),
                        MenuItem::action("Mute / Unmute Tab", MuteTab),
                    ],
                },
            ]);
            cx.on_action(|_: &AboutFelifox, cx| about::open(cx));
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1280.), px(900.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1000.), px(700.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Felifox".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(16.), px(21.))),
                    }),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Browser::new(window, cx)),
            )
            .expect("Could not open Felifox window");
            cx.activate(true);
        });
}
