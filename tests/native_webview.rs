//! Opt-in native integration test. WKWebView requires macOS's main thread.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_webview

#[allow(dead_code, unused_imports)]
#[path = "../src/audio.rs"]
mod audio;
#[allow(dead_code, unused_imports)]
#[path = "../src/auth_prompt.rs"]
mod auth_prompt;
#[allow(dead_code, unused_imports)]
#[path = "../src/downloads.rs"]
mod downloads;
// `theme.rs` needs the search engine and the website language.
#[allow(dead_code, unused_imports)]
#[path = "../src/engine.rs"]
mod engine;
#[allow(dead_code, unused_imports)]
#[path = "../src/http_auth.rs"]
mod http_auth;
#[allow(dead_code, unused_imports)]
#[path = "../src/icons.rs"]
mod icons;
#[allow(dead_code, unused_imports)]
#[path = "../src/identity.rs"]
mod identity;
#[allow(dead_code, unused_imports)]
#[path = "../src/input.rs"]
mod input;
#[allow(dead_code, unused_imports)]
#[path = "../src/language.rs"]
mod language;
#[allow(dead_code, unused_imports)]
#[path = "../src/load_error.rs"]
mod load_error;
#[allow(dead_code)]
#[path = "../src/swipe.rs"]
mod swipe;
#[allow(dead_code, unused_imports)]
#[path = "../src/theme.rs"]
mod theme;
#[allow(dead_code)]
#[path = "../src/webview.rs"]
mod webview;

use gpui::{prelude::*, *};

// `input.rs` sends this action from the Not secure mark of the address bar.
actions!(felinaut, [ExplainNotSecure]);
use std::{
    cell::Cell,
    io::{Read, Write},
    net::TcpListener,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use webview::{Event, NativePage};

struct TestWindow {
    page: Rc<NativePage>,
    /// The focus of the browser itself, and a stand-in for the address bar.
    browser: FocusHandle,
    address: FocusHandle,
    _second_tab: NativePage,
    _failed_tab: Rc<NativePage>,
    _hidden: NativePage,
    _hidden_auth: NativePage,
}
impl Render for TestWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .size_full()
            .track_focus(&self.browser)
            .child(div().track_focus(&self.address));
        // As in the browser: the sign-in prompt takes the place of the page.
        if let Some(prompt) = auth_prompt::view(&self.page, cx) {
            return root.child(prompt);
        }
        let page = self.page.clone();
        root.child(
            canvas(
                move |bounds, _, _| {
                    page.set_bounds(bounds).expect("native bounds");
                    page.set_visible(true).expect("native visibility");
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    }
}

/// Shows the prompt for the page's open question, then presses the keys as a user does.
async fn sign_in_keys(
    page: &NativePage,
    browser: &FocusHandle,
    keys: &[&str],
    cx: &mut AsyncWindowContext,
) {
    cx.update(|window, cx| {
        assert!(
            auth_prompt::sync(page, true, browser, window, cx),
            "a new question changes the tab"
        );
        assert!(auth_prompt::view(page, cx).is_some());
        // As the browser does for the active tab.
        auth_prompt::focus(page, window, cx);
        window.refresh();
    })
    .unwrap();
    // Typed text goes to the field that had
    // the focus at the last paint. So a frame must pass at the start and after each Tab.
    let mut wait = true;
    for key in keys {
        if wait {
            Timer::after(Duration::from_millis(250)).await;
        }
        wait = *key == "tab";
        cx.update(|window, cx| window.dispatch_keystroke(Keystroke::parse(key).unwrap(), cx))
            .unwrap();
    }
}

/// The question was answered: the prompt goes away and the page comes back with the keys.
fn prompt_closed(page: &NativePage, browser: &FocusHandle, cx: &mut AsyncWindowContext) {
    cx.update(|window, cx| {
        assert!(
            auth_prompt::sync(page, true, browser, window, cx),
            "an answered question changes the tab"
        );
        assert!(auth_prompt::view(page, cx).is_none());
        assert!(page.has_keys_for_test(), "the page takes the keys back");
        window.refresh();
    })
    .unwrap();
}

/// Page scripts that notice, and would break, a find bar that edits the selection from the
/// page's own JavaScript world.
const FIND_OVERRIDES: &str = "<script>Selection.prototype.collapseToStart = Selection.prototype.removeAllRanges = function () { top.findTouched = true; };</script>";

/// Reads `frame-or-element-id:text` of the selection in the page or its frames, or "".
const SELECTION: &str = "(() => { for (const w of [window, ...Array.from(document.querySelectorAll('iframe'), f => f.contentWindow)]) { const s = w.getSelection(); if (s && s.rangeCount && !s.isCollapsed) return s.anchorNode.parentElement.id + ':' + s; } return ''; })()";

/// Runs `page.find`, then returns whether it matched and what is selected.
async fn find_step(page: &NativePage, text: &str, backwards: bool) -> (bool, String) {
    page.find(text, backwards);
    (wait_found(page).await, read(page, SELECTION).await)
}

async fn wait_found(page: &NativePage) -> bool {
    let start = Instant::now();
    loop {
        Timer::after(Duration::from_millis(20)).await;
        if let Some(found) = page
            .drain_events()
            .into_iter()
            .find_map(|event| match event {
                Event::Found(found) => Some(found),
                _ => None,
            })
        {
            return found;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "no find result");
    }
}

async fn read(page: &NativePage, script: &str) -> String {
    let (sender, receiver) = std::sync::mpsc::channel();
    page.evaluate(script, move |value| {
        let _ = sender.send(value);
    })
    .unwrap();
    let start = Instant::now();
    loop {
        Timer::after(Duration::from_millis(20)).await;
        if let Ok(value) = receiver.try_recv() {
            return value.trim_matches('"').to_string();
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "no readback of {script:?}"
        );
    }
}

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST")
        .or_else(|| std::env::var_os("FELIFOX_NATIVE_TEST"))
        .as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        println!("Native webview test skipped; set FELINAUT_NATIVE_TEST=1 to run.");
        return;
    }
    // Downloads go to `$HOME/Downloads`: give the test a home of its own, before any thread
    // starts, so nothing lands in the real one.
    let home = std::env::temp_dir().join(format!("felinaut-native-{}", std::process::id()));
    let folder = home.join("Downloads");
    std::fs::create_dir_all(&folder).expect("temporary downloads folder");
    // SAFETY: the process has only this thread so far.
    unsafe { std::env::set_var("HOME", &home) };
    // All fixture traffic stays on loopback; exercise the production, URL-only link bridge.
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let download_requests = Arc::new(AtomicUsize::new(0));
    let served_downloads = download_requests.clone();
    let auth_requests = Arc::new(AtomicUsize::new(0));
    let served_auth = auth_requests.clone();
    let cross_requests = Arc::new(AtomicUsize::new(0));
    let served_cross = cross_requests.clone();
    let image_requests = Arc::new(AtomicUsize::new(0));
    let served_images = image_requests.clone();
    let fixture_port = listener.local_addr().unwrap().port();
    // `(source, user agent)`: the User-Agent header of every request, keyed by its path, and
    // `navigator.userAgent` as the page reports it.
    let user_agents = Arc::new(Mutex::new(Vec::new()));
    let seen_agents = user_agents.clone();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else {
                break;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let Ok(count) = stream.read(&mut request) else {
                continue;
            };
            let request = String::from_utf8_lossy(&request[..count]);
            let path = request
                .strip_prefix("GET ")
                .and_then(|rest| rest.split(['?', ' ']).next())
                .unwrap_or("?")
                .to_string();
            let header = request
                .lines()
                .find_map(|line| line.strip_prefix("User-Agent: "))
                .map(|agent| (path, agent.to_string()));
            let reported = request
                .strip_prefix("GET /agent?")
                .and_then(|rest| rest.split(' ').next())
                .map(|query| ("navigator.userAgent".to_string(), query.replace("%20", " ")));
            seen_agents
                .lock()
                .unwrap()
                .extend(header.into_iter().chain(reported));
            if request.starts_with("GET /download ") {
                // The server asks for a name outside the downloads folder.
                served_downloads.fetch_add(1, Ordering::SeqCst);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"../../fixture.bin\"\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture");
                continue;
            }
            if request.starts_with("GET /embed ") {
                // The page is on 127.0.0.1 and its image is on localhost: another origin.
                let body = format!(
                    "<title>EMBED</title><img src='http://localhost:{fixture_port}/cross'>"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                continue;
            }
            // Find fixtures: the page replaces the Selection methods that the find bar relies on,
            // and a same-origin frame holds words that appear nowhere else.
            let fixture = if request.starts_with("GET /find ") {
                Some(format!(
                    "<p id=m1>tap</p><p id=m2>tab</p><p id=m3>table</p><iframe src='/frame'></iframe>{FIND_OVERRIDES}"
                ))
            } else if request.starts_with("GET /frame ") {
                Some(format!(
                    "<p id=f1>zebra</p><p id=f2>zebu</p><p id=f3>zebus</p>{FIND_OVERRIDES}"
                ))
            } else {
                None
            };
            if let Some(body) = fixture {
                let body = format!("<!doctype html><meta charset=utf-8><title>Find</title>{body}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                continue;
            }
            if request.starts_with("GET /cross ") {
                served_cross.fetch_add(1, Ordering::SeqCst);
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Session expired\"\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
                continue;
            }
            if request.starts_with("GET /auth-image ") {
                // A protected image of the same origin, on the 401 page of `/auth-both`.
                served_images.fetch_add(1, Ordering::SeqCst);
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Image\"\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
                continue;
            }
            if request.starts_with("GET /auth") {
                // Two protected areas. WebKit reads the header as Latin-1, so the three bytes of
                // U+202E arrive as "â", the control character U+0080 and "®".
                served_auth.fetch_add(1, Ordering::SeqCst);
                let other = request.starts_with("GET /auth-other ");
                // This area offers two methods. After a refusal of the first one, WebKit asks
                // for the second one.
                let both = request.starts_with("GET /auth-both ");
                let response = if !other
                    && !both
                    && request.contains("\r\nAuthorization: Basic dXNlcjpwYXNz\r\n")
                {
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 22\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n<title>AUTH OK</title>".to_string()
                } else {
                    let realm = if other {
                        "Other"
                    } else if both {
                        "Both"
                    } else {
                        "Fixture\u{202e} realm"
                    };
                    let digest = if both {
                        "WWW-Authenticate: Digest realm=\"Both\", nonce=\"00\", qop=\"auth\"\r\n"
                    } else {
                        ""
                    };
                    // After a Cancel, the 401 page of this area loads a frame, and then an
                    // image that asks again. Neither may open a new question.
                    let body = if both {
                        "<title>AUTH NEEDED</title><iframe src='/second' onload=\"new Image().src='/auth-image'\"></iframe>"
                    } else {
                        "<title>AUTH NEEDED</title>"
                    };
                    format!(
                        "HTTP/1.1 401 Unauthorized\r\n{digest}WWW-Authenticate: Basic realm=\"{realm}\"\r\nContent-Type: text/html\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = stream.write_all(response.as_bytes());
                continue;
            }
            let second = request.starts_with("GET /second ");
            let script = if second {
                "document.title = localStorage.getItem('felifox-smoke') === 'yes' ? 'JS SECOND' : 'STORAGE FAILED';"
            } else {
                "fetch('/agent?' + navigator.userAgent); localStorage.setItem('felifox-smoke', 'yes'); document.title = 'JS FIRST';"
            };
            let body = format!(
                "<!doctype html><meta charset=utf-8><title>Waiting</title><style>body{{background:#edf0ff;color:#101324}}</style><h1 id=result>JavaScript test</h1><a id=testlink href='/link-destination'><span id=linkchild>Test link</span></a><iframe src='/frame'></iframe><script>{script}document.querySelector('#result').textContent = document.title;</script>"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    Application::new().run(move |cx| {
        cx.set_global(theme::ThemeSettings::default());
        input::init(cx);
        webview::forward_page_keys();
        let first_url = format!("{base}/first");
        let second_url = format!("{base}/second");
        let download_url = format!("{base}/download");
        let auth_url = format!("{base}/auth");
        let embed_url = format!("{base}/embed");
        let other_auth_url = format!("{base}/auth-other");
        let both_auth_url = format!("{base}/auth-both");
        let port = url::Url::parse(&base).unwrap().port().unwrap();
        let auth_requests = auth_requests.clone();
        let cross_requests = cross_requests.clone();
        let image_requests = image_requests.clone();
        let find_url = format!("{base}/find");
        let folder = folder.clone();
        let download_requests = download_requests.clone();
        let user_agents = user_agents.clone();
        let handle = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(800.), px(600.)), cx))),
                ..Default::default()
            },
            |window, cx| {
                let page = Rc::new(NativePage::new(window, &first_url).expect("create native page"));
                let second_tab = NativePage::new(window, "about:blank").expect("create inactive tab");
                second_tab.set_visible(false).unwrap();
                // A first load whose host never resolves (.invalid is reserved) leaves WKWebView
                // without a URL; polling such a tab must not abort the app.
                let failed_tab = Rc::new(NativePage::new(window, "http://felifox.invalid/").expect("create failing tab"));
                let failed_page = failed_tab.clone();
                // A page the user never sees asks for the same download; it must be refused.
                let hidden = NativePage::hidden(window, &download_url).expect("create hidden page");
                // A page the user never sees meets a sign-in challenge; it must not ask.
                let hidden_auth = NativePage::hidden(window, &auth_url).expect("create hidden sign-in page");
                // Tabs keep the in-app inspector, but Safari's Develop menu can't attach to them;
                // the hidden live-folder page has no inspector at all.
                for tab in [&*page, &second_tab, &*failed_tab] {
                    assert_eq!(tab.inspectable_for_test(), Some(false), "a tab must not be inspectable from Safari");
                    assert!(tab.developer_extras_for_test(), "tabs keep Inspect Element");
                    assert!(tab.container_for_test().is_some_and(|c| c.holds_page), "each tab page sits in its own container");
                }
                assert!(!hidden.developer_extras_for_test(), "the hidden live-folder page has developer extras off");
                assert_eq!(hidden.inspectable_for_test(), Some(false));
                assert!(hidden.container_for_test().is_none(), "the hidden page needs no container");
                page.focus().unwrap();
                page.focus_parent().unwrap();
                let second_page = page.clone();
                let browser = cx.focus_handle();
                let address = cx.focus_handle();
                let (window_browser, window_address) = (browser.clone(), address.clone());
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        let start = Instant::now();
                        let mut stage = 0;
                        let mut title = String::new();
                        let mut hover_seen = false;
                        let mut hover_cleared = false;
                        let mut background_opens = 0;
                        let mut foreground_open = false;
                        let mut failure = None;
                        let mut page_failed = false;
                        let mut downloads = Vec::new();
                        let key = second_page.key().expect("a visible page has a key");
                        let mut closing = None;
                        let mut found = Vec::new();
                        loop {
                            Timer::after(Duration::from_millis(50)).await;
                            downloads.extend(downloads::native::drain());
                            for event in failed_page.drain_events() {
                                if let Event::Failed(value) = event {
                                    failure = Some(value);
                                }
                            }
                            let mut finished = false;
                            for event in second_page.drain_events() {
                                match event {
                                    Event::Title(value) => title = value,
                                    Event::Finished(_) => finished = true,
                                    Event::Failed(_) => page_failed = true,
                                    Event::Hover(Some(url)) if url.ends_with("/link-destination") => hover_seen = true,
                                    Event::Hover(None) => hover_cleared = true,
                                    Event::OpenInBackground(url) if url.ends_with("/link-destination") => background_opens += 1,
                                    Event::Open(url) if url.ends_with("/link-destination") => foreground_open = true,
                                    Event::Found(value) => found.push(value),
                                    _ => {},
                                }
                            }
                            assert!(start.elapsed() < Duration::from_secs(40), "native test timed out at stage {stage}; title={title:?}");
                            if (stage <= 4 || stage == 8) && !finished { continue; }
                            if stage == 5 && (title != "LINK EVENTS OK" || failure.is_none()) { continue; }
                            let downloaded = downloads.iter().any(|event| matches!(event, downloads::Event::Finished { .. }));
                            if stage == 6 && !(downloaded && download_requests.load(Ordering::SeqCst) >= 2) { continue; }
                            if stage == 7 && found.len() < 3 { continue; }
                            if stage == 9 && (title != "EMBED" || cross_requests.load(Ordering::SeqCst) == 0) { continue; }
                            // The id of the question each sign-in stage waits for.
                            let question = http_auth::waiting(key).map(|(id, _)| id);
                            if stage == 10 && (question != Some(1) || auth_requests.load(Ordering::SeqCst) < 2) { continue; }
                            if stage == 11 && title != "AUTH NEEDED" { continue; }
                            if (stage == 12 && question != Some(2)) || (stage == 13 && question != Some(3)) { continue; }
                            if stage == 14 && title != "AUTH OK" { continue; }
                            let closing_key = closing.as_ref().and_then(NativePage::key);
                            if (stage == 15 || stage == 17) && question.is_none() { continue; }
                            if stage == 16 && (title != "AUTH NEEDED" || image_requests.load(Ordering::SeqCst) == 0) { continue; }
                            if stage == 18 && title != "JS FIRST" { continue; }
                            if (stage == 19 || stage == 21) && question.is_none() { continue; }
                            if stage == 20 && title != "AUTH NEEDED" { continue; }
                            if stage == 22 && !page_failed { continue; }
                            if stage == 23 && closing_key.and_then(http_auth::waiting).is_none() { continue; }
                            match stage {
                                0 => {
                                    assert_eq!(title, "JS FIRST", "JavaScript and DOM title update");
                                    // The policy hook tells a frame apart from the main frame and still lets the page load.
                                    assert!(webview::subframe_decisions() >= 1, "frame navigations reach the policy as frames");
                                    second_page.navigate(&second_url).unwrap();
                                }
                                1 => {
                                    assert_eq!(title, "JS SECOND", "JavaScript and localStorage");
                                    assert!(second_page.can_go_back());
                                    second_page.back().unwrap();
                                }
                                2 => {
                                    assert_eq!(title, "JS FIRST");
                                    assert!(second_page.can_go_forward());
                                    second_page.forward().unwrap();
                                }
                                3 => {
                                    assert_eq!(title, "JS SECOND");
                                    second_page.reload().unwrap();
                                }
                                4 => {
                                    assert_eq!(title, "JS SECOND");
                                    // Visibility and bounds act on the tab's container, which carries the page.
                                    second_page.set_visible(false).unwrap();
                                    let hidden_tab = second_page.container_for_test().unwrap();
                                    assert!(hidden_tab.hidden && !hidden_tab.page_hidden, "{hidden_tab:?}");
                                    second_page.set_bounds(Bounds::new(point(px(10.), px(20.)), size(px(640.), px(400.)))).unwrap();
                                    let moved = second_page.container_for_test().unwrap();
                                    let top = if moved.parent_flipped { 20. } else { moved.parent_height - 20. - 400. };
                                    assert_eq!(moved.frame, (10., top, 640., 400.), "{moved:?}");
                                    assert!(moved.holds_page && moved.page_fills, "{moved:?}");
                                    second_page.set_visible(true).unwrap();
                                    let shown = second_page.container_for_test().unwrap();
                                    assert!(!shown.hidden && !shown.page_hidden, "{shown:?}");
                                    second_page.evaluate_for_test(r#"
                                        (() => {
                                          const child = document.getElementById('linkchild');
                                          child.dispatchEvent(new PointerEvent('pointerover', { bubbles: true, composed: true }));
                                          const click = new MouseEvent('click', { bubbles: true, cancelable: true, metaKey: true, button: 0 });
                                          child.dispatchEvent(click);
                                          const foreground = new MouseEvent('click', { bubbles: true, cancelable: true, metaKey: true, shiftKey: true, button: 0 });
                                          child.dispatchEvent(foreground);
                                          const middle = new MouseEvent('auxclick', { bubbles: true, cancelable: true, button: 1 });
                                          child.dispatchEvent(middle);
                                          const menu = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 40, clientY: 80 });
                                          child.dispatchEvent(menu);
                                          const shown = !!document.querySelector('[data-felifox-link-menu]');
                                          window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
                                          const dismissed = !document.querySelector('[data-felifox-link-menu]');
                                          child.dispatchEvent(new PointerEvent('pointerout', { bubbles: true, composed: true, relatedTarget: document.body }));
                                          document.title = click.defaultPrevented && foreground.defaultPrevented && middle.defaultPrevented && menu.defaultPrevented && shown && dismissed ? 'LINK EVENTS OK' : 'LINK EVENTS FAILED';
                                        })();
                                    "#).unwrap();
                                }
                                5 => {
                                    assert!(hover_seen && hover_cleared, "link hover status must set and clear");
                                    assert_eq!(background_opens, 3, "Cmd-click, middle-click and context-menu item must each open one background tab");
                                    assert!(foreground_open, "Cmd-Shift-click opens a foreground tab");
                                    let failure = failure.take().unwrap();
                                    assert_eq!(failure.kind, load_error::Kind::HostNotFound, "{failure:?}");
                                    assert_eq!(failure.url, "http://felifox.invalid/");
                                    second_page.navigate(&download_url).unwrap();
                                }
                                6 => {
                                    // Give a wrongly accepted hidden download time to show up.
                                    Timer::after(Duration::from_millis(500)).await;
                                    downloads.extend(downloads::native::drain());
                                    let started: Vec<_> = downloads.iter().filter_map(|event| match event {
                                        downloads::Event::Started { url, path, total, .. } => Some((url.as_str(), path.clone(), *total)),
                                        _ => None,
                                    }).collect();
                                    // WebKit itself replaces the slashes; either way the file stays inside.
                                    let [(url, saved, total)] = &started[..] else { panic!("expected one download: {downloads:?}") };
                                    assert_eq!((*url, *total), (download_url.as_str(), Some(7)));
                                    assert_eq!(saved.parent(), Some(folder.as_path()), "{saved:?}");
                                    assert!(saved.to_string_lossy().ends_with("fixture.bin"), "{saved:?}");
                                    assert!(downloads.iter().any(|event| matches!(event, downloads::Event::Finished { received: 7, .. })), "{downloads:?}");
                                    assert_eq!(std::fs::read(saved).unwrap(), b"fixture");
                                    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1, "the hidden page saved nothing");
                                    std::fs::remove_dir_all(folder.parent().unwrap()).unwrap();
                                    // The visible page's requests, its `navigator.userAgent` and the hidden
                                    // page's request all carry one Safari-compatible User-Agent.
                                    let agents = user_agents.lock().unwrap().clone();
                                    let agent = identity::user_agent();
                                    let (webkit, safari) = agent.split_once(" Version/").unwrap_or_else(|| panic!("no Safari part: {agent:?}"));
                                    assert!(webkit.starts_with("Mozilla/5.0 (Macintosh; ") && webkit.ends_with(" (KHTML, like Gecko)"), "{agent:?}");
                                    let version = safari.strip_suffix(" Safari/605.1.15").unwrap_or_else(|| panic!("no Safari token: {agent:?}"));
                                    assert!(version.split('.').all(|part| part.parse::<u16>().is_ok()), "{agent:?}");
                                    assert!(!agent.to_lowercase().contains("feli"), "{agent:?}");
                                    let sent = |source: &str| agents.iter().filter(|(from, _)| from == source).count();
                                    assert!(sent("/first") >= 1, "the visible page's request sent no User-Agent: {agents:?}");
                                    assert!(sent("navigator.userAgent") >= 1, "the page never reported navigator.userAgent: {agents:?}");
                                    assert!(sent("/download") >= 2, "expected the visible and the hidden page's downloads: {agents:?}");
                                    assert!(agents.iter().all(|(_, other)| other == agent), "expected {agent:?} everywhere: {agents:?}");

                                    // Web Inspector: docked inside the tab's container, then detached.
                                    macro_rules! wait_for {
                                        ($what:literal, $ready:expr) => {{
                                            let deadline = Instant::now() + Duration::from_secs(10);
                                            loop {
                                                let state = second_page.inspector_for_test();
                                                if state.as_ref().is_some_and($ready) { break; }
                                                assert!(Instant::now() < deadline, concat!("Web Inspector never ", $what, ": {:?}"), state);
                                                Timer::after(Duration::from_millis(50)).await;
                                            }
                                        }};
                                    }
                                    second_page.toggle_devtools();
                                    wait_for!("opened", |s| s.visible);
                                    second_page.dock_inspector_for_test(true);
                                    wait_for!("docked in the tab", |s| s.visible && !s.detached && s.in_container);
                                    second_page.toggle_devtools();
                                    wait_for!("closed when docked", |s| !s.visible);
                                    second_page.toggle_devtools();
                                    wait_for!("reopened", |s| s.visible);
                                    second_page.dock_inspector_for_test(false);
                                    wait_for!("detached", |s| s.visible && s.detached && !s.in_container);
                                    // With the browser window in front, ⌥⌘I brings the inspector back. Window
                                    // order and main-window status need the app active, which other apps
                                    // may have taken since launch.
                                    cx.update(|window, cx| { cx.activate(true); window.activate_window(); }).unwrap();
                                    wait_for!("went behind the browser window", |s| s.visible && !s.front);
                                    second_page.toggle_devtools();
                                    wait_for!("came to the front", |s| s.visible && s.front);
                                    second_page.toggle_devtools();
                                    wait_for!("closed when in front", |s| !s.visible);
                                    second_page.find("test link", false);
                                    second_page.find("TEST", true);
                                    second_page.find("no such words", false);
                                }
                                7 => {
                                    assert_eq!(found, [true, true, false], "find matches case-insensitively and reports misses");
                                    second_page.navigate(&find_url).unwrap();
                                }
                                8 => {
                                    // Typing letter by letter refines the current match instead of
                                    // jumping past it; repeating the query (⌘G, ⇧⌘G) steps.
                                    for (text, backwards, selected) in [
                                        ("t", false, "m1:t"),
                                        ("ta", false, "m1:ta"),
                                        ("tab", false, "m2:tab"),
                                        ("tab", false, "m3:tab"),
                                        ("tab", true, "m2:tab"),
                                        ("tabl", false, "m3:tabl"),
                                        ("tab", false, "m3:tab"),
                                        // The same inside a same-origin frame.
                                        ("z", false, "f1:z"),
                                        ("ze", false, "f1:ze"),
                                        ("zebu", false, "f2:zebu"),
                                        ("zebu", false, "f3:zebu"),
                                        ("zebus", false, "f3:zebus"),
                                    ] {
                                        assert_eq!(find_step(&second_page, text, backwards).await, (true, selected.to_string()), "finding {text:?}, backwards: {backwards}");
                                    }
                                    // Emptying the query clears the match, here inside the frame.
                                    second_page.deselect();
                                    assert_eq!(read(&second_page, SELECTION).await, "", "the last match in the frame is deselected");
                                    assert_eq!(find_step(&second_page, "tap", false).await, (true, "m1:tap".to_string()));
                                    second_page.deselect();
                                    assert_eq!(read(&second_page, SELECTION).await, "", "the last match in the page is deselected");
                                    assert_eq!(read(&second_page, "String(!!top.findTouched)").await, "false", "the page never sees the find bar edit its selection");
                                    // The find shortcut goes to the page first. A page that takes ⌘F keeps
                                    // it: no report. This page does not take ⌘G and ⇧⌘G: one report each.
                                    read(&second_page, "addEventListener('keydown', e => { if (e.metaKey && e.key === 'f') { e.preventDefault(); document.title = 'PAGE FIND'; } }); 'ok'").await;
                                    cx.update(|window, cx| { cx.activate(true); window.activate_window(); }).unwrap();
                                    second_page.drain_events();
                                    second_page.native_key("Meta+f").unwrap();
                                    let deadline = Instant::now() + Duration::from_secs(5);
                                    while read(&second_page, "document.title").await != "PAGE FIND" {
                                        assert!(Instant::now() < deadline, "the page never got ⌘F");
                                        Timer::after(Duration::from_millis(50)).await;
                                    }
                                    second_page.native_key("Meta+g").unwrap();
                                    second_page.native_key("Meta+Shift+g").unwrap();
                                    let mut keys = Vec::new();
                                    while keys.len() < 2 {
                                        assert!(Instant::now() < deadline + Duration::from_secs(5), "the page did not report the keys it left: {keys:?}");
                                        Timer::after(Duration::from_millis(50)).await;
                                        keys.extend(second_page.drain_events().into_iter().filter_map(|event| match event { Event::FindKey(step) => Some(step), _ => None }));
                                    }
                                    assert_eq!(keys, [1, -1], "⌘F that the page took makes no report; ⌘G and ⇧⌘G make one each");
                                    second_page.navigate(&embed_url).unwrap();
                                }
                                9 => {
                                    // The image of another origin got a sign-in challenge. Give
                                    // a wrongly taken question time to show up.
                                    Timer::after(Duration::from_millis(500)).await;
                                    assert_eq!(http_auth::waiting(key), None, "another origin must not ask the user");
                                    second_page.navigate(&auth_url).unwrap();
                                }
                                10 => {
                                    // The hidden page's challenge came first. It took no id, so
                                    // this one is the first question.
                                    let (id, challenge) = http_auth::waiting(key).unwrap();
                                    assert_eq!(challenge, http_auth::Challenge::new("127.0.0.1", port, "Fixtureâ® realm", false, false));
                                    // Escape cancels: the page shows the server's 401 text.
                                    sign_in_keys(&second_page, &browser, &["x", "escape"], cx).await;
                                    assert!(!http_auth::is_waiting(id));
                                    http_auth::answer(id, None);
                                }
                                11 => {
                                    prompt_closed(&second_page, &browser, cx);
                                    second_page.navigate(&auth_url).unwrap();
                                }
                                // Typed text goes to the user name, Tab moves to the password, Enter signs in.
                                12 => sign_in_keys(&second_page, &browser, &["u", "s", "e", "r", "tab", "n", "o", "enter"], cx).await,
                                13 => {
                                    let (_, challenge) = http_auth::waiting(key).unwrap();
                                    assert!(challenge.retry, "the server refused the first answer");
                                    // Tab from the password goes back to the user name.
                                    sign_in_keys(&second_page, &browser, &["tab", "p", "a", "s", "s", "tab", "u", "s", "e", "r", "enter"], cx).await;
                                }
                                14 => {
                                    prompt_closed(&second_page, &browser, cx);
                                    second_page.navigate(&both_auth_url).unwrap();
                                }
                                15 => {
                                    let (id, challenge) = http_auth::waiting(key).unwrap();
                                    assert_eq!(challenge.realm, "Both");
                                    sign_in_keys(&second_page, &browser, &["escape"], cx).await;
                                    assert!(!http_auth::is_waiting(id));
                                }
                                16 => {
                                    // The server's 401 text shows, and its frame and its image
                                    // loaded. Give a question for the second method or for the
                                    // image time to show up.
                                    Timer::after(Duration::from_millis(300)).await;
                                    assert_eq!(http_auth::waiting(key), None, "Cancel must not ask again until the next navigation");
                                    prompt_closed(&second_page, &browser, cx);
                                    // A new navigation asks again.
                                    second_page.navigate(&other_auth_url).unwrap();
                                }
                                17 => {
                                    // The page leaves for another address with the prompt open,
                                    // while the user types in the address bar.
                                    cx.update(|window, cx| {
                                        assert!(auth_prompt::sync(&second_page, true, &browser, window, cx));
                                        assert!(auth_prompt::view(&second_page, cx).is_some());
                                        window.focus(&address);
                                        window.refresh();
                                    }).unwrap();
                                    second_page.navigate(&first_url).unwrap();
                                }
                                18 => {
                                    // The new document cancelled the question and its prompt.
                                    // The keys stay in the address bar.
                                    assert_eq!(http_auth::waiting(key), None);
                                    cx.update(|window, cx| {
                                        assert!(auth_prompt::sync(&second_page, true, &browser, window, cx));
                                        assert!(auth_prompt::view(&second_page, cx).is_none());
                                        assert!(address.is_focused(window));
                                        assert!(!second_page.has_keys_for_test(), "a closed prompt must not take the keys from the address bar");
                                        window.focus(&browser);
                                        window.refresh();
                                    }).unwrap();
                                    second_page.navigate(&other_auth_url).unwrap();
                                }
                                19 => {
                                    // The question comes while the tab is in the background and
                                    // the user types in the address bar: the prompt takes nothing.
                                    let (id, _) = http_auth::waiting(key).unwrap();
                                    cx.update(|window, cx| {
                                        window.focus(&address);
                                        assert!(auth_prompt::sync(&second_page, false, &browser, window, cx));
                                        assert!(auth_prompt::view(&second_page, cx).is_some());
                                        assert!(address.is_focused(window), "a prompt in the background takes no keys");
                                        // The user selects the tab: the browser gives the keys to
                                        // the prompt, as in select_tab.
                                        window.focus(&browser);
                                        auth_prompt::focus(&second_page, window, cx);
                                        assert!(!browser.is_focused(window) && !address.is_focused(window));
                                        window.refresh();
                                    }).unwrap();
                                    // Escape works without a click in a field.
                                    Timer::after(Duration::from_millis(250)).await;
                                    cx.update(|window, cx| window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx)).unwrap();
                                    assert!(!http_auth::is_waiting(id), "the selected tab's prompt has the keys");
                                }
                                20 => {
                                    prompt_closed(&second_page, &browser, cx);
                                    second_page.navigate(&other_auth_url).unwrap();
                                }
                                21 => {
                                    // The load fails before a document commits, with the prompt
                                    // open: WebKit blocks port 1.
                                    cx.update(|window, cx| {
                                        assert!(auth_prompt::sync(&second_page, true, &browser, window, cx));
                                        window.refresh();
                                    }).unwrap();
                                    page_failed = false;
                                    second_page.navigate("http://127.0.0.1:1/").unwrap();
                                }
                                22 => {
                                    // The failed load cancelled the question and its prompt.
                                    assert_eq!(http_auth::waiting(key), None);
                                    prompt_closed(&second_page, &browser, cx);
                                    let page = cx.update(|window, _| NativePage::new(window, &other_auth_url)).unwrap();
                                    closing = Some(page.expect("create the tab to close"));
                                }
                                23 => {
                                    // The tab closes with its question open. WebKit raises an
                                    // exception unless the handler is called exactly one time.
                                    let (id, challenge) = closing_key.and_then(http_auth::waiting).unwrap();
                                    assert_eq!(challenge.realm, "Other");
                                    drop(closing.take());
                                    assert!(!http_auth::is_waiting(id));
                                    Timer::after(Duration::from_millis(300)).await;
                                    assert_eq!(http_auth::waiting(key), None);
                                    println!("PASS: WebKit JavaScript, DOM, storage, history, failed-load error event and URL polling, hover status, Cmd-click, middle-click and right-click menu bridge, frame-aware navigation policy, a download that stays in the downloads folder while a hidden page's is refused, tab containers, a Web Inspector that Safari can't attach to, one Safari-compatible User-Agent, find in page, the find shortcut that a page can take, and the HTTP sign-in prompt: Escape cancels, typed text, Tab and Enter sign in after a refused answer, Cancel does not ask again for a second method, a frame or an image, a prompt of a background tab takes no keys and has them once its tab is selected, a new document, a failed load and a closed tab cancel, and a hidden page and an image of another origin ask nothing");
                                    result.set(true);
                                    cx.update(|window, cx| { window.remove_window(); cx.quit(); }).unwrap();
                                    break;
                                }
                                _ => unreachable!(),
                            }
                            stage += 1;
                        }
                    }).detach();
                    TestWindow { page, browser: window_browser, address: window_address, _second_tab: second_tab, _failed_tab: failed_tab, _hidden: hidden, _hidden_auth: hidden_auth }
                })
            },
        ).expect("native test window");
        let _ = handle;
        cx.activate(true);
    });
    assert!(passed.get(), "native webview test did not finish");
}
