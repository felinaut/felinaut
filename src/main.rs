mod about;
mod agent;
mod appearance;
mod art;
mod audio;
mod brand;
mod download_panel;
mod downloads;
mod engine;
mod history;
mod icons;
mod input;
mod live;
mod load_error;
mod session;
mod suggestions;
mod theme;
mod volume;
mod webview;
mod zoom;

use audio::{AudioReport, PageSound, TabAudio};
use gpui::{prelude::*, *};
use history::{History, Suggestion};
use input::{AddressEvent, AddressInput, Submit};
use load_error::LoadFailure;
use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use theme::{AddressDisplay, Palette, ThemeSettings};
use webview::{Event, NativePage};

const HOME: &str = "felinaut://newtab";
const SIDEBAR_SLIDE: Duration = Duration::from_millis(200);
/// AppKit only hovers and clicks the traffic lights inside its title bar view (32 pt on this
/// macOS), so the 14 pt buttons must start at most 18 pt down or they go half dead.
/// shortcut: 28 pt title bars (older macOS) still clip 4 pt; measure it at runtime if testers
/// report it.
const TRAFFIC_LIGHT_Y: f32 = 18.;
/// The sidebar header and toolbar height, centring them on the traffic lights.
const CHROME_ROW: f32 = 2. * (TRAFFIC_LIGHT_Y + 7.);
/// The pointer within this many px of the window's left edge peeks a hidden sidebar…
const PEEK_EDGE: f32 = 6.;
/// …which stays until the pointer is this far past its right edge.
const PEEK_MARGIN: f32 = 24.;

/// A tab being dragged within the sidebar, by id.
struct DraggedTab(u64);
/// The sidebar's right edge being dragged to resize it.
struct SidebarResize;
/// A faint copy of the dragged tab that follows the pointer, Arc-style.
struct Ghost {
    title: SharedString,
    icon: Option<Arc<Image>>,
    size: Size<Pixels>,
    p: Palette,
}
/// What follows the pointer while dragging: a tab ghost, or nothing for a resize.
struct DragPreview(Option<Ghost>);
impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().when_some(self.0.as_ref(), |preview, ghost| {
            let p = ghost.p;
            preview
                .w(ghost.size.width)
                .h(ghost.size.height)
                .px_2()
                .rounded_lg()
                .bg(rgb(p.elevated))
                .opacity(0.7)
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(rgb(p.text))
                .font_family(".AppleSystemUIFont")
                .when_some(ghost.icon.clone(), |row, icon| {
                    row.child(img(icon).size(px(16.)).rounded_sm())
                })
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(ghost.title.clone()),
                )
        })
    }
}

/// A tab's right-click menu: which tab, and where the click was (window coordinates).
#[derive(Clone, Copy)]
struct TabMenu {
    id: u64,
    at: Point<Pixels>,
}

/// The one live folder refresh in flight: a hidden page, and the extractor's last answer.
struct Fetch {
    folder: usize,
    section: usize,
    page: NativePage,
    started: Instant,
    loaded: bool,

    next_try: Instant,
    result: Arc<Mutex<Option<String>>>,
}
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Poll through loading/navigation up to the deadline, not a smaller attempt budget.
fn live_refresh_can_poll(started: Instant, next_try: Instant, now: Instant) -> bool {
    now.duration_since(started) < FETCH_TIMEOUT && now >= next_try
}

/// The system font with tabular figures, so counts that change keep their width.
fn tabular_font(weight: FontWeight) -> Font {
    Font {
        family: ".AppleSystemUIFont".into(),
        features: FontFeatures(Arc::new(vec![("tnum".into(), 1)])),
        fallbacks: None,
        weight,
        style: FontStyle::Normal,
    }
}

/// One right-click menu entry: its label and, when it has one, the shortcut that does the same.
fn menu_item(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    shortcut: Option<&'static str>,
    p: Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .px_2()
        // Concentric with the menu: 8 pt corners, 4 pt padding.
        .rounded(px(4.))
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .cursor_pointer()
        .hover(move |s| s.bg(rgb(p.elevated)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label.into()),
        )
        .when_some(shortcut, |row, keys| {
            row.child(div().text_xs().text_color(rgb(p.muted)).child(keys))
        })
}

fn menu_separator(p: Palette) -> Div {
    div().h(px(1.)).mx_1().my_1().bg(rgb(p.border))
}

/// Where a tab dragged over a sidebar row will go: a line above or below the row, or into a
/// split view with the row's tab. Indices are into `Browser::tabs`, before the drop.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DropHint {
    /// Insert before the row whose first tab is at this index.
    Before(usize),
    /// Insert after the row whose last tab is at this index.
    After(usize),
    /// Show side by side with the tab at this index.
    Split(usize),
}

/// Where a dragged tab lands once removed from the list. `at` is the index it should precede
/// before removal (`usize::MAX` for the end); the result is clamped into the tab's group so
/// pinned tabs stay first.
fn drop_index(at: usize, from: usize, pin: bool, pinned: usize, len: usize) -> usize {
    let at = if from < at { at - 1 } else { at };
    if pin {
        at.min(pinned)
    } else {
        at.clamp(pinned, len)
    }
}

/// The Arc-style insertion line above (`top`) or below a sidebar row, with a ring at its start.
/// Over a tile, the pin grid or the New tab row, a row's line or split preview no longer
/// applies; those targets show their own outline instead.
fn clear_drop_hint(
    this: &mut Browser,
    event: &DragMoveEvent<DraggedTab>,
    _: &mut Window,
    cx: &mut Context<Browser>,
) {
    if event.bounds.contains(&event.event.position) && this.drop_hint.take().is_some() {
        cx.notify();
    }
}

fn drop_line(top: bool, p: Palette) -> Div {
    div()
        .absolute()
        .left(px(4.))
        .right_0()
        .when(top, |line| line.top(px(-3.)))
        .when(!top, |line| line.bottom(px(-3.)))
        .h(px(2.))
        .rounded_full()
        .bg(rgb(p.text))
        .child(
            div()
                .absolute()
                .left(px(-6.))
                .top(px(-3.))
                .size(px(8.))
                .rounded_full()
                .border_2()
                .border_color(rgb(p.text))
                .bg(rgb(p.background)),
        )
}

/// Rebuilds the sidebar from saved tabs: pinned first, split pairs re-linked only when both
/// halves are unpinned neighbours, and the saved active tab selected, or a fresh New Tab when
/// the active one was blank (blank tabs are not saved).
fn restore_tabs(mut saved: Vec<session::SavedTab>) -> (Vec<Tab>, usize) {
    saved.sort_by_key(|tab| !tab.pinned);
    let mut tabs: Vec<Tab> = Vec::new();
    let mut active = None;
    for (id, entry) in (1..).zip(saved) {
        let mut tab = Tab::pinned(id, entry.url, entry.title);
        if !entry.pinned {
            tab.pinned = None;
            if entry.split
                && let Some(left) = tabs.last_mut()
                && left.pinned.is_none()
                && left.split.is_none()
            {
                left.split = Some(id);
                tab.split = Some(left.id);
            }
        }
        if entry.active {
            active = Some(tabs.len());
        }
        tabs.push(tab);
    }
    let active = active.unwrap_or_else(|| {
        let at = tabs.iter().take_while(|tab| tab.pinned.is_some()).count();
        tabs.insert(at, Tab::new(tabs.len() as u64 + 1));
        at
    });
    (tabs, active)
}

/// The host of a web URL, which keys site icons and labels the title chip.
pub fn host(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
}
pub const DISPLAY_VERSION: &str = "0.1 beta";

actions!(
    felinaut,
    [
        NewTab,
        CloseTab,
        FocusAddress,
        Back,
        Forward,
        Reload,
        Home,
        Quit,
        AboutFelinaut,
        VolumeControls,
        MuteTab,
        ToggleSidebar,
        PinTab,
        NextTab,
        PreviousTab,
        ReopenTab,
        NewLiveFolder,
        ShowDownloads
    ]
);

/// What ⌘⇧T needs to bring a closed tab back where it was.
struct ClosedTab {
    url: String,
    title: String,
    index: usize,
    partner: Option<u64>,
}

/// ⌘1–⌘8 select that tab in sidebar order; `LAST_TAB` (⌘9) selects the last one.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = felinaut, no_json)]
struct ActivateTab(usize);
/// Steps the active page's zoom: `1` in, `-1` out, `0` back to 100 %.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = felinaut, no_json)]
struct Zoom(i8);
const LAST_TAB: usize = usize::MAX;

struct Tab {
    id: u64,
    title: String,
    url: String,
    home: bool,
    loading: bool,
    error: Option<String>,
    load_failure: Option<LoadFailure>,
    hovered_link: Option<String>,
    page: Option<Rc<NativePage>>,
    /// The user's settings for this tab; they persist across its navigations.
    audio: TabAudio,
    /// What the current document reports about its playback.
    sound: PageSound,
    /// The last URL this tab added to history, so reloads and the URL poll don't count twice.
    recorded: String,
    /// The URL a pinned tab reopens on launch. Pinned tabs come first in `Browser::tabs`.
    pinned: Option<String>,
    /// The tab shown beside this one in split view. Partners are unpinned, adjacent in
    /// `Browser::tabs` and point at each other.
    split: Option<u64>,
    /// The live folder item this tab shows; such tabs are drawn in their folder, not the rows.
    live: Option<String>,
}
impl Tab {
    /// A saved tab (pinned unless the caller clears `pinned`); its page is created when it is
    /// first shown.
    fn pinned(id: u64, url: String, title: String) -> Self {
        Self {
            title: if title.is_empty() { url.clone() } else { title },
            url: url.clone(),
            home: false,
            pinned: Some(url),
            ..Self::new(id)
        }
    }
    fn new(id: u64) -> Self {
        Self {
            id,
            title: "New Tab".into(),
            url: HOME.into(),
            home: true,
            loading: false,
            error: None,
            load_failure: None,
            hovered_link: None,
            page: None,
            audio: TabAudio::default(),
            sound: PageSound::default(),
            recorded: String::new(),
            pinned: None,
            split: None,
            live: None,
        }
    }
    /// An earlier failure stays until the new page commits or fails, so the page under the
    /// error page never flashes.
    fn start_load(&mut self, url: String) {
        self.title = url::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| "Loading…".into());
        self.url = url;
        self.home = false;
        self.loading = true;
        self.error = None;
    }
    /// Shows a failed load's error page. Try Again needs a URL, so a failure without one
    /// keeps the URL the tab was loading.
    fn show_failure(&mut self, mut failure: LoadFailure) {
        if failure.url.is_empty() {
            failure.url.clone_from(&self.url);
        }
        self.loading = false;
        self.url.clone_from(&failure.url);
        self.title = failure.kind.text().0.into();
        self.load_failure = Some(failure);
    }
    /// Adds the current page to history once per URL per tab.
    fn record_visit(&mut self, history: &mut History) {
        if self.recorded != self.url && history.record(&self.url, &self.title, history::now()) {
            self.recorded.clone_from(&self.url);
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
    zoom_flash: Option<Instant>,
    history: History,
    last_save: Instant,
    /// History rows matching the address text, shown by `suggestions` while typing.
    suggestions: Vec<Suggestion>,
    selected: Option<usize>,
    /// What the user typed, restored when stepping off the last suggestion or pressing Esc.
    typed: String,
    _address_events: Subscription,
    _address_focus: Subscription,
    _address_blur: Subscription,
    _activation_observer: Subscription,
    _bounds_observer: Subscription,
    /// `history/icons/`, beside the visits file.
    icon_dir: Option<PathBuf>,
    /// Site icons by host; `None` records a host without one so disk is read once.
    icons: RefCell<HashMap<String, Option<Arc<Image>>>>,
    /// Commands from the control socket; `None` unless started with `FELINAUT_AGENT=1`.
    agent: Option<agent::Queue>,
    /// What the hovered sidebar control does, shown in the status bar. GPUI tooltips would
    /// draw underneath the native page, so the status bar carries them instead.
    hint: Option<&'static str>,
    /// The sidebar row under the pointer; it reveals the pin and fills the status bar.
    hovered_tab: Option<u64>,
    tab_scroll: ScrollHandle,
    /// When the sidebar last started sliding; `None` once settled or with motion reduced.
    sidebar_anim: Option<Instant>,
    /// A hidden sidebar shown because the pointer touched the window's left edge, like Arc.
    peek: bool,
    /// Set when a tab drag starts; with an active drag it shows the pin drop zone.
    dragging_tab: bool,
    /// Where the tab being dragged over the rows would go: a line or a split preview.
    drop_hint: Option<DropHint>,
    /// The open right-click menu, if any.
    menu: Option<TabMenu>,
    /// Focus for the menu, so Escape closes it.
    menu_focus: FocusHandle,
    /// Recently closed tabs, newest last, for ⌘⇧T.
    closed: Vec<ClosedTab>,
    /// `tabs.tsv`; `None` without a home, or when it was unreadable so it is never overwritten.
    session_path: Option<PathBuf>,
    /// What `tabs.tsv` last received, so unchanged tabs are not rewritten every poll.
    last_session: RefCell<String>,
    live: Vec<live::LiveFolder>,
    /// `live.tsv`; `None` like `session_path`.
    live_path: Option<PathBuf>,
    last_live: RefCell<String>,
    fetch: Option<Fetch>,
    /// A live folder's right-click menu: its index and where the click was.
    live_menu: Option<(usize, Point<Pixels>)>,
    /// The live folder settings, open under the Live folders header.
    live_settings: bool,
    /// The live folder item under the pointer, by URL; it reveals the item's ×.
    hovered_live: Option<String>,
    /// Where live folder rows sit in the sidebar list, for `reveal_active`: how many children
    /// they add, and each item's child index by URL.
    live_layout: RefCell<(usize, HashMap<String, usize>)>,
    /// The hovered live folder item's title and address, for the status bar.
    live_hover: Option<String>,
    downloads: downloads::Downloads,
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
                this.submit(&event.0, window, cx);
            });
        let address_events = cx.subscribe_in(
            &address,
            window,
            |this, _, event: &AddressEvent, window, cx| match event {
                AddressEvent::Changed => this.update_suggestions(window, cx),
                AddressEvent::Up => this.move_selection(false, cx),
                AddressEvent::Down => this.move_selection(true, cx),
                AddressEvent::Escape => this.close_suggestions(true, window, cx),
                AddressEvent::ResetZoom => this.zoom(&Zoom(0), window, cx),
            },
        );
        let address_focus = address.read(cx).focus_handle(cx);
        let address_blur = cx.on_blur(&address_focus, window, |this, window, cx| {
            // GPUI also blurs when the window deactivates, as it does while a row or its × is
            // clicked in the dropdown; the activation observer below handles that case.
            if window.is_window_active() {
                this.close_suggestions(false, window, cx);
            }
            cx.notify();
        });
        // An empty address bar offers recent pages as soon as it is focused.
        let address_focus_observer = cx.on_focus(&address_focus, window, |this, window, cx| {
            if this.address.read(cx).text().is_empty() {
                this.update_suggestions(window, cx);
            }
            cx.notify();
        });
        // The dropdown floats above other apps, so it must go when the window loses focus;
        // clicking a suggestion also deactivates us briefly, hence the short grace period.
        let activation_observer = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() || this.suggestions.is_empty() {
                return;
            }
            cx.spawn_in(window, async move |this, cx| {
                Timer::after(Duration::from_millis(80)).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if !window.is_window_active() && !suggestions::is_active(cx) {
                        this.close_suggestions(false, window, cx);
                    }
                });
            })
            .detach();
        });
        // GPUI also reports bounds on activation changes (clicking a row makes the dropdown
        // key); only a real move or resize should close the list.
        let mut last_bounds = window.bounds();
        let bounds_observer = cx.observe_window_bounds(window, move |this, window, cx| {
            let bounds = window.bounds();
            if bounds != last_bounds {
                last_bounds = bounds;
                this.close_suggestions(false, window, cx);
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
            // Otherwise the dropdown outlives us and the app never quits.
            suggestions::close(cx);
            let _ = weak.update(cx, |this, _| {
                for tab in &mut this.tabs {
                    tab.page.take();
                }
                this.fetch = None;
                this.save_live();
                // The debounced save may still be pending; this is the last chance to write.
                if let Some((path, contents)) = this.history.take_snapshot()
                    && let Err(error) = history::write(&path, &contents)
                {
                    eprintln!("Could not save browsing history: {error}");
                }
                this.save_session();
                if let Some((path, contents)) = this.downloads.take_snapshot()
                    && let Err(error) = history::write(&path, &contents)
                {
                    eprintln!("Could not save downloads: {error}");
                }
            });
            true
        });
        let focus = cx.focus_handle();
        window.focus(&focus);
        let mut session_path = session::default_path();
        let saved = match session_path.as_deref().map(fs::read_to_string) {
            Some(Ok(text)) => session::parse(&text),
            Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Some(Err(error)) => {
                eprintln!("Could not read saved tabs: {error}");
                session_path = None;
                Vec::new()
            }
            None => Vec::new(),
        };
        let (tabs, active) = restore_tabs(saved);
        let mut live_path = live::default_path();
        let live = match live_path.as_deref().map(fs::read_to_string) {
            Some(Ok(text)) => live::parse(&text),
            Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Some(Err(error)) => {
                eprintln!("Could not read live folders: {error}");
                live_path = None;
                Vec::new()
            }
            None => Vec::new(),
        };
        let mut browser = Self {
            active,
            next_id: tabs.len() as u64 + 1,
            tabs,
            address,
            focus,
            _submit: subscription,
            _theme_observer: theme_observer,
            art_phase: 0.,
            zoom_flash: None,
            history: History::load(history::default_path()),
            last_save: Instant::now(),
            suggestions: Vec::new(),
            selected: None,
            typed: String::new(),
            _address_events: address_events,
            _address_focus: address_focus_observer,
            _address_blur: address_blur,
            _activation_observer: activation_observer,
            _bounds_observer: bounds_observer,
            icon_dir: history::default_path()
                .and_then(|path| path.parent().map(|dir| dir.join("icons"))),
            icons: RefCell::new(HashMap::new()),
            agent: agent::start(),
            hint: None,
            hovered_tab: None,
            tab_scroll: ScrollHandle::new(),
            sidebar_anim: None,
            peek: false,
            dragging_tab: false,
            drop_hint: None,
            menu: None,
            menu_focus: cx.focus_handle(),
            closed: Vec::new(),
            session_path,
            last_session: RefCell::new(String::new()),
            live,
            live_path,
            last_live: RefCell::new(String::new()),
            fetch: None,
            live_menu: None,
            live_settings: false,
            hovered_live: None,
            live_layout: RefCell::new((0, HashMap::new())),
            live_hover: None,
            downloads: downloads::Downloads::load(downloads::default_path()),
        };
        // Restored tabs load lazily; only the active one (and its split partner) needs a page.
        browser.load_if_needed(active, window);
        if let Some(partner) = browser.partner(active) {
            browser.load_if_needed(partner, window);
        }
        browser
    }
    fn pinned_count(&self) -> usize {
        self.tabs
            .iter()
            .take_while(|tab| tab.pinned.is_some())
            .count()
    }
    /// Writes `tabs.tsv` when the tabs differ from the last write. It runs on every poll and
    /// at window close, so navigations, titles, order, pins and splits all survive a quit.
    fn save_session(&self) {
        let Some(path) = &self.session_path else {
            return;
        };
        let mut saved = Vec::new();
        let mut previous_kept = false;
        for (index, tab) in self.tabs.iter().enumerate() {
            let url = tab.pinned.clone().unwrap_or_else(|| tab.url.clone());
            // Blank New Tabs have nothing to reopen.
            if tab.pinned.is_none() && (tab.home || tab.live.is_some() || !url.starts_with("http"))
            {
                previous_kept = false;
                continue;
            }
            saved.push(session::SavedTab {
                url,
                title: tab.title.clone(),
                pinned: tab.pinned.is_some(),
                split: previous_kept && index > 0 && self.partner(index) == Some(index - 1),
                active: index == self.active,
            });
            previous_kept = true;
        }
        let contents = session::serialize(&saved);
        if *self.last_session.borrow() == contents {
            return;
        }
        if let Err(error) = history::write(path, &contents) {
            eprintln!("Could not save tabs: {error}");
        }
        // Recorded even on failure, so a broken disk is reported once per change, not per poll.
        *self.last_session.borrow_mut() = contents;
    }
    /// Writes `live.tsv` when the folders differ from the last write, like `save_session`.
    fn save_live(&self) {
        let Some(path) = &self.live_path else {
            return;
        };
        let contents = live::serialize(&self.live);
        if *self.last_live.borrow() == contents {
            return;
        }
        // An empty list is only written once a file exists, so a fresh profile stays clean.
        if (!self.live.is_empty() || path.exists())
            && let Err(error) = history::write(path, &contents)
        {
            eprintln!("Could not save live folders: {error}");
        }
        *self.last_live.borrow_mut() = contents;
    }
    /// Drives the one hidden refresh page, and starts the next due section while the window
    /// is active.
    fn poll_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if let Some(fetch) = &mut self.fetch {
            let integration = self.live[fetch.folder].integration;
            let extractor = self.live[fetch.folder].sections[fetch.section].extractor;
            let mut signed_out = false;
            for event in fetch.page.drain_events() {
                match event {
                    Event::Finished(url) | Event::Url(url) if integration.is_sign_in(&url) => {
                        signed_out = true;
                    }
                    Event::Finished(_) if !fetch.loaded => {
                        fetch.loaded = true;
                        fetch.next_try = now + Duration::from_millis(300);
                    }
                    _ => {}
                }
            }
            let answer = fetch
                .result
                .lock()
                .ok()
                .and_then(|mut answer| answer.take());
            // An API answers a lapsed session with an error rather than a sign-in page.
            signed_out |= answer.as_deref() == Some(live::SIGNED_OUT);
            let mut done = answer.and_then(|json| live::parse_refresh(&json, integration.host));
            if done.is_none()
                && fetch.loaded
                && live_refresh_can_poll(fetch.started, fetch.next_try, now)
            {
                fetch.next_try = now + Duration::from_secs(1);
                let result = fetch.result.clone();
                let _ = fetch.page.evaluate(extractor, move |json| {
                    if let Ok(mut answer) = result.lock() {
                        *answer = Some(json);
                    }
                });
            }
            if done.is_none() && now.duration_since(fetch.started) >= FETCH_TIMEOUT {
                done = Some(Err(format!(
                    "{} did not answer. Click to try again.",
                    integration.name
                )));
            }
            if signed_out || done.is_some() {
                let (index, section) = (fetch.folder, fetch.section);
                self.fetch = None;
                let folder = &mut self.live[index];
                if signed_out {
                    folder.sign_out(now);
                } else {
                    let result = done.expect("a completed refresh has a result");
                    if result.is_ok() {
                        folder.signed_out = false;
                    }
                    let gone = folder.sections[section].finish_refresh(result, now);
                    if !gone.is_empty() {
                        self.sync_live_tabs();
                    }
                }
                cx.notify();
            }
            return;
        }
        if !window.is_window_active() {
            return;
        }
        let Some((index, section)) = self
            .live
            .iter()
            .enumerate()
            .find_map(|(index, folder)| Some((index, folder.due()?)))
        else {
            return;
        };
        match NativePage::hidden(window, self.live[index].sections[section].url) {
            Ok(page) => {
                self.fetch = Some(Fetch {
                    folder: index,
                    section,
                    page,
                    started: now,
                    loaded: false,

                    next_try: now,
                    result: Arc::new(Mutex::new(None)),
                });
            }
            Err(error) => {
                let section = &mut self.live[index].sections[section];
                section.error = Some(error);
                section.last = Some(now);
            }
        }
        cx.notify();
    }
    /// A tab finished loading a page of a signed-out integration's site, such as after the
    /// user signed in: check again now.
    fn recheck_live(&mut self, url: &str) {
        let host = host(url);
        for folder in &mut self.live {
            if folder.signed_out
                && host.as_deref() == Some(folder.integration.host)
                && !folder.integration.is_sign_in(url)
            {
                folder.refresh();
            }
        }
    }
    fn refresh_live(&mut self, index: usize, cx: &mut Context<Self>) {
        if self
            .fetch
            .as_ref()
            .is_some_and(|fetch| fetch.folder == index)
        {
            self.fetch = None;
        }
        if let Some(folder) = self.live.get_mut(index) {
            folder.refresh();
        }
        cx.notify();
    }
    fn remove_live(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.live.len() {
            return;
        }
        match &mut self.fetch {
            Some(fetch) if fetch.folder == index => self.fetch = None,
            Some(fetch) if fetch.folder > index => fetch.folder -= 1,
            _ => {}
        }
        self.live.remove(index);
        self.sync_live_tabs();
        self.save_live();
        cx.notify();
    }
    /// Shows a live folder item in its own tab, kept in the folder: selects the tab if the
    /// item has one, else opens it.
    fn open_live_item(
        &mut self,
        (folder, section, item): (usize, usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self
            .live
            .get_mut(folder)
            .and_then(|folder| folder.sections.get_mut(section))
            .and_then(|section| section.items.get_mut(item))
        else {
            return;
        };
        let (url, title) = (item.url.clone(), item.title.clone());
        live::restore_items(&mut self.live, &url);
        if let Some(index) = self.live_tab(&url) {
            self.select_tab(index, window, cx);
            return;
        }
        match NativePage::new(window, &url) {
            Ok(page) => {
                let page = Rc::new(page);
                let mut tab = Tab::new(self.next_id);
                self.next_id += 1;
                tab.url = url.clone();
                tab.title = title;
                tab.home = false;
                tab.loading = true;
                tab.page = Some(page.clone());
                tab.live = Some(url);
                // Live tabs sit after the others; the rows skip them wherever they are.
                self.tabs.push(tab);
                self.select_tab(self.tabs.len() - 1, window, cx);
                window.on_next_frame(move |_, _| {
                    let _ = page.focus();
                });
            }
            Err(error) => self.tabs[self.active].error = Some(error),
        }
        cx.notify();
    }
    fn live_tab(&self, url: &str) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.live.as_deref() == Some(url))
    }
    /// Closing an item, or its tab, hides it until the site reports an update.
    fn dismiss_live(&mut self, url: &str) {
        for item in self
            .live
            .iter_mut()
            .flat_map(|folder| &mut folder.sections)
            .flat_map(|section| &mut section.items)
            .filter(|item| item.url == url)
        {
            item.dismissed = Some(item.updated.clone());
            item.seen = true;
        }
    }
    /// The × of a live item: closes its tab if it has one, which dismisses it too.
    fn close_live_item(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.live_tab(url) {
            Some(index) => self.close_tab_at(index, window, cx),
            None => {
                self.dismiss_live(url);
                cx.notify();
            }
        }
    }
    /// Live tabs whose item is no longer listed in a shown section (merged, closed, its
    /// section hidden or folder removed) leave: an unseen one closes, the one the user is
    /// looking at stays as an ordinary tab.
    fn sync_live_tabs(&mut self) {
        let listed = |live: &[live::LiveFolder], url: &str| {
            live.iter()
                .flat_map(|folder| &folder.sections)
                .filter(|section| section.shown)
                .any(|section| section.items.iter().any(|item| item.url == url))
        };
        let mut index = 0;
        while index < self.tabs.len() {
            let orphan = self.tabs[index]
                .live
                .as_deref()
                .is_some_and(|url| !listed(&self.live, url));
            if !orphan {
                index += 1;
            } else if index == self.active {
                self.tabs[index].live = None;
                index += 1;
            } else {
                self.tabs.remove(index);
                if index < self.active {
                    self.active -= 1;
                }
            }
        }
    }
    fn new_live_folder(&mut self, _: &NewLiveFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.live_settings = true;
        if !self.sidebar_shown(cx) {
            self.toggle_sidebar(&ToggleSidebar, window, cx);
        }
        cx.notify();
    }
    fn toggle_live_section(&mut self, folder: usize, section: usize, cx: &mut Context<Self>) {
        let Some(section) = self
            .live
            .get_mut(folder)
            .and_then(|folder| folder.sections.get_mut(section))
        else {
            return;
        };
        section.shown = !section.shown;
        // Shown again: fetch it now rather than show a stale list.
        section.last = None;
        self.sync_live_tabs();
        self.save_live();
        cx.notify();
    }
    /// Adds the folder; its first refresh checks the sign-in before any list shows.
    fn add_live(&mut self, integration: live::Integration, cx: &mut Context<Self>) {
        if !self.live.iter().any(|f| f.integration == integration) {
            self.live.push(live::LiveFolder::new(integration));
            self.save_live();
        }
        cx.notify();
    }
    /// Pins at the end of the pinned group, or unpins to the top of the other tabs.
    fn toggle_pin_at(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs[index].pinned.is_none() && self.tabs[index].url() == HOME {
            return;
        }
        self.hint = None;
        let active_id = self.tabs[self.active].id;
        self.unsplit(index);
        let mut tab = self.tabs.remove(index);
        tab.pinned = match tab.pinned {
            Some(_) => None,
            None => Some(tab.url.clone()),
        };
        // Pinning takes a live tab out of its folder.
        tab.live = None;
        self.tabs.insert(self.pinned_count(), tab);
        self.active = self
            .tabs
            .iter()
            .position(|tab| tab.id == active_id)
            .unwrap_or(0);
        self.hide_others();
        self.save_session();
        cx.notify();
    }
    fn pin_tab(&mut self, _: &PinTab, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_pin_at(self.active, cx);
    }
    /// Saved with the other settings, so the sidebar stays collapsed across launches.
    fn sidebar_shown(&self, cx: &App) -> bool {
        cx.global::<ThemeSettings>().sidebar || self.peek
    }
    /// Starts a slide toward the opposite of the current state; call before flipping it.
    fn start_slide(&mut self, cx: &App) {
        self.sidebar_anim = theme::motion_allowed(cx).then(|| {
            // Reversing mid-slide continues from the current width instead of jumping.
            let left = SIDEBAR_SLIDE.saturating_sub(
                self.sidebar_anim
                    .map_or(SIDEBAR_SLIDE, |start| start.elapsed()),
            );
            Instant::now() - left
        });
    }
    /// Peeks the hidden sidebar while the pointer is at the window's left edge, and hides it
    /// again once the pointer moves past it or leaves the window.
    fn update_peek(&mut self, window: &Window, cx: &mut Context<Self>) {
        if cx.global::<ThemeSettings>().sidebar {
            self.peek = false;
            return;
        }
        let pointer = window
            .is_window_active()
            .then(|| webview::pointer(window))
            .flatten();
        let width = cx.global::<ThemeSettings>().sidebar_width;
        let peek = pointer.is_some_and(|at| {
            at.x <= if self.peek {
                width + PEEK_MARGIN
            } else {
                PEEK_EDGE
            }
        });
        if peek != self.peek {
            self.start_slide(cx);
            self.peek = peek;
            cx.notify();
        }
    }
    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.hint = None;
        // ⌘S during a peek keeps the already visible sidebar open: no slide.
        if !self.peek {
            self.start_slide(cx);
        }
        self.peek = false;
        cx.update_global::<ThemeSettings, _>(|settings, _| {
            settings.sidebar = !settings.sidebar;
            settings.save();
        });
        cx.notify();
    }
    fn activate_tab(&mut self, action: &ActivateTab, window: &mut Window, cx: &mut Context<Self>) {
        let index = action.0.min(self.tabs.len() - 1);
        self.select_tab(index, window, cx);
    }
    /// Cycles through tabs in sidebar order, wrapping at either end.
    fn step_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len();
        let index = if forward {
            (self.active + 1) % count
        } else {
            (self.active + count - 1) % count
        };
        self.select_tab(index, window, cx);
    }
    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(true, window, cx);
    }
    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(false, window, cx);
    }
    /// Pinned tabs restored at launch have no page until they are first shown.
    fn load_if_needed(&mut self, index: usize, window: &mut Window) {
        let tab = &mut self.tabs[index];
        if tab.page.is_some() || tab.home {
            return;
        }
        match NativePage::new(window, &tab.url) {
            Ok(page) => {
                tab.page = Some(Rc::new(page));
                tab.loading = true;
            }
            Err(error) => tab.error = Some(error),
        }
    }
    /// Runs one command from the control socket against the active tab.
    fn handle_agent(
        &mut self,
        request: agent::Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use serde_json::{Value, json};
        let command = &request.command;
        let result = match command["cmd"].as_str().unwrap_or_default() {
            "tabs" => Ok(self
                .tabs
                .iter()
                .enumerate()
                .map(|(index, tab)| {
                    json!({
                        "index": index,
                        "title": tab.display_title(),
                        "url": tab.url,
                        "active": index == self.active,
                        "loading": tab.loading,
                    })
                })
                .collect()),
            "tab" => match command["index"].as_u64().map(|index| index as usize) {
                Some(index) if index < self.tabs.len() => {
                    self.select_tab(index, window, cx);
                    Ok(Value::Null)
                }
                _ => Err("No such tab".into()),
            },
            "live" => self.agent_live(command, window, cx),
            "downloads" => {
                if command["show"].as_bool() == Some(true) {
                    self.show_downloads(&ShowDownloads, window, cx);
                }
                download_panel::agent(&mut self.downloads, command)
            }
            "navigate" => {
                self.navigate(command["url"].as_str().unwrap_or_default(), window, cx);
                let tab = &self.tabs[self.active];
                tab.error.clone().map_or_else(|| Ok(json!(tab.url)), Err)
            }
            _ => {
                let tab = &self.tabs[self.active];
                match (&tab.page, &self.agent) {
                    (Some(page), Some((_, queue))) if !tab.home => {
                        return agent::run(page, request, queue);
                    }
                    _ => Err("The active tab has no page. Navigate first.".into()),
                }
            }
        };
        let _ = request.reply.send(agent::response(result));
    }
    /// `live`: lists the live folders, after adding, refreshing or removing one if asked.
    fn agent_live(
        &mut self,
        command: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, String> {
        use serde_json::json;
        let index = |key| command[key].as_u64().map(|index| index as usize);
        if let Some(key) = command["add"].as_str() {
            let integration = live::ALL
                .into_iter()
                .find(|integration| integration.key == key)
                .ok_or_else(|| format!("Unknown integration: {key}"))?;
            self.add_live(integration, cx);
        } else if let Some(index) = index("refresh") {
            self.refresh_live(index, cx);
        } else if let Some(index) = index("remove") {
            self.remove_live(index, cx);
        } else if let Some([folder, section, item]) = command["open"]
            .as_array()
            .and_then(|at| {
                at.iter()
                    .map(|n| n.as_u64().map(|n| n as usize))
                    .collect::<Option<Vec<_>>>()
            })
            .and_then(|at| <[usize; 3]>::try_from(at).ok())
        {
            let url = self
                .live
                .get(folder)
                .and_then(|f| f.sections.get(section))
                .and_then(|s| s.items.get(item))
                .map(|item| item.url.clone())
                .ok_or("No such live folder item")?;
            if command["close"].as_bool() == Some(true) {
                self.close_live_item(&url, window, cx);
            } else {
                self.open_live_item((folder, section, item), window, cx);
            }
        }
        Ok(self
            .live
            .iter()
            .map(|folder| {
                json!({
                    "integration": folder.integration.key,
                    "open": folder.open,
                    "checking": folder.checking(),
                    "signed_out": folder.signed_out,
                    "sections": folder.sections.iter().map(|section| json!({
                        "label": section.label,
                        "shown": section.shown,
                        "loaded": section.last.is_some(),
                        "error": section.error,
                        "items": section.items.iter().map(|item| json!({
                            "title": item.title,
                            "url": item.url,
                            "seen": item.seen,
                            "updated": item.updated,
                            "dismissed": item.dismissed,
                            "tab": self.live_tab(&item.url),
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect())
    }
    fn icon_for(&self, host: &str) -> Option<Arc<Image>> {
        let mut icons = self.icons.borrow_mut();
        if let Some(icon) = icons.get(host) {
            return icon.clone();
        }
        // One small read per host per session; misses are remembered too.
        let icon = self
            .icon_dir
            .as_ref()
            .and_then(|dir| fs::read(icons::path(dir, host)).ok())
            .filter(|bytes| icons::valid_png(bytes))
            .map(|bytes| Arc::new(Image::from_bytes(ImageFormat::Png, bytes)));
        icons.insert(host.to_owned(), icon.clone());
        icon
    }
    /// The site's icon, or a themed monogram of its host.
    fn site_icon(&self, host: &str, size: f32, p: Palette) -> AnyElement {
        if let Some(image) = self.icon_for(host) {
            return img(image).size(px(size)).rounded_sm().into_any_element();
        }
        let (letter, slot) = icons::monogram(host);
        let color = [p.primary, p.secondary, p.accent, p.highlight][slot];
        div()
            .size(px(size))
            .flex_shrink_0()
            .rounded_md()
            .bg(rgb(color))
            .text_color(rgb(p.background))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(size * 0.6))
            .font_weight(FontWeight::BOLD)
            .child(letter.to_string())
            .into_any_element()
    }
    fn store_icon(&mut self, url: &str, png: Vec<u8>, cx: &mut Context<Self>) {
        let Some(host) = host(url) else {
            return;
        };
        let image = Arc::new(Image::from_bytes(ImageFormat::Png, png.clone()));
        let unchanged = self.icons.borrow().get(&host).is_some_and(|cached| {
            cached
                .as_ref()
                .is_some_and(|cached| cached.id() == image.id())
        });
        if unchanged {
            return;
        }
        if let Some(dir) = &self.icon_dir {
            let path = icons::path(dir, &host);
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = icons::write(&path, &png) {
                        eprintln!("Could not save site icon: {error}");
                    }
                })
                .detach();
        }
        self.icons.borrow_mut().insert(host, Some(image));
        cx.notify();
    }
    /// The idle address field: icon plus page title and domain, or the domain alone.
    fn title_chip(
        &self,
        display: AddressDisplay,
        not_secure: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = &self.tabs[self.active];
        let host = host(&tab.url).unwrap_or_default();
        let (label, detail) = if display == AddressDisplay::Domain {
            (host.clone(), None)
        } else {
            (tab.display_title(), Some(host.clone()))
        };
        div()
            .id("title-chip")
            // The address field's height, matching the 34 pt toolbar buttons.
            .h(px(34.))
            .w_full()
            .min_w_0()
            .rounded_lg()
            .bg(rgb(p.input))
            .border_1()
            .border_color(rgb(p.border))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .cursor_text()
            .hover(move |s| s.border_color(rgb(p.primary)))
            .on_click(
                cx.listener(|this, _, window, cx| this.focus_address(&FocusAddress, window, cx)),
            )
            .when(not_secure, |chip| chip.child(input::not_secure_badge(p)))
            .child(self.site_icon(&host, 16., p))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(14.))
                    .child(label),
            )
            .when_some(self.zoom_label(), |chip, (zoom, flash)| {
                chip.child(input::zoom_badge(p, zoom, flash).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.zoom(&Zoom(0), window, cx);
                    }),
                ))
            })
            .when_some(detail, |chip, detail| {
                chip.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(rgb(p.muted))
                        .child(detail),
                )
            })
            .into_any_element()
    }
    fn submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A selected or autocompleted row navigates to its exact URL, not the shown text.
        let target = self
            .selected
            .and_then(|index| self.suggestions.get(index))
            .map_or_else(|| text.to_owned(), |row| row.url.clone());
        self.close_suggestions(false, window, cx);
        self.navigate(&target, window, cx);
        let tab = &self.tabs[self.active];
        if !tab.home
            && tab.error.is_none()
            && let Some(page) = &tab.page
        {
            window.focus(&self.focus);
            let _ = page.focus();
        }
    }
    /// The user edited the address: refresh the rows and, with the caret at the end, complete
    /// the first row whose URL contains the text (`le` → `goog·le·.com`, dimmed around it).
    fn update_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.address.read(cx);
        let text = input.text().to_owned();
        let at_end = input.caret_at_end();
        self.typed = text;
        self.refresh_suggestions(window, cx);
        if at_end && let Some((index, start)) = history::completion(&self.typed, &self.suggestions)
        {
            let row = self.suggestions.remove(index);
            self.suggestions.insert(0, row);
            self.selected = Some(0);
            let display = &self.suggestions[0].display_url;
            let ghost = (
                display[..start].to_owned(),
                display[start + self.typed.len()..].to_owned(),
            );
            self.address
                .update(cx, |input, cx| input.set_completion(Some(ghost), cx));
        }
    }
    /// Recomputes the rows for the typed text and opens or closes the dropdown accordingly.
    fn refresh_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let anchor = self.address.read(cx).bounds();
        self.selected = None;
        self.suggestions = if self.typed.trim().is_empty() {
            self.history.recent(history::now(), suggestions::MAX_ROWS)
        } else {
            self.history
                .suggest(&self.typed, history::now(), suggestions::MAX_ROWS)
        };
        match anchor {
            Some(anchor) if !self.suggestions.is_empty() => {
                let browser = cx.weak_entity();
                let rows = self.suggestions.len();
                // Opening a window from inside this update would re-enter it; see `open_volume`.
                window.defer(cx, move |window, cx| {
                    suggestions::open(browser, anchor, rows, window, cx)
                });
            }
            _ => window.defer(cx, |_, cx| suggestions::close(cx)),
        }
        cx.notify();
    }
    /// ↓ walks from the typed text through the rows and back; ↑ the other way round.
    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.suggestions.len();
        if count == 0 {
            return;
        }
        self.selected = match (self.selected, down) {
            (None, true) => Some(0),
            (None, false) => Some(count - 1),
            (Some(index), true) => (index + 1 < count).then_some(index + 1),
            (Some(index), false) => index.checked_sub(1),
        };
        let text = match self.selected {
            Some(index) => self.suggestions[index].url.clone(),
            None => self.typed.clone(),
        };
        self.address
            .update(cx, |input, cx| input.set_text(text, cx));
        cx.notify();
    }
    fn close_suggestions(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        if restore && self.selected.is_some() {
            let typed = self.typed.clone();
            self.address
                .update(cx, |input, cx| input.set_text(typed, cx));
        }
        self.address
            .update(cx, |input, cx| input.set_completion(None, cx));
        if self.suggestions.is_empty() && self.selected.is_none() {
            return;
        }
        self.suggestions.clear();
        self.selected = None;
        window.defer(cx, |_, cx| suggestions::close(cx));
        cx.notify();
    }
    /// Called by the dropdown when a row is clicked.
    fn open_suggestion(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self.suggestions.get(index).map(|s| s.url.clone()) {
            self.selected = Some(index);
            self.submit(&url, window, cx);
        }
    }
    /// Called by a row's × button: forgets the page and refills the list from the typed text.
    fn remove_suggestion(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = self.suggestions.get(index).map(|s| s.url.clone()) else {
            return;
        };
        self.history.remove(&url);
        // Refresh first: `set_text` drops the painted bounds the dropdown is anchored to.
        self.refresh_suggestions(window, cx);
        let typed = self.typed.clone();
        self.address
            .update(cx, |input, cx| input.set_text(typed, cx));
    }
    /// Writes history at most once a second, off the UI thread.
    fn flush_history(&mut self, cx: &mut Context<Self>) {
        if self.last_save.elapsed() < Duration::from_secs(1) {
            return;
        }
        if let Some((path, contents)) = self.history.take_snapshot() {
            self.last_save = Instant::now();
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = history::write(&path, &contents) {
                        eprintln!("Could not save browsing history: {error}");
                    }
                })
                .detach();
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
        let requests: Vec<_> = self
            .agent
            .iter()
            .flat_map(|(requests, _)| requests.try_iter())
            .collect();
        for request in requests {
            self.handle_agent(request, window, cx);
        }
        let mut changed = false;
        let mut address_changed = false;
        let mut open = Vec::new();
        let mut icons = Vec::new();
        let mut finished = Vec::new();
        let mut page_focused = false;
        let mut focused_tab = None;
        let visible_partner = self.partner(self.active);
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(page) = tab.page.clone() else {
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
                        tab.load_failure = None;
                        tab.url = url;
                    }
                    Event::Failed(failure) => {
                        tab.show_failure(failure);
                        let _ = page.set_visible(false);
                        address_changed |= index == self.active;
                    }
                    Event::Stopped => tab.loading = page.is_loading(),
                    Event::Finished(url) => {
                        tab.loading = false;
                        finished.push(url.clone());
                        tab.url = url;
                        tab.record_visit(&mut self.history);
                        if !tab.audio.is_default() {
                            tab.apply_audio();
                        }
                    }
                    Event::Title(title) => {
                        tab.title = title;
                        if !tab.loading && tab.recorded == tab.url {
                            self.history.set_title(&tab.url, &tab.title);
                        }
                    }
                    Event::Url(url) => {
                        tab.url = url;
                        // Same-document navigations (pushState) never finish a load.
                        if !tab.loading {
                            tab.record_visit(&mut self.history);
                        }
                        address_changed |= index == self.active;
                    }
                    Event::Icon(png) => icons.push((tab.url.clone(), png)),
                    Event::Open(url) => open.push((url, true)),
                    Event::OpenInBackground(url) => open.push((url, false)),
                    Event::Hover(url) => tab.hovered_link = url,
                    Event::Focused => {
                        page_focused |= index == self.active || Some(index) == visible_partner;
                        focused_tab = Some(index);
                    }
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
        // Clicking into the other half of a split view makes it the active tab; a click on a
        // page never reaches GPUI, so it also has to close an open tab menu here.
        if let Some(index) = focused_tab {
            self.activate_half(index, cx);
            self.close_menu(window, cx);
        }
        let address_focused = self.address.read(cx).focus_handle(cx).is_focused(window);
        if address_changed && !address_focused {
            self.sync_address(cx);
        }
        // Clicking the page while editing the address ends the edit, like other browsers.
        if page_focused && address_focused {
            window.focus(&self.focus);
            self.sync_address(cx);
        }
        for (url, activate) in open {
            self.open_link_tab(&url, activate, window, cx);
        }
        for (url, png) in icons {
            self.store_icon(&url, png, cx);
        }
        for url in finished {
            self.recheck_live(&url);
        }
        for event in downloads::native::drain() {
            changed |= self.downloads.apply(event, history::now());
        }
        if let Some((path, contents)) = self.downloads.snapshot_due(Instant::now()) {
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = history::write(&path, &contents) {
                        eprintln!("Could not save downloads: {error}");
                    }
                })
                .detach();
        }
        self.poll_live(window, cx);
        self.update_peek(window, cx);
        self.flush_history(cx);
        self.save_session();
        self.save_live();
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
            Ok(()) => tab.start_load(url),
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
                tab.start_load(url.into());
                tab.page = Some(page.clone());
                let index = self.insert_tab(tab);
                if activate {
                    self.select_tab(index, window, cx);
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
    fn zoom(&mut self, zoom: &Zoom, _: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if let Some(page) = tab.page.as_ref().filter(|_| !tab.home) {
            let before = page.zoom();
            tab.error = page.set_zoom(zoom::step(before, zoom.0)).err();
            if before == 1.0 && theme::motion_allowed(cx) {
                self.zoom_flash = Some(Instant::now());
            }
            cx.notify();
        }
    }
    /// The active page's zoom level, shown in the address bar while it isn't 100 %.
    fn zoom_label(&self) -> Option<(SharedString, f32)> {
        let tab = &self.tabs[self.active];
        let label = zoom::label(tab.page.as_ref().filter(|_| !tab.home)?.zoom())?;
        let flash = self
            .zoom_flash
            .map_or(1., |at| at.elapsed().as_secs_f32() / 0.4);
        Some((label.into(), flash.min(1.)))
    }
    fn mute_tab(&mut self, _: &MuteTab, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_mute(self.tabs[self.active].id, cx);
    }
    fn volume_controls(&mut self, _: &VolumeControls, window: &mut Window, cx: &mut Context<Self>) {
        open_volume(cx.weak_entity(), None, window, cx);
    }
    fn show_downloads(&mut self, _: &ShowDownloads, window: &mut Window, cx: &mut Context<Self>) {
        let browser = cx.weak_entity();
        // The list reads the browser as it draws, so open it after this update.
        window.defer(cx, move |window, cx| {
            download_panel::toggle(browser, window, cx)
        });
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
    /// The index of the tab shown beside `index` in split view.
    fn partner(&self, index: usize) -> Option<usize> {
        let id = self.tabs[index].split?;
        self.tabs.iter().position(|tab| tab.id == id)
    }
    /// Ends the split view `index` is part of, if any.
    fn unsplit(&mut self, index: usize) {
        if let Some(partner) = self.partner(index) {
            self.tabs[partner].split = None;
        }
        self.tabs[index].split = None;
    }
    /// Native pages stay on screen until hidden; only the active tab and its split partner
    /// should be. The content canvas shows those two every frame.
    fn hide_others(&self) {
        let partner = self.partner(self.active);
        for (index, tab) in self.tabs.iter().enumerate() {
            if index != self.active
                && Some(index) != partner
                && let Some(page) = &tab.page
            {
                let _ = page.set_visible(false);
            }
        }
        debug_assert!(self.tabs_consistent(), "tab order invariants broken");
    }
    /// Pinned tabs come first; split partners are unpinned, adjacent and mutual.
    fn tabs_consistent(&self) -> bool {
        let pinned = self.pinned_count();
        self.tabs[pinned..].iter().all(|tab| tab.pinned.is_none())
            && (0..self.tabs.len()).all(|index| match self.partner(index) {
                None => self.tabs[index].split.is_none(),
                Some(partner) => {
                    index.abs_diff(partner) == 1
                        && self.tabs[index].pinned.is_none()
                        && self.tabs[partner].split == Some(self.tabs[index].id)
                }
            })
    }
    /// Makes the other half of the visible split active, keeping both pages where they are.
    fn activate_half(&mut self, index: usize, cx: &mut Context<Self>) {
        if index != self.active && self.partner(self.active) == Some(index) {
            self.active = index;
            self.sync_address(cx);
            cx.notify();
        }
    }
    fn open_menu(
        &mut self,
        id: u64,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some(TabMenu { id, at });
        self.live_menu = None;
        window.focus(&self.menu_focus);
        cx.notify();
    }
    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() | self.live_menu.take().is_some() {
            window.focus(&self.focus);
            cx.notify();
        }
    }
    /// Right-click menu for a tab: pin, split view and close, so neither depends on knowing
    /// a shortcut or the drag gesture.
    fn tab_menu(&self, menu: TabMenu, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == menu.id) else {
            return div().into_any_element();
        };
        let tab = &self.tabs[index];
        let id = tab.id;
        let pinned = tab.pinned.is_some();
        let split = tab.split.is_some();
        let full = cx.global::<ThemeSettings>().sidebar_width;
        let width = (full - 16.).min(240.);
        // Kept inside the sidebar: anything drawn over the native page is hidden beneath it.
        let x = f32::from(menu.at.x).clamp(8., (full - width - 8.).max(8.));
        // Unpinned tabs outside a split can pair with another such tab.
        let candidates: Vec<(u64, String)> = if pinned || split {
            Vec::new()
        } else {
            self.tabs[self.pinned_count()..]
                .iter()
                .filter(|other| other.id != id && other.split.is_none() && other.live.is_none())
                .take(6)
                .map(|other| (other.id, other.display_title()))
                .collect()
        };
        let can_pin = pinned || tab.url() != HOME;
        let body = div()
            .id("tab-menu")
            .track_focus(&self.menu_focus)
            .w(px(width))
            .p_1()
            .rounded_lg()
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.border))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_menu(window, cx);
                }
            }))
            .when(can_pin, |menu| {
                menu.child(
                    menu_item(
                        "menu-pin",
                        if pinned { "Unpin Tab" } else { "Pin Tab" },
                        Some("⌘D"),
                        p,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_menu(window, cx);
                        if let Some(index) = this.tabs.iter().position(|tab| tab.id == id) {
                            this.toggle_pin_at(index, cx);
                        }
                    })),
                )
            })
            .when(split, |menu| {
                menu.child(
                    menu_item("menu-unsplit", "End Split View", None, p).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.close_menu(window, cx);
                            if let Some(index) = this.tabs.iter().position(|tab| tab.id == id) {
                                this.unsplit(index);
                                this.hide_others();
                                cx.notify();
                            }
                        },
                    )),
                )
            })
            .when(!candidates.is_empty(), |menu| {
                menu.child(menu_separator(p))
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .child("Show side by side with"),
                    )
                    .children(candidates.into_iter().map(|(other, title)| {
                        menu_item(("menu-split", other), title, None, p).on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.close_menu(window, cx);
                                if let Some(index) = this.tabs.iter().position(|tab| tab.id == id) {
                                    this.split_tabs(other, index, window, cx);
                                }
                            },
                        ))
                    }))
            })
            .child(menu_separator(p))
            .child(
                menu_item("menu-close", "Close Tab", Some("⌘W"), p).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.close_menu(window, cx);
                        if let Some(index) = this.tabs.iter().position(|tab| tab.id == id) {
                            this.close_tab_at(index, window, cx);
                        }
                    },
                )),
            );
        deferred(
            anchored()
                .position(point(px(x), menu.at.y))
                .snap_to_window_with_margin(px(8.))
                .child(body),
        )
        .with_priority(1)
        .into_any_element()
    }
    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs[self.active].hovered_link = None;
        if let Some(page) = &self.tabs[self.active].page {
            let _ = page.focus_parent();
        }
        self.active = index;
        self.tabs[index].hovered_link = None;
        self.load_if_needed(index, window);
        if let Some(partner) = self.partner(index) {
            self.load_if_needed(partner, window);
        }
        self.hide_others();
        self.reveal_active();
        window.focus(&self.focus);
        self.sync_address(cx);
        cx.notify();
    }
    /// New tabs open at the top of the unpinned list, right under the New tab row.
    fn insert_tab(&mut self, tab: Tab) -> usize {
        let at = self.pinned_count();
        self.tabs.insert(at, tab);
        if self.active >= at {
            self.active += 1;
        }
        at
    }
    /// Scrolls the sidebar just enough to show the active row.
    fn reveal_active(&self) {
        let pinned = self.pinned_count();
        // List children: the pinned grid (when pins exist), the New tab row, then the rows,
        // where a split view's two tabs share one.
        let (live_rows, live_items) = &*self.live_layout.borrow();
        let first_live = usize::from(pinned > 0);
        let row = if self.active < pinned {
            0
        } else if let Some(at) = self.tabs[self.active]
            .live
            .as_ref()
            .and_then(|url| live_items.get(url))
        {
            first_live + at
        } else {
            let row = self
                .rows()
                .iter()
                .position(|&(first, last)| (first..=last).contains(&self.active))
                .unwrap_or(0);
            row + first_live + live_rows + 1
        };
        self.tab_scroll.scroll_to_item(row);
    }
    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        let tab = Tab::new(self.next_id);
        self.next_id += 1;
        let index = self.insert_tab(tab);
        self.select_tab(index, window, cx);
        self.focus_address(&FocusAddress, window, cx);
    }
    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, window, cx);
    }
    /// Reopens the most recently closed tab where it was, back in its split view when the
    /// other half is still open and free.
    fn reopen_tab(&mut self, _: &ReopenTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.closed.pop() else {
            return;
        };
        let page = match NativePage::new(window, &closed.url) {
            Ok(page) => Rc::new(page),
            Err(error) => {
                self.tabs[self.active].error = Some(error);
                cx.notify();
                return;
            }
        };
        let mut tab = Tab::new(self.next_id);
        self.next_id += 1;
        let id = tab.id;
        tab.url = closed.url;
        tab.title = closed.title;
        tab.home = false;
        tab.loading = true;
        tab.page = Some(page);
        // Back among the unpinned tabs, never between the two halves of a split view.
        let pinned = self.pinned_count();
        let mut at = closed.index.clamp(pinned, self.tabs.len());
        if at < self.tabs.len() && at > 0 && self.partner(at) == Some(at - 1) {
            at += 1;
        }
        self.tabs.insert(at, tab);
        if self.active >= at {
            self.active += 1;
        }
        let partner = closed
            .partner
            .and_then(|partner| self.tabs.iter().position(|tab| tab.id == partner))
            .filter(|&index| self.tabs[index].split.is_none() && self.tabs[index].pinned.is_none());
        match partner {
            Some(index) => self.split_tabs(id, index, window, cx),
            None => self.select_tab(at, window, cx),
        }
    }
    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.hint = None;
        self.hovered_tab = None;
        if index == self.active {
            if let Some(page) = &self.tabs[index].page {
                let _ = page.focus_parent();
            }
            window.focus(&self.focus);
        }
        if let Some(url) = self.tabs[index].pinned.clone() {
            self.unload_pinned(index, url, window, cx);
            return;
        }
        if let Some(url) = self.tabs[index].live.clone() {
            self.dismiss_live(&url);
        }
        let tab = &self.tabs[index];
        if !tab.home && tab.page.is_some() {
            // shortcut: session-only and capped at 20; persist it if restoring after quit matters.
            if self.closed.len() == 20 {
                self.closed.remove(0);
            }
            self.closed.push(ClosedTab {
                url: tab.url.clone(),
                title: tab.title.clone(),
                index,
                partner: tab.split,
            });
        }
        self.unsplit(index);
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(self.next_id));
            self.next_id += 1;
        }
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.load_if_needed(self.active, window);
        if let Some(partner) = self.partner(self.active) {
            self.load_if_needed(partner, window);
        }
        self.hide_others();
        self.reveal_active();
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
        // A failed load added no history entry: going back shows the page still underneath.
        if tab.load_failure.is_some() {
            match tab.page.as_ref().and_then(|page| page.document()) {
                Some((url, title)) => {
                    tab.load_failure = None;
                    tab.url = url;
                    tab.title = title;
                    self.sync_address(cx);
                    cx.notify();
                }
                None => self.home(&Home, window, cx),
            }
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
                tab.load_failure = None;
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
            // Reloading would load the page underneath the error page, not the one that failed.
            tab.error = match &tab.load_failure {
                Some(failure) => page.navigate(&failure.url).err(),
                None => page.reload().err(),
            };
            tab.loading |= tab.error.is_none();
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
                open_volume(cx.weak_entity(), anchor, window, cx);
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
                                    .child("SMALL BROWSER. INFINITE CURIOSITY."),
                            )
                            .child(
                                div()
                                    .w(px(280.))
                                    .h(px(210.))
                                    .flex_shrink_0()
                                    .relative()
                                    .top(px((phase * 1.2).sin() * 6.))
                                    .child(brand::mascot().absolute().inset_0().size_full())
                                    .child(
                                        brand::exploration_box().absolute().inset_0().size_full(),
                                    ),
                            )
                            .child(
                                div()
                                    .font_family("Georgia")
                                    .italic()
                                    .text_size(px(48.))
                                    .line_height(px(52.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(p.text))
                                    .child("Felinaut"),
                            )
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .text_color(rgb(p.muted))
                                    .child("A little curiosity. A whole universe."),
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
                                            .child(format!("About Felinaut · {DISPLAY_VERSION}")),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
    /// Replaces a page whose load failed. Try Again loads the failed URL again.
    fn error_page(
        &self,
        index: usize,
        failure: &LoadFailure,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme::palette(cx);
        let address: String = failure.url.chars().take(160).collect();
        let note = |text: SharedString| {
            div()
                .max_w(px(520.))
                .text_center()
                .text_color(rgb(p.muted))
                .child(text)
        };
        div()
            .id(("error-page", self.tabs[index].id))
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(p.background))
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
                    .child(brand::mascot().w(px(160.)).h(px(120.)).flex_shrink_0())
                    .child(
                        div()
                            .text_size(px(26.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(p.text))
                            .child(failure.kind.text().0),
                    )
                    .child(note(failure.kind.text().1.into()))
                    .child(
                        div()
                            .max_w(px(520.))
                            .text_sm()
                            .text_color(rgb(p.text))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(address),
                    )
                    .child(
                        div()
                            .id("error-retry")
                            .mt_2()
                            .px_4()
                            .py_2()
                            .rounded_lg()
                            .cursor_pointer()
                            .bg(rgb(p.primary))
                            .text_color(rgb(p.background))
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.activate_half(index, cx);
                                this.reload(&Reload, window, cx);
                            }))
                            .child("Try Again  ⌘R"),
                    )
                    .child(note(failure.detail.clone().into()).text_xs())
                    .when(failure.certificate.is_some(), |page| {
                        page.child(
                            div()
                                .id("error-bypass")
                                .mt_2()
                                .text_sm()
                                .whitespace_nowrap()
                                .underline()
                                .cursor_pointer()
                                .text_color(rgb(p.text))
                                .hover(move |s| s.text_color(rgb(p.primary)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if let Some(failure) = &this.tabs[index].load_failure
                                        && let Some(certificate) = failure.certificate.clone()
                                    {
                                        webview::allow_certificate(&failure.url, certificate);
                                    }
                                    this.activate_half(index, cx);
                                    this.reload(&Reload, window, cx);
                                }))
                                .child("Visit this site anyway"),
                        )
                        .child(
                            note(
                                "Until Felinaut quits. Anyone on your network could read or change \
                                 what you send, even passwords."
                                    .into(),
                            )
                            .text_xs(),
                        )
                    }),
            )
            .into_any_element()
    }
    /// Closing a pinned tab frees its page and resets it to the pinned URL; the pin stays.
    fn unload_pinned(
        &mut self,
        index: usize,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = &mut self.tabs[index];
        if let Some(page) = tab.page.take() {
            let _ = page.set_visible(false);
        }
        *tab = Tab::pinned(tab.id, url, std::mem::take(&mut tab.title));
        if index == self.active {
            if self.tabs.len() == 1 {
                self.tabs.push(Tab::new(self.next_id));
                self.next_id += 1;
            }
            let next = if index + 1 < self.tabs.len() {
                index + 1
            } else {
                index - 1
            };
            self.select_tab(next, window, cx);
        }
        cx.notify();
    }
    /// Shows `text` in the status bar while the element is hovered.
    fn hint_on_hover(
        text: &'static str,
        cx: &mut Context<Self>,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered {
                this.hint = Some(text);
            } else if this.hint == Some(text) {
                this.hint = None;
            }
            cx.notify();
        })
    }
    /// A 24 × 24 icon control inside a tab row.
    fn row_control(id: (&'static str, u64), p: Palette) -> Stateful<Div> {
        div()
            .id(id)
            .size(px(24.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_color(rgb(p.muted))
            // `border` differs from both the selected and the hovered row fill in every theme.
            .hover(move |s| s.bg(rgb(p.border)).text_color(rgb(p.text)))
    }
    fn sidebar_button(&self, p: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        self.button("sidebar-toggle", "", p)
            .on_click(
                cx.listener(|this, _, window, cx| this.toggle_sidebar(&ToggleSidebar, window, cx)),
            )
            .on_hover(Self::hint_on_hover(
                if cx.global::<ThemeSettings>().sidebar {
                    "Hide sidebar · ⌘S"
                } else if self.peek {
                    "Keep sidebar open · ⌘S"
                } else {
                    "Show sidebar · ⌘S"
                },
                cx,
            ))
            .child(
                brand::icon(brand::icon::SIDEBAR)
                    .size(px(18.))
                    .text_color(rgb(p.muted)),
            )
    }
    /// One tab inside a sidebar row: icon, title, sound and × (selected or hovered). A row
    /// holds one cell, or two in split view.
    fn tab_cell(&self, index: usize, p: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let tab = &self.tabs[index];
        let selected = self.active == index;
        let id = tab.id;
        let hovered = self.hovered_tab == Some(id);
        div()
            .id(("tab", id))
            .flex_1()
            .min_w_0()
            .h_full()
            .px_2()
            .rounded_lg()
            // Selection is the strongest state: fill plus an outline, which light themes need
            // because their `elevated` is close to `background`. Hover only hints.
            .border_1()
            .border_color(rgb(if selected { p.border } else { p.background }))
            .bg(rgb(if selected { p.elevated } else { p.background }))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .when(!selected, |row| {
                row.hover(move |s| s.bg(rgb(p.surface)).border_color(rgb(p.surface)))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered_tab = Some(id);
                } else if this.hovered_tab == Some(id) {
                    this.hovered_tab = None;
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_tab(index, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(id, event.position, window, cx);
                }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(if selected { p.primary } else { p.muted }))
                    .child(if tab.loading && !tab.home {
                        div().child("◌").into_any_element()
                    } else if let Some(host) = (!tab.home).then(|| host(&tab.url)).flatten() {
                        self.site_icon(&host, 16., p)
                    } else {
                        brand::symbol().size(px(18.)).into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_sm()
                    .child(tab.display_title()),
            )
            .when(tab.sound.audible() || tab.audio.muted, |row| {
                let muted = tab.audio.muted;
                row.child(
                    Self::row_control(("tab-sound", id), p)
                        .on_hover(Self::hint_on_hover(
                            if muted {
                                "Unmute tab · ⌘⇧M"
                            } else {
                                "Mute tab · ⌘⇧M"
                            },
                            cx,
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_mute(id, cx);
                        }))
                        .child(
                            brand::icon(volume::speaker_icon(tab.audio))
                                .size(px(14.))
                                .text_color(rgb(if muted { p.muted } else { p.primary })),
                        ),
                )
            })
            .when(hovered || selected, |row| {
                row.child(
                    Self::row_control(("close", id), p)
                        .on_hover(Self::hint_on_hover("Close tab · ⌘W", cx))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_at(index, window, cx);
                        }))
                        .child("×"),
                )
            })
    }
    /// Unpinned tabs grouped into sidebar rows as `(first, last)` indices; a split view's two
    /// tabs share one row.
    fn rows(&self) -> Vec<(usize, usize)> {
        let mut rows = Vec::new();
        let mut index = self.pinned_count();
        while index < self.tabs.len() {
            if self.tabs[index].live.is_some() {
                index += 1;
                continue;
            }
            let last = if self.partner(index) == Some(index + 1) {
                index + 1
            } else {
                index
            };
            rows.push((index, last));
            index = last + 1;
        }
        rows
    }
    /// A sidebar row (one tab, or a split view's two) and the drop target for tab drags: near
    /// its top or bottom edge a line shows where the tab goes; over the middle of a single tab
    /// a placeholder shows the split view that releasing opens.
    fn tab_row(
        &self,
        (first, last): (usize, usize),
        last_row: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let split = first != last;
        let cell_width = self.list_width(cx) / if split { 2. } else { 1. };
        let hint = self.drop_hint;
        let cells: Vec<_> = (first..=last)
            .map(|index| {
                let cell = self.tab_cell(index, p, cx);
                self.draggable(cell, index, size(px(cell_width), px(34.)), p, cx)
            })
            .collect();
        div()
            .id(("row", self.tabs[first].id))
            .relative()
            .h(px(34.))
            .w_full()
            .flex_shrink_0()
            .flex()
            .gap_1()
            .when(split, |row| row.rounded_lg().bg(rgb(p.surface)))
            .children(cells)
            .when(hint == Some(DropHint::Split(first)), |row| {
                row.child(
                    div()
                        .flex_1()
                        .h_full()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(p.text))
                        .bg(rgb(p.surface)),
                )
            })
            .when(hint == Some(DropHint::Before(first)), |row| {
                row.child(drop_line(true, p))
            })
            .when(hint == Some(DropHint::After(last)), |row| {
                row.child(drop_line(false, p))
            })
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedTab>, _, cx| {
                    let at = event.event.position;
                    let bounds = event.bounds;
                    let hint = if bounds.contains(&at) {
                        let y = (at.y - bounds.top()) / bounds.size.height;
                        let dragged = event.drag(cx).0;
                        let own = (first..=last)
                            .any(|index| this.tabs.get(index).is_some_and(|t| t.id == dragged));
                        // Only a single tab other than the dragged one can become a split.
                        let can_split = !split && !own;
                        if y < 0.3 || (!can_split && y < 0.5) {
                            DropHint::Before(first)
                        } else if y > 0.7 || !can_split {
                            DropHint::After(last)
                        } else {
                            DropHint::Split(first)
                        }
                    } else if last_row
                        && at.y > bounds.bottom()
                        && (bounds.left()..bounds.right()).contains(&at.x)
                    {
                        DropHint::After(last)
                    } else {
                        return;
                    };
                    if this.drop_hint != Some(hint) {
                        this.drop_hint = Some(hint);
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(|this, dragged: &DraggedTab, window, cx| {
                this.apply_drop(dragged.0, window, cx);
            }))
    }
    /// Applies the line or split the pointer was showing when a tab drag was released.
    fn apply_drop(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        match self.drop_hint.take() {
            Some(DropHint::Before(index)) => self.drop_tab(id, index, false, cx),
            Some(DropHint::After(index)) => self.drop_tab(id, index + 1, false, cx),
            Some(DropHint::Split(index)) => self.split_tabs(id, index, window, cx),
            None => self.dragging_tab = false,
        }
    }
    /// Moves a dragged tab before the tab at `at` (pre-removal index, `usize::MAX` for the
    /// end). Landing among pinned tiles pins it, among rows unpins it; either way it leaves
    /// any split view. Blank New Tabs cannot be pinned.
    fn drop_tab(&mut self, id: u64, at: usize, pin: bool, cx: &mut Context<Self>) {
        self.dragging_tab = false;
        self.drop_hint = None;
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        if pin && self.tabs[from].pinned.is_none() && self.tabs[from].url() == HOME {
            return;
        }
        let active_id = self.tabs[self.active].id;
        self.unsplit(from);
        let mut tab = self.tabs.remove(from);
        if pin != tab.pinned.is_some() {
            tab.pinned = pin.then(|| tab.url.clone());
        }
        let to = drop_index(at, from, pin, self.pinned_count(), self.tabs.len());
        self.tabs.insert(to, tab);
        self.active = self
            .tabs
            .iter()
            .position(|tab| tab.id == active_id)
            .unwrap_or(0);
        self.hovered_tab = None;
        self.hide_others();
        self.save_session();
        cx.notify();
    }
    /// Shows the dragged tab beside the tab at `target` in split view and activates it.
    fn split_tabs(&mut self, id: u64, target: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.dragging_tab = false;
        let (Some(from), Some(target_tab)) = (
            self.tabs.iter().position(|tab| tab.id == id),
            self.tabs.get(target),
        ) else {
            return;
        };
        let target_id = target_tab.id;
        if target_id == id || target_tab.split.is_some() || target_tab.pinned.is_some() {
            return;
        }
        let active_id = self.tabs[self.active].id;
        self.unsplit(from);
        let mut tab = self.tabs.remove(from);
        tab.pinned = None;
        tab.split = Some(target_id);
        let left = self
            .tabs
            .iter()
            .position(|tab| tab.id == target_id)
            .unwrap_or(0);
        self.tabs.insert(left + 1, tab);
        self.tabs[left].split = Some(id);
        self.active = self
            .tabs
            .iter()
            .position(|tab| tab.id == active_id)
            .unwrap_or(0);
        self.hovered_tab = None;
        self.save_session();
        self.select_tab(left + 1, window, cx);
    }
    /// The sidebar list's inner width: the sidebar minus its border and padding.
    fn list_width(&self, cx: &App) -> f32 {
        cx.global::<ThemeSettings>().sidebar_width - 1. - 16.
    }
    /// Makes a row cell or tile draggable, with a faint ghost of it following the pointer.
    fn draggable(
        &self,
        element: Stateful<Div>,
        index: usize,
        size: Size<Pixels>,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let tab = &self.tabs[index];
        let title = SharedString::from(tab.display_title());
        let icon = (!tab.home)
            .then(|| host(&tab.url))
            .flatten()
            .and_then(|host| self.icon_for(&host));
        let browser = cx.weak_entity();
        element.on_drag(DraggedTab(tab.id), move |_, _, _, cx| {
            let _ = browser.update(cx, |this, cx| {
                this.dragging_tab = true;
                cx.notify();
            });
            let ghost = Ghost {
                title: title.clone(),
                icon: icon.clone(),
                size,
                p,
            };
            cx.new(|_| DragPreview(Some(ghost)))
        })
    }
    /// The sidebar's current width: its saved width or 0, eased in between while it slides.
    fn sidebar_width(&self, cx: &App) -> f32 {
        let shown = self.sidebar_shown(cx);
        let full = cx.global::<ThemeSettings>().sidebar_width;
        let t = self.sidebar_anim.map_or(1., |start| {
            (start.elapsed().as_secs_f32() / SIDEBAR_SLIDE.as_secs_f32()).min(1.)
        });
        let eased = 1. - (1. - t).powi(3);
        full * if shown { eased } else { 1. - eased }
    }
    /// Arc-style pinned tile: the site icon centred in a square-ish card, three per row.
    fn pinned_tile(&self, index: usize, p: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let tab = &self.tabs[index];
        let selected = self.active == index;
        let id = tab.id;
        // Three tiles and two 8 px gaps fill the grid: the width minus the sidebar border,
        // the list padding and the grid's own drop-target border.
        let tile = (self.list_width(cx) - 2. - 16.) / 3.;
        let element =
            div()
                .id(("pinned", id))
                .relative()
                .w(px(tile))
                .h(px(46.))
                .flex_shrink_0()
                .rounded_xl()
                .border_1()
                // Light themes' white tiles vanish on their near-white sidebar without an edge.
                .border_color(rgb(if selected || !p.dark {
                    p.border
                } else {
                    p.surface
                }))
                .bg(rgb(if selected { p.elevated } else { p.surface }))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(!selected, |tile| {
                    let edge = if p.dark { p.elevated } else { p.border };
                    tile.hover(move |s| s.bg(rgb(p.elevated)).border_color(rgb(edge)))
                })
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.hovered_tab = Some(id);
                    } else if this.hovered_tab == Some(id) {
                        this.hovered_tab = None;
                    }
                    cx.notify();
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_tab(index, window, cx);
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_menu(id, event.position, window, cx);
                    }),
                )
                .child(div().when(tab.loading, |icon| icon.opacity(0.5)).child(
                    match host(&tab.url) {
                        Some(host) => self.site_icon(&host, 20., p),
                        None => brand::symbol().size(px(20.)).into_any_element(),
                    },
                ));
        // Pin and unpin by dragging or ⌘D; ⌘W on a selected tile frees its page, keeping the pin.
        // A tab dropped on a tile takes its place: after it when dragged forward, before it
        // when dragged back.
        self.draggable(element, index, size(px(tile), px(46.)), p, cx)
            .drag_over::<DraggedTab>(move |style, _, _, _| style.border_color(rgb(p.text)))
            .on_drag_move(cx.listener(clear_drop_hint))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                let from = this.tabs.iter().position(|tab| tab.id == dragged.0);
                let at = if from.is_some_and(|from| from < index) {
                    index + 1
                } else {
                    index
                };
                this.drop_tab(dragged.0, at, true, cx);
            }))
    }
    /// A disclosure chevron, pointing down while open.
    fn chevron(open: bool, p: Palette) -> Svg {
        brand::icon(brand::icon::CHEVRON)
            .size(px(10.))
            .flex_shrink_0()
            .text_color(rgb(p.muted))
            .when(open, |icon| {
                icon.with_transformation(Transformation::rotate(radians(
                    std::f32::consts::FRAC_PI_2,
                )))
            })
    }
    /// A one-line row inside a live folder, indented under the folder's icon.
    fn live_row(id: impl Into<ElementId>, height: f32) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(height))
            .w_full()
            .flex_shrink_0()
            .pl(px(26.))
            .pr_2()
            .rounded_lg()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
    }
    /// A quiet note in a live folder: checking, empty or failed.
    fn live_note(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        p: Palette,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(26.))
            .flex_shrink_0()
            .pl(px(50.))
            .pr_2()
            .flex()
            .items_center()
            .text_xs()
            .text_color(rgb(p.muted))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(text.into()),
            )
    }
    /// The always-visible Live folders header, which opens their settings, then each folder.
    /// Records where items land in the list for `reveal_active`.
    fn live_folders(&self, p: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = vec![self.live_header(p, cx)];
        if self.live_settings {
            rows.push(self.live_settings_view(p, cx));
        } else if self.live.is_empty() {
            rows.push(
                Self::live_row("live-empty-add", 28.)
                    .pl(px(8.))
                    .text_xs()
                    .text_color(rgb(p.muted))
                    .hover(move |s| s.bg(rgb(p.surface)).text_color(rgb(p.text)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_live_folder(&NewLiveFolder, window, cx)
                    }))
                    .child(
                        div()
                            .w(px(18.))
                            .flex()
                            .justify_center()
                            .text_sm()
                            .child("+"),
                    )
                    .child(format!(
                        "Add {}",
                        live::ALL.map(|spec| spec.name).join(" or ")
                    ))
                    .into_any_element(),
            );
        }
        let mut items = HashMap::new();
        for (index, folder) in self.live.iter().enumerate() {
            rows.push(self.live_folder_row(index, folder, p, cx));
            if folder.open {
                self.live_folder_body(index, folder, &mut rows, &mut items, p, cx);
            }
        }
        // Set apart from the tabs, like the pinned tiles.
        rows.push(div().h(px(6.)).flex_shrink_0().into_any_element());
        *self.live_layout.borrow_mut() = (rows.len(), items);
        rows
    }
    fn live_header(&self, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let open = self.live_settings;
        div()
            .id("live-header")
            .h(px(26.))
            .flex_shrink_0()
            .px_2()
            .rounded_lg()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(rgb(p.muted))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(p.surface)))
            .on_hover(Self::hint_on_hover("Choose which live folders to show", cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.live_settings = !open;
                cx.notify();
            }))
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Live folders"),
            )
            .child(
                div()
                    .text_color(rgb(if open { p.primary } else { p.muted }))
                    .child(if open { "Done" } else { "Edit" }),
            )
            .into_any_element()
    }
    /// A checkbox row in the live folder settings.
    fn live_check(
        id: impl Into<ElementId>,
        checked: bool,
        label: &'static str,
        p: Palette,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(26.))
            .pl(px(34.))
            .pr_2()
            // Concentric with the 8 pt panel and its 4 pt padding.
            .rounded(px(4.))
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(p.elevated)))
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(rgb(if checked { p.primary } else { p.muted }))
                    .when(checked, |box_| box_.bg(rgb(p.primary)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(checked, |box_| {
                        box_.child(
                            brand::icon(brand::icon::CHECK)
                                .size(px(10.))
                                .text_color(rgb(p.background)),
                        )
                    }),
            )
            .child(label)
    }
    /// The premade integrations, each with an Add or Remove control and, once added, the
    /// lists it shows.
    fn live_settings_view(&self, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("live-settings")
            .flex_shrink_0()
            .mb_1()
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(rgb(p.border))
            .bg(rgb(p.surface))
            .flex()
            .flex_col()
            .children(live::ALL.into_iter().map(|integration| {
                let added = self.live.iter().position(|f| f.integration == integration);
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_2()
                            .py(px(6.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                brand::icon(integration.icon)
                                    .size(px(18.))
                                    .flex_shrink_0()
                                    .text_color(rgb(p.text)),
                            )
                            .child(div().flex_1().min_w_0().text_sm().child(integration.name))
                            .child(
                                div()
                                    .id(integration.key)
                                    .px_2()
                                    .py(px(2.))
                                    .rounded_md()
                                    .text_xs()
                                    .cursor_pointer()
                                    .map(|button| match added {
                                        Some(index) => button
                                            .border_1()
                                            .border_color(rgb(p.border))
                                            .text_color(rgb(p.muted))
                                            .hover(move |s| s.text_color(rgb(p.text)))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.remove_live(index, cx)
                                            }))
                                            .child("Remove"),
                                        None => button
                                            .bg(rgb(p.primary))
                                            .text_color(rgb(p.background))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.add_live(integration, cx)
                                            }))
                                            .child("Add"),
                                    }),
                            ),
                    )
                    .map(|block| match added {
                        None => block.child(
                            div()
                                .pl(px(34.))
                                .pr_2()
                                .pb(px(6.))
                                .text_xs()
                                .text_color(rgb(p.muted))
                                .child(integration.summary),
                        ),
                        Some(index) => {
                            block.children(self.live[index].sections.iter().enumerate().map(
                                |(s, section)| {
                                    Self::live_check(
                                        ("live-shown", index * 8 + s),
                                        section.shown,
                                        section.label,
                                        p,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.toggle_live_section(index, s, cx)
                                        },
                                    ))
                                },
                            ))
                        }
                    })
            }))
            .into_any_element()
    }
    /// A folder's sign-in state, or its shown sections and their items as tabs.
    fn live_folder_body(
        &self,
        index: usize,
        folder: &live::LiveFolder,
        rows: &mut Vec<AnyElement>,
        items: &mut HashMap<String, usize>,
        p: Palette,
        cx: &mut Context<Self>,
    ) {
        let name = folder.integration.name;
        if !folder.has_enabled_sections() {
            rows.push(
                Self::live_note(
                    ("live-disabled", index),
                    "No lists enabled. Choose lists in Live folders settings.",
                    p,
                )
                .into_any_element(),
            );
            return;
        }
        if folder.checking() {
            rows.push(
                Self::live_note(
                    ("live-checking", index),
                    format!("Checking your {name} sign-in…"),
                    p,
                )
                .into_any_element(),
            );
            return;
        }
        if folder.signed_out {
            let url = folder.integration.sign_in_url;
            rows.push(
                Self::live_row(("live-sign-in", index), 30.)
.hover(move |s| s.bg(rgb(p.surface)))
                    .text_sm()
                    .text_color(rgb(p.primary))
                    .on_hover(Self::hint_on_hover(
                        "Opens the sign-in page in a new tab. The folder fills in once you are signed in",
                        cx,
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_link_tab(url, true, window, cx);
                    }))
                    .child(div().w(px(16.)).flex_shrink_0())
                    .child(format!("Sign in to {name}"))
                    .into_any_element(),
            );
            rows.push(
                Self::live_note(
                    ("live-sign-in-note", index),
                    format!("Then your {} show up here", folder.integration.noun),
                    p,
                )
                .mt(px(-6.))
                .into_any_element(),
            );
            return;
        }
        let active_live = self.tabs[self.active].live.as_deref();
        for (s, section) in folder.sections.iter().enumerate() {
            if !section.shown {
                continue;
            }
            let fetching = self
                .fetch
                .as_ref()
                .is_some_and(|f| f.folder == index && f.section == s);
            let count = section.shown_items().count();
            let unseen = section.shown_items().filter(|(_, item)| !item.seen).count();
            // Collapsed, the dots are hidden: the section shows how many are new instead.
            let badge = !section.open && unseen > 0;
            rows.push(
                Self::live_row(("live-section", index * 8 + s), 26.)
                    .hover(move |s| s.bg(rgb(p.surface)))
                    .on_hover(Self::hint_on_hover("Click to show or hide", cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(section) = this
                            .live
                            .get_mut(index)
                            .and_then(|folder| folder.sections.get_mut(s))
                        {
                            section.open = !section.open;
                            cx.notify();
                        }
                    }))
                    // 16 pt wide, the folder icon's slot: labels line up with the folder name.
                    .child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(Self::chevron(section.open, p)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(p.muted))
                            .child(section.label),
                    )
                    .when(badge, |row| row.child(Self::unseen_badge(unseen, p)))
                    .when(!badge && (section.last.is_some() || count > 0), |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(rgb(p.muted))
                                .opacity(if fetching { 0.5 } else { 1. })
                                .font(tabular_font(FontWeight::NORMAL))
                                .child(count.to_string()),
                        )
                    })
                    .into_any_element(),
            );
            if !section.open {
                continue;
            }
            let id = index * 8 + s;
            if let Some(error) = &section.error {
                rows.push(
                    Self::live_note(("live-error", id), error.clone(), p)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.refresh_live(index, cx)))
                        .into_any_element(),
                );
            } else if section.last.is_none() && count == 0 {
                rows.push(Self::live_note(("live-loading", id), "Loading…", p).into_any_element());
            } else if count == 0 {
                rows.push(
                    Self::live_note(("live-empty", id), "Nothing here", p).into_any_element(),
                );
            }
            for (n, item) in section.shown_items() {
                items.entry(item.url.clone()).or_insert(rows.len());
                rows.push(self.live_item(
                    (index, s, n),
                    item,
                    active_live == Some(item.url.as_str()),
                    p,
                    cx,
                ));
            }
        }
    }
    /// A live folder item, drawn like a tab: selected while its tab shows, × to close it,
    /// which keeps it away until it is updated.
    fn live_item(
        &self,
        at: (usize, usize, usize),
        item: &live::Item,
        selected: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (folder, section, n) = at;
        let status = if item.detail.is_empty() {
            item.title.clone()
        } else {
            format!("{} · {}", item.title, item.detail)
        };
        let url = item.url.clone();
        let hovered = self.hovered_live.as_deref() == Some(item.url.as_str());
        let loading = self
            .live_tab(&item.url)
            .is_some_and(|index| self.tabs[index].loading);
        Self::live_row(
            ("live-item", (folder * 8 + section) * live::MAX_ITEMS + n),
            30.,
        )
        .pr_1()
        .border_1()
        .border_color(rgb(if selected { p.border } else { p.background }))
        .bg(rgb(if selected { p.elevated } else { p.background }))
        .when(!selected, |row| {
            row.hover(move |s| s.bg(rgb(p.surface)).border_color(rgb(p.surface)))
        })
        .on_hover(cx.listener({
            let url = url.clone();
            move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.live_hover = Some(status.clone());
                    this.hovered_live = Some(url.clone());
                } else if this.hovered_live.as_ref() == Some(&url) {
                    this.live_hover = None;
                    this.hovered_live = None;
                }
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_live_item(at, window, cx);
        }))
        // New items carry a dot where the section has its chevron.
        .child(
            div()
                .w(px(16.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .when(!item.seen, |slot| {
                    slot.child(div().size(px(6.)).rounded_full().bg(rgb(p.primary)))
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .opacity(if loading { 0.6 } else { 1. })
                .child(item.title.clone()),
        )
        .when(hovered || selected, |row| {
            row.child(
                Self::row_control(
                    (
                        "live-close",
                        ((folder * 8 + section) * live::MAX_ITEMS + n) as u64,
                    ),
                    p,
                )
                .on_hover(Self::hint_on_hover(
                    "Close · it comes back when it is updated",
                    cx,
                ))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.hovered_live = None;
                    this.live_hover = None;
                    this.close_live_item(&url, window, cx);
                }))
                .child("×"),
            )
        })
        .into_any_element()
    }
    fn live_folder_row(
        &self,
        index: usize,
        folder: &live::LiveFolder,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = folder.checking() || self.fetch.as_ref().is_some_and(|f| f.folder == index);
        let unseen = folder.unseen();
        div()
            .id(("live", index))
            .h(px(30.))
            .w_full()
            .flex_shrink_0()
            .px_2()
            .rounded_lg()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(p.surface)))
            .on_hover(Self::hint_on_hover(
                "Live folder · click to show or hide, right-click to refresh or remove",
                cx,
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(folder) = this.live.get_mut(index) {
                    folder.open = !folder.open;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.menu = None;
                    this.live_menu = Some((index, event.position));
                    window.focus(&this.menu_focus);
                    cx.notify();
                }),
            )
            .child(Self::chevron(folder.open, p))
            .child(
                brand::icon(folder.integration.icon)
                    .size(px(16.))
                    .flex_shrink_0()
                    .text_color(rgb(p.text))
                    // Dimmed while refreshing, like a loading pinned tile.
                    .opacity(if busy { 0.5 } else { 1. }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_sm()
                    .child(folder.integration.name),
            )
            .when(unseen > 0, |row| row.child(Self::unseen_badge(unseen, p)))
            .into_any_element()
    }
    /// The bright count of new or updated items on a folder or a collapsed section.
    fn unseen_badge(unseen: usize, p: Palette) -> Div {
        div()
            .min_w(px(18.))
            .h(px(18.))
            .px(px(5.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .font(tabular_font(FontWeight::SEMIBOLD))
            .bg(rgb(p.primary))
            .text_color(rgb(p.background))
            .child(unseen.to_string())
    }
    /// Right-click menu for a live folder, kept inside the sidebar like the tab menu.
    fn live_menu_view(
        &self,
        (index, at): (usize, Point<Pixels>),
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(folder) = self.live.get(index) else {
            return div().into_any_element();
        };
        let full = cx.global::<ThemeSettings>().sidebar_width;
        let width = (full - 16.).min(220.);
        let x = f32::from(at.x).clamp(8., (full - width - 8.).max(8.));
        let body = div()
            .id("live-menu")
            .track_focus(&self.menu_focus)
            .w(px(width))
            .p_1()
            .rounded_lg()
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.border))
            .shadow_lg()
            .flex()
            .flex_col()
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close_menu(window, cx);
                }
            }))
            .child(
                menu_item("live-refresh", "Refresh", None, p).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.close_menu(window, cx);
                        this.refresh_live(index, cx);
                    },
                )),
            )
            .when(folder.unseen() > 0, |menu| {
                menu.child(
                    menu_item("live-mark-seen", "Mark All as Seen", None, p).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.close_menu(window, cx);
                            if let Some(folder) = this.live.get_mut(index) {
                                folder
                                    .sections
                                    .iter_mut()
                                    .flat_map(|section| &mut section.items)
                                    .for_each(|item| item.seen = true);
                            }
                        },
                    )),
                )
            })
            .child(menu_separator(p))
            .child(
                menu_item(
                    "live-remove",
                    format!("Remove {} Folder", folder.integration.name),
                    None,
                    p,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.close_menu(window, cx);
                    this.remove_live(index, cx);
                })),
            );
        deferred(
            anchored()
                .position(point(px(x), at.y))
                .snap_to_window_with_margin(px(8.))
                .child(body),
        )
        .with_priority(1)
        .into_any_element()
    }
    /// Arc-style vertical tabs: pinned tiles, then New tab and the other tabs.
    fn sidebar_view(&self, p: Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let pinned = self.pinned_count();
        let rows = self.rows();
        // Dragging a tab with nothing pinned yet offers a place to pin it.
        let pin_zone = pinned == 0 && self.dragging_tab && cx.has_active_drag();
        div()
            .id("sidebar")
            .relative()
            .w(px(cx.global::<ThemeSettings>().sidebar_width))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(p.border))
            .child(
                // Leaves room for the traffic lights, level with the toolbar.
                div()
                    .h(px(CHROME_ROW))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px_3()
                    .child(self.sidebar_button(p, cx)),
            )
            .child(
                div()
                    .id("tab-list")
                    .track_scroll(&self.tab_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(pinned > 0 || pin_zone, |list| {
                        // Dropping on the grid itself, not on a tile, pins at the end.
                        list.child(
                            div()
                                .id("pinned-grid")
                                .min_h(px(46.))
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .mb_2()
                                .rounded_xl()
                                .border_1()
                                .border_color(rgb(p.background))
                                .drag_over::<DraggedTab>(move |style, _, _, _| {
                                    style.border_color(rgb(p.text))
                                })
                                .on_drag_move(cx.listener(clear_drop_hint))
                                .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                                    this.drop_tab(dragged.0, usize::MAX, true, cx);
                                }))
                                .when(pin_zone, |zone| {
                                    zone.justify_center()
                                        .border_color(rgb(p.border))
                                        .text_xs()
                                        .text_color(rgb(p.muted))
                                        .child("Drop here to pin")
                                })
                                .children((0..pinned).map(|index| self.pinned_tile(index, p, cx))),
                        )
                    })
                    .children(self.live_folders(p, cx))
                    .child(
                        // An action, not a tab: no fill or icon tile, quieter text, its shortcut.
                        // Dropping a tab here moves it to the top of the list (unpinning it).
                        div()
                            .id("sidebar-new-tab")
                            .border_1()
                            .border_color(rgb(p.background))
                            .drag_over::<DraggedTab>(move |style, _, _, _| {
                                style.border_color(rgb(p.text))
                            })
                            .on_drag_move(cx.listener(clear_drop_hint))
                            .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                                // 0 clamps to the first unpinned slot, counted after removal.
                                this.drop_tab(dragged.0, 0, false, cx);
                            }))
                            .h(px(30.))
                            .flex_shrink_0()
                            .px_2()
                            .rounded_lg()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(rgb(p.muted))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)).text_color(rgb(p.text)))
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.new_tab(&NewTab, window, cx)
                                }),
                            )
                            .child(
                                div()
                                    .w(px(18.))
                                    .flex()
                                    .justify_center()
                                    .text_sm()
                                    .child("+"),
                            )
                            .child(div().flex_1().child("New tab"))
                            .child("⌘T"),
                    )
                    .children(
                        rows.iter()
                            .enumerate()
                            .map(|(n, &row)| self.tab_row(row, n + 1 == rows.len(), p, cx)),
                    )
                    // Releasing in a gap between rows still lands where the line shows.
                    .on_drop(cx.listener(|this, dragged: &DraggedTab, window, cx| {
                        this.apply_drop(dragged.0, window, cx);
                    })),
            )
            .child(
                // The right edge drags to resize; the width is saved when the drag ends.
                div()
                    .id("sidebar-resize")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(5.))
                    .cursor_col_resize()
                    .hover(move |s| s.bg(rgb(p.border)))
                    .on_hover(Self::hint_on_hover("Drag to resize the sidebar", cx))
                    .on_drag(SidebarResize, {
                        let browser = cx.weak_entity();
                        move |_, _, _, cx| {
                            let _ = browser.update(cx, |this, _| this.dragging_tab = false);
                            cx.new(|_| DragPreview(None))
                        }
                    }),
            )
            .when_some(self.menu, |sidebar, menu| {
                sidebar.child(self.tab_menu(menu, p, cx))
            })
            .when_some(self.live_menu, |sidebar, menu| {
                sidebar.child(self.live_menu_view(menu, p, cx))
            })
    }
    /// The active tab's page, or the active split view's two pages side by side.
    fn content(&self, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(partner) = self.partner(self.active) else {
            return self.tab_content(self.active, cx);
        };
        let halves = [self.active.min(partner), self.active.max(partner)];
        div()
            .size_full()
            .flex()
            .gap(px(6.))
            .p(px(6.))
            .bg(rgb(p.background))
            .children(halves.map(|index| {
                let active = index == self.active;
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .rounded_lg()
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgb(if active { p.primary } else { p.border }))
                    // Native pages report their own clicks; this catches the GPUI New Tab page.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.activate_half(index, cx)),
                    )
                    .child(self.tab_content(index, cx))
            }))
            .into_any_element()
    }
    fn tab_content(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let tab = &self.tabs[index];
        if tab.home {
            return self.home_page(cx);
        }
        // While loading after a failure: a blank page, not the old error or the old document.
        match &tab.load_failure {
            Some(_) if tab.loading => {
                return div()
                    .size_full()
                    .bg(rgb(theme::palette(cx).background))
                    .into_any_element();
            }
            Some(failure) => return self.error_page(index, failure, cx),
            None => {}
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        let address_focused = self.address.read(cx).focus_handle(cx).is_focused(window);
        let display = cx.global::<ThemeSettings>().address;
        let sidebar_width = self.sidebar_width(cx);
        // A drag released outside every drop target leaves no line behind.
        if !cx.has_active_drag() {
            self.drop_hint = None;
        }
        if self
            .sidebar_anim
            .is_some_and(|start| start.elapsed() < SIDEBAR_SLIDE)
        {
            window.request_animation_frame();
        } else {
            self.sidebar_anim = None;
        }
        // The traffic lights cover the window's first 88 px; once the sidebar is narrower than
        // that, the toolbar carries the toggle just right of them.
        let toolbar_toggle = sidebar_width < 88.;
        let tab = &self.tabs[self.active];
        let show_chip = !tab.home && !address_focused && display != AddressDisplay::Url;
        let not_secure =
            !tab.home && tab.load_failure.is_none() && webview::has_certificate_exception(&tab.url);
        self.address
            .update(cx, |input, _| input.set_not_secure(not_secure));
        let back_enabled = tab.can_go_back();
        let forward_enabled = tab.can_go_forward();
        // A hovered sidebar row shows its full title and address, which the row truncates.
        let hovered_row = self
            .hovered_tab
            .and_then(|id| self.tabs.iter().find(|tab| tab.id == id))
            .filter(|tab| !tab.home)
            .map(|tab| {
                format!(
                    "{} · {}  —  Right-click to pin or show side by side",
                    tab.display_title(),
                    history::display_url(&tab.url)
                )
            });
        let drop_status = match self.drop_hint {
            Some(DropHint::Split(_)) => Some("Release to show side by side".to_owned()),
            _ => None,
        };
        let zoom = self.zoom_label();
        if zoom.as_ref().is_some_and(|(_, flash)| *flash < 1.) {
            window.request_animation_frame();
        }
        self.address.update(cx, |input, _| input.set_zoom(zoom));
        let status = drop_status
            .or(self.hint.map(str::to_owned))
            .or(self.live_hover.clone())
            .or(hovered_row)
            .unwrap_or_else(|| {
                tab.hovered_link
                    .as_deref()
                    .or(tab.error.as_deref())
                    .unwrap_or(if tab.loading && !tab.home {
                        "Loading · WebKit"
                    } else {
                        "WebKit · JavaScript enabled"
                    })
                    .to_owned()
            });
        div()
            .id("felinaut")
            .track_focus(&self.focus)
            .key_context("Browser")
            .size_full()
            .bg(rgb(p.background))
            .text_color(rgb(p.text))
            .font_family(".AppleSystemUIFont")
            .flex()
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::focus_address))
            .on_action(cx.listener(Self::back))
            .on_action(cx.listener(Self::forward))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::mute_tab))
            .on_action(cx.listener(Self::volume_controls))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::pin_tab))
            .on_action(cx.listener(Self::activate_tab))
            .on_action(cx.listener(Self::zoom))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::reopen_tab))
            .on_action(cx.listener(Self::new_live_folder))
            .on_action(cx.listener(Self::show_downloads))
            // Mouse drags stay with the GPUI view even over the native page, so the root
            // follows a resize anywhere in the window and saves the width when it ends.
            .on_drag_move(
                cx.listener(|_, event: &DragMoveEvent<SidebarResize>, _, cx| {
                    let width = f32::from(event.event.position.x)
                        .clamp(theme::SIDEBAR_MIN, theme::SIDEBAR_MAX);
                    cx.update_global::<ThemeSettings, _>(|settings, _| {
                        settings.sidebar_width = width;
                    });
                    cx.notify();
                }),
            )
            .on_drop(cx.listener(|_, _: &SidebarResize, _, cx| {
                cx.global::<ThemeSettings>().save();
            }))
            .when(sidebar_width > 0., |root| {
                // A clipping frame slides; the sidebar inside keeps its width, so nothing reflows.
                root.child(
                    div()
                        .w(px(sidebar_width))
                        .h_full()
                        .flex_shrink_0()
                        .overflow_hidden()
                        .child(self.sidebar_view(p, cx)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(CHROME_ROW))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .px_4()
                            .gap_2()
                            .bg(rgb(p.surface))
                            .when(toolbar_toggle, |bar| {
                                bar.pl(px((88. - sidebar_width).max(16.)))
                                    .child(self.sidebar_button(p, cx))
                            })
                            .child(
                                self.button("back", "←", p)
                                    .opacity(if back_enabled { 1. } else { 0.35 })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.back(&Back, window, cx)
                                    })),
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
                            .child(self.button("home", "⌂", p).on_click(
                                cx.listener(|this, _, window, cx| this.home(&Home, window, cx)),
                            ))
                            .child(div().flex_1().min_w_0().child(if show_chip {
                                self.title_chip(display, not_secure, p, cx)
                            } else {
                                self.address.clone().into_any_element()
                            }))
                            .child(download_panel::button(&self.downloads, p).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.show_downloads(&ShowDownloads, window, cx)
                                }),
                            ))
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
                            .child(self.content(p, cx)),
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
                                    .child(status),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .child(format!("FELINAUT  /  {DISPLAY_VERSION}")),
                            ),
                    ),
            )
    }
}
/// The popover draws as soon as its window opens and reads the browser while doing so, so it
/// must not be opened from inside one of the browser's own updates (click or action handlers).
fn open_volume(
    browser: WeakEntity<Browser>,
    anchor: Option<Pixels>,
    window: &mut Window,
    cx: &mut Context<Browser>,
) {
    window.defer(cx, move |window, cx| {
        volume::toggle(browser, anchor, window, cx)
    });
}

/// Links handed over by macOS (Felinaut as the default browser, `open -a Felinaut <url>`).
/// The bundle registers only http/https; `NativePage` rejects anything else. A launch by
/// link delivers the URLs before the window exists, so they wait in `pending`.
#[derive(Default)]
struct UrlHandoff {
    pending: Vec<String>,
    browser: Option<(WindowHandle<Browser>, AsyncApp)>,
}

fn open_links(window: WindowHandle<Browser>, cx: &mut impl AppContext, urls: Vec<String>) {
    let _ = window.update(cx, |browser, window, cx| {
        for url in urls {
            browser.open_link_tab(&url, true, window, cx);
        }
    });
}

fn main() {
    let handoff = Rc::new(RefCell::new(UrlHandoff::default()));
    let opener = handoff.clone();
    let app = Application::new().with_assets(brand::BrandAssets);
    app.on_open_urls(move |urls| {
        let mut handoff = opener.borrow_mut();
        match &mut handoff.browser {
            Some((window, cx)) => open_links(*window, cx, urls),
            None => handoff.pending.extend(urls),
        }
    });
    app.run(move |cx: &mut App| {
        cx.set_global(ThemeSettings::load());
        input::init(cx);
        #[cfg(target_os = "macos")]
        webview::forward_command_arrows();
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
            KeyBinding::new("cmd-s", ToggleSidebar, None),
            KeyBinding::new("cmd-d", PinTab, None),
            KeyBinding::new("cmd-shift-t", ReopenTab, None),
            KeyBinding::new("ctrl-tab", NextTab, None),
            KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
            KeyBinding::new("cmd-shift-]", NextTab, None),
            KeyBinding::new("cmd-shift-[", PreviousTab, None),
            KeyBinding::new("cmd-9", ActivateTab(LAST_TAB), None),
            KeyBinding::new("cmd-=", Zoom(1), None),
            KeyBinding::new("cmd-+", Zoom(1), None),
            KeyBinding::new("cmd--", Zoom(-1), None),
            KeyBinding::new("cmd-0", Zoom(0), None),
            KeyBinding::new("cmd-shift-j", ShowDownloads, None),
        ]);
        cx.bind_keys(
            (1..=8).map(|n| KeyBinding::new(&format!("cmd-{n}"), ActivateTab(n - 1), None)),
        );
        // Native menu equivalents remain available while WKWebView owns keyboard focus.
        cx.set_menus(vec![
            Menu {
                name: "Felinaut".into(),
                items: vec![
                    MenuItem::action("About Felinaut", AboutFelinaut),
                    MenuItem::separator(),
                    MenuItem::action("Quit Felinaut", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Tab", NewTab),
                    MenuItem::action("Close Tab", CloseTab),
                    MenuItem::action("Reopen Closed Tab", ReopenTab),
                    MenuItem::action("Pin / Unpin Tab", PinTab),
                    MenuItem::separator(),
                    MenuItem::action("New Live Folder…", NewLiveFolder),
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
                name: "View".into(),
                // Menu items keep these working while the page has keyboard focus.
                items: [
                    MenuItem::action("Toggle Sidebar", ToggleSidebar),
                    MenuItem::action("Downloads", ShowDownloads),
                    MenuItem::separator(),
                    MenuItem::action("Show Next Tab", NextTab),
                    MenuItem::action("Show Previous Tab", PreviousTab),
                    MenuItem::separator(),
                ]
                .into_iter()
                .chain((1..=8).map(|n| MenuItem::action(format!("Tab {n}"), ActivateTab(n - 1))))
                .chain([
                    MenuItem::action("Last Tab", ActivateTab(LAST_TAB)),
                    MenuItem::separator(),
                    MenuItem::action("Zoom In", Zoom(1)),
                    MenuItem::action("Zoom Out", Zoom(-1)),
                    MenuItem::action("Actual Size", Zoom(0)),
                ])
                .collect(),
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
        cx.on_action(|_: &AboutFelinaut, cx| about::open(cx));
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(900.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1000.), px(700.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Felinaut".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(16.), px(TRAFFIC_LIGHT_Y))),
                    }),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Browser::new(window, cx)),
            )
            .expect("Could not open Felinaut window");
        let mut handoff = handoff.borrow_mut();
        handoff.browser = Some((window, cx.to_async()));
        open_links(window, cx, std::mem::take(&mut handoff.pending));
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        FETCH_TIMEOUT, drop_index, live_refresh_can_poll, restore_tabs, session::SavedTab,
    };
    use std::time::{Duration, Instant};

    fn failed(url: &str) -> super::LoadFailure {
        super::load_error::failure("NSURLErrorDomain", -1003, url, "Not found").unwrap()
    }

    #[test]
    fn failures_stop_loading_and_stay_until_a_new_page_commits() {
        let mut tab = super::Tab::new(1);
        tab.start_load("https://typed.example/".into());
        // A failure without a URL retries the one the tab was loading.
        tab.show_failure(failed(""));
        assert!(!tab.loading);
        assert_eq!(tab.title, "Can't find this site");
        assert_eq!(
            tab.load_failure.as_ref().unwrap().url,
            "https://typed.example/"
        );
        // Loading another address keeps the failure, so the old page stays hidden.
        tab.start_load("https://expired.badssl.com/".into());
        assert!(tab.loading && tab.load_failure.is_some());
        assert_eq!(tab.title, "expired.badssl.com");
        tab.show_failure(failed("https://expired.badssl.com/"));
        assert_eq!(tab.url, "https://expired.badssl.com/");
    }

    #[test]
    fn rename_keeps_existing_profile_files_and_settings_together() {
        let profile = std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join("Library/Application Support/Felifox"));
        assert_eq!(
            super::history::default_path(),
            profile.as_ref().map(|p| p.join("history/visits.tsv"))
        );
        assert_eq!(
            super::session::default_path(),
            profile.as_ref().map(|p| p.join("tabs.tsv"))
        );
        assert_eq!(
            super::live::default_path(),
            profile.as_ref().map(|p| p.join("live.tsv"))
        );
        assert_eq!(
            super::theme::settings_path(),
            profile.as_ref().map(|p| p.join("settings"))
        );
    }

    #[test]
    fn live_refresh_polls_through_navigation_until_its_deadline() {
        let started = Instant::now();
        let mut next = started;
        // Loading/departing documents may consume polls without producing a list.
        // A final page ready after the old 15-attempt cap must still be evaluated.
        for second in 0..20 {
            let now = started + Duration::from_secs(second);
            assert!(live_refresh_can_poll(started, next, now));
            next = now + Duration::from_secs(1);
            assert!(!live_refresh_can_poll(started, next, now));
        }
        let deadline = started + FETCH_TIMEOUT;
        assert!(!live_refresh_can_poll(started, started, deadline));
        assert!(!live_refresh_can_poll(
            started,
            started,
            deadline + Duration::from_secs(1)
        ));
    }

    fn saved(url: &str, pinned: bool, split: bool, active: bool) -> SavedTab {
        SavedTab {
            url: url.into(),
            title: String::new(),
            pinned,
            split,
            active,
        }
    }

    #[test]
    fn restores_order_splits_and_active_tab() {
        // A hand-edited file: a pin after a tab, a split flag on a pin and on a first tab.
        let (tabs, active) = restore_tabs(vec![
            saved("https://a/", false, true, false),
            saved("https://b/", false, true, true),
            saved("https://p/", true, true, false),
            saved("https://c/", false, true, false),
        ]);
        let urls: Vec<_> = tabs.iter().map(|tab| tab.url.as_str()).collect();
        assert_eq!(
            urls,
            ["https://p/", "https://a/", "https://b/", "https://c/"]
        );
        assert!(tabs[0].pinned.is_some() && tabs[1..].iter().all(|tab| tab.pinned.is_none()));
        // a+b pair up; c's split flag has no free left half.
        assert_eq!(tabs[1].split, Some(tabs[2].id));
        assert_eq!(tabs[2].split, Some(tabs[1].id));
        assert_eq!((tabs[0].split, tabs[3].split), (None, None));
        assert_eq!(active, 2);
        let mut ids: Vec<_> = tabs.iter().map(|tab| tab.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), tabs.len());
    }

    #[test]
    fn opens_a_new_tab_when_the_active_one_was_blank() {
        let (tabs, active) = restore_tabs(vec![saved("https://p/", true, false, false)]);
        assert_eq!(active, 1);
        assert!(tabs[1].home && tabs[1].id == 2);
        let (tabs, active) = restore_tabs(Vec::new());
        assert_eq!((tabs.len(), active), (1, 0));
    }

    /// Replays `drop_tab` on (id, pinned) pairs: remove, then insert at `drop_index`.
    fn drop(tabs: &[(u8, bool)], from: usize, at: usize, pin: bool) -> Vec<(u8, bool)> {
        let mut tabs = tabs.to_vec();
        let (id, _) = tabs.remove(from);
        let pinned = tabs.iter().take_while(|(_, p)| *p).count();
        let to = drop_index(at, from, pin, pinned, tabs.len());
        tabs.insert(to, (id, pin));
        tabs
    }

    #[test]
    fn lands_where_the_line_shows() {
        let tabs = [(1, true), (2, true), (3, false), (4, false), (5, false)];
        // The line below tab 5 (`at` = 5) and above tab 3 (`at` = 2).
        assert_eq!(
            drop(&tabs, 2, 5, false),
            [(1, true), (2, true), (4, false), (5, false), (3, false)]
        );
        assert_eq!(
            drop(&tabs, 4, 2, false),
            [(1, true), (2, true), (5, false), (3, false), (4, false)]
        );
        // Dropping just above or below itself changes nothing.
        assert_eq!(drop(&tabs, 3, 3, false), tabs);
        assert_eq!(drop(&tabs, 3, 4, false), tabs);
    }

    #[test]
    fn pins_and_unpins_across_groups() {
        let tabs = [(1, true), (2, true), (3, false), (4, false), (5, false)];
        // Onto the first tile pins before it; the grid itself pins at the end.
        assert_eq!(
            drop(&tabs, 4, 0, true),
            [(5, true), (1, true), (2, true), (3, false), (4, false)]
        );
        assert_eq!(
            drop(&tabs, 3, usize::MAX, true),
            [(1, true), (2, true), (4, true), (3, false), (5, false)]
        );
        // A pinned tab dropped below tab 4 unpins there; New tab (`at` = 0) puts it on top.
        assert_eq!(
            drop(&tabs, 0, 4, false),
            [(2, true), (3, false), (4, false), (1, false), (5, false)]
        );
        assert_eq!(
            drop(&tabs, 0, 0, false),
            [(2, true), (1, false), (3, false), (4, false), (5, false)]
        );
    }
}
