//! Opt-in native test of sign-in persistence and the Cookies window. Loopback traffic only.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_cookies
//!
//! Cookies only prove that they persist when a new process reads them, so this process
//! serves a fixture and runs itself twice as a child. The first child loads pages that set
//! cookies, checks the Cookies window against them and deletes a branch and a single cookie
//! through the window's own handlers. The second child must get the cookie with an expiry
//! back from WebKit, must not get the session cookie, and then removes the fixture's cookies.
//! The test binary has its own WebKit storage, apart from Felinaut's.

#[allow(dead_code, unused_imports)]
#[path = "../src/audio.rs"]
mod audio;
#[allow(dead_code, unused_imports)]
#[path = "../src/cookie_graph.rs"]
mod cookie_graph;
#[allow(dead_code)]
#[path = "../src/cookie_store.rs"]
mod cookie_store;
#[allow(dead_code)]
#[path = "../src/cookie_window.rs"]
mod cookie_window;
#[allow(dead_code)]
#[path = "../src/cookies.rs"]
mod cookies;
#[allow(dead_code, unused_imports)]
#[path = "../src/downloads.rs"]
mod downloads;
#[allow(dead_code, unused_imports)]
#[path = "../src/egui_host.rs"]
mod egui_host;
#[allow(dead_code, unused_imports)]
#[path = "../src/icons.rs"]
mod icons;
#[allow(dead_code, unused_imports)]
#[path = "../src/input.rs"]
mod input;
#[allow(dead_code, unused_imports)]
#[path = "../src/load_error.rs"]
mod load_error;
#[allow(dead_code, unused_imports)]
#[path = "../src/theme.rs"]
mod theme;
#[allow(dead_code)]
#[path = "../src/webview.rs"]
mod webview;

use cookies::{Cookie, Key, Node, SameSite};
use gpui::{prelude::*, *};
use std::{
    cell::Cell,
    collections::HashSet,
    io::{Read, Write},
    net::TcpListener,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use webview::{Event, NativePage};

const PHASE: &str = "FELINAUT_COOKIE_TEST_PHASE";
const PORT: &str = "FELINAUT_COOKIE_TEST_PORT";
const LIMIT: Duration = Duration::from_secs(30);

struct TestWindow {
    _pages: [Rc<NativePage>; 2],
}
impl Render for TestWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

/// `/set` sets the fixture's cookies. Every page's title lists the cookies the request had.
fn serve(listener: TcpListener) {
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else {
            break;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut request = [0; 8192];
        let Ok(count) = stream.read(&mut request) else {
            continue;
        };
        let request = String::from_utf8_lossy(&request[..count]);
        let mut names: Vec<&str> = request
            .lines()
            .find_map(|line| {
                let (header, value) = line.split_once(':')?;
                header.eq_ignore_ascii_case("cookie").then_some(value)
            })
            .unwrap_or_default()
            .split(';')
            .filter_map(|pair| Some(pair.split_once('=')?.0.trim()))
            .collect();
        names.sort_unstable();
        let set = if request.starts_with("GET /set ") {
            "Set-Cookie: felinaut_persist=p; Max-Age=3600; Path=/\r\n\
             Set-Cookie: felinaut_extra=e; Max-Age=3600; Path=/\r\n\
             Set-Cookie: felinaut_session=s; Path=/\r\n\
             Set-Cookie: felinaut_scoped=c; Max-Age=3600; Path=/scoped; HttpOnly; SameSite=Strict\r\n"
        } else {
            ""
        };
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>COOKIES[{}]</title>",
            names.join(",")
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\n{set}Connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    }
}

async fn tick(start: Instant, stage: &str) {
    Timer::after(Duration::from_millis(50)).await;
    assert!(start.elapsed() < LIMIT, "timed out: {stage}");
}

/// The `COOKIES[…]` title of the page's next document.
async fn title(page: &NativePage, start: Instant, stage: &str) -> String {
    loop {
        tick(start, stage).await;
        for event in page.drain_events() {
            if let Event::Title(title) = event
                && title.starts_with("COOKIES[")
            {
                return title;
            }
        }
    }
}

/// WebKit's answer to a cookie store request, as the Cookies window waits for it.
async fn answer(cookies: mpsc::Receiver<Vec<Cookie>>, start: Instant, stage: &str) -> Vec<Cookie> {
    loop {
        tick(start, stage).await;
        if let Ok(cookies) = cookies.try_recv() {
            return cookies;
        }
    }
}

/// The window's rows as (depth, label, cookies), once they show what WebKit reported.
async fn rows(
    cx: &mut AsyncWindowContext,
    start: Instant,
    stage: &str,
    ready: impl Fn(&[(usize, String, usize)]) -> bool,
) -> Vec<(usize, String, usize)> {
    loop {
        tick(start, stage).await;
        let rows = cx
            .update(|_, cx| {
                cookie_window::update_for_test(cx, |manager, _, _| {
                    manager.loaded().then(|| {
                        manager
                            .rows()
                            .iter()
                            .map(|row| (row.depth, row.label.clone(), row.count))
                            .collect::<Vec<_>>()
                    })
                })
            })
            .unwrap();
        if let Some(rows) = rows
            && ready(&rows)
        {
            return rows;
        }
    }
}

fn probe(cx: &mut AsyncWindowContext) -> egui_host::Probe {
    cx.update(|_, cx| cookie_window::update_for_test(cx, |manager, _, _| manager.graph_probe()))
        .unwrap()
}

/// The graph once it has come to rest: no pass for half a second.
async fn rest(cx: &mut AsyncWindowContext, start: Instant, stage: &str) -> egui_host::Probe {
    let mut last = probe(cx);
    let mut still = 0;
    while still < 10 {
        tick(start, stage).await;
        let now = probe(cx);
        still = if now.passes == last.passes {
            still + 1
        } else {
            0
        };
        last = now;
    }
    last
}

/// The graph follows a resize by itself.
async fn graph(cx: &mut AsyncWindowContext, start: Instant) {
    let before = rest(cx, start, "the graph comes to rest").await;
    let painted = before.painted.expect("the graph was painted");
    cx.update(|_, cx| {
        cookie_window::window_for_test(cx, |window, _| {
            window.resize(size(px(1040.), px(700.)));
        })
    })
    .unwrap();
    let resized = loop {
        tick(start, "the resize reaches the graph").await;
        let now = probe(cx);
        if now
            .painted
            .is_some_and(|bounds| bounds.size != painted.size)
        {
            break now;
        }
    };
    // Nothing else happens: the resize alone must bring the next pass to the new size.
    let after = rest(cx, start, "the resized graph comes to rest").await;
    assert_eq!(
        after.passed,
        after.painted.map(|bounds| bounds.size),
        "the graph's pass follows the resize ({resized:?} then {after:?})"
    );
}

fn has(rows: &[(usize, String, usize)], depth: usize, label: &str, count: usize) -> bool {
    rows.iter()
        .any(|row| *row == (depth, label.to_string(), count))
}

fn fixture(cookies: &[Cookie]) -> Vec<&Cookie> {
    cookies
        .iter()
        .filter(|cookie| cookie.name.starts_with("felinaut_"))
        .collect()
}

/// The first process: set cookies, then read and delete them through the Cookies window.
async fn set(cx: &mut AsyncWindowContext, pages: [Rc<NativePage>; 2], port: u16) {
    let start = Instant::now();
    let [by_address, by_name] = pages;
    // Both pages have set their cookies. A run that failed earlier may have left some of the
    // same cookies behind, so what these first requests carried says nothing.
    title(&by_address, start, "127.0.0.1/set").await;
    title(&by_name, start, "localhost/set").await;

    cx.update(|_, cx| cookie_window::open(cx)).unwrap();
    let sites = rows(cx, start, "the window lists both sites", |rows| {
        has(rows, 1, "127.0.0.1", 4) && has(rows, 1, "localhost", 4)
    })
    .await;
    let others = sites.iter().filter(|row| row.0 == 1).count() - 2;
    assert_eq!(sites[0], (0, "All cookies".to_string(), 8 + others));

    // The menu action also arrives while the Cookies window is the active one, inside that
    // window's own update. It must come forward again, not open a second window.
    let windows = cx.update(|_, cx| cx.windows().len()).unwrap();
    cx.update(|_, cx| {
        cookie_window::update_for_test(cx, |_, window, cx| {
            window.dispatch_action(Box::new(cookie_window::ShowCookies), cx);
        })
    })
    .unwrap();
    for _ in 0..4 {
        tick(start, "the repeated menu action").await;
    }
    assert_eq!(
        cx.update(|_, cx| cx.windows().len()).unwrap(),
        windows,
        "Cookies is a single window"
    );
    graph(cx, start).await;

    // A branch asks first: nothing goes until the one confirmation.
    cx.update(|_, cx| {
        cookie_window::update_for_test(cx, |manager, window, cx| {
            manager.select(Node::Site("localhost".into()), cx);
            manager.delete_selection(window, cx);
        })
    })
    .unwrap();
    let unconfirmed = rows(cx, start, "the unconfirmed delete", |_| true).await;
    assert!(has(&unconfirmed, 1, "localhost", 4), "{unconfirmed:?}");
    cx.update(|_, cx| {
        cookie_window::update_for_test(cx, |manager, window, cx| {
            manager.confirm_delete(window, cx);
        })
    })
    .unwrap();
    let after = rows(cx, start, "the site is deleted", |rows| {
        !rows.iter().any(|row| row.1 == "localhost")
    })
    .await;
    assert!(has(&after, 1, "127.0.0.1", 4), "{after:?}");
    // WebKit agrees: the deleted site's next request carries no cookie, the other's does.
    by_name
        .navigate(&format!("http://localhost:{port}/show"))
        .unwrap();
    assert_eq!(title(&by_name, start, "localhost/show").await, "COOKIES[]");
    by_address
        .navigate(&format!("http://127.0.0.1:{port}/show"))
        .unwrap();
    assert_eq!(
        title(&by_address, start, "127.0.0.1/show").await,
        "COOKIES[felinaut_extra,felinaut_persist,felinaut_session]"
    );

    // One cookie goes at once, found by opening the site and its host.
    cx.update(|_, cx| {
        cookie_window::update_for_test(cx, |manager, window, cx| {
            // A small store opens fully by itself; open what is still closed.
            for branch in [
                Node::Site("127.0.0.1".into()),
                Node::Host("127.0.0.1".into()),
            ] {
                let closed = manager
                    .rows()
                    .iter()
                    .any(|row| row.node == branch && row.expanded == Some(false));
                if closed {
                    manager.toggle(&branch, cx);
                }
            }
            let extra = manager
                .rows()
                .iter()
                .find(|row| row.depth == 3 && row.label == "felinaut_extra")
                .expect("the cookie's row")
                .node
                .clone();
            manager.select(extra, cx);
            manager.delete_selection(window, cx);
        })
    })
    .unwrap();
    let after = rows(cx, start, "the cookie is deleted", |rows| {
        has(rows, 1, "127.0.0.1", 3)
    })
    .await;
    assert!(!after.iter().any(|row| row.1 == "felinaut_extra"));
    assert!(has(&after, 3, "felinaut_scoped", 1), "{after:?}");
}

/// The second process: what did WebKit keep?
async fn check(page: Rc<NativePage>) {
    let start = Instant::now();
    assert_eq!(
        title(&page, start, "127.0.0.1/show after a restart").await,
        "COOKIES[felinaut_persist]",
        "the cookie with an expiry must survive a restart, the session cookie must not"
    );

    let (reply, cookies) = mpsc::channel();
    cookie_store::fetch(reply).unwrap();
    let cookies = answer(cookies, start, "reading the store").await;
    let kept = fixture(&cookies);
    let mut names: Vec<_> = kept.iter().map(|cookie| cookie.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["felinaut_persist", "felinaut_scoped"]);
    for cookie in &kept {
        assert_eq!(cookie.host(), "127.0.0.1");
        assert!(cookie.expires.is_some() && !cookie.secure, "{cookie:?}");
        let scoped = cookie.name == "felinaut_scoped";
        assert_eq!(cookie.path, if scoped { "/scoped" } else { "/" });
        assert_eq!(cookie.http_only, scoped);
        assert_eq!(cookie.same_site == SameSite::Strict, scoped, "{cookie:?}");
    }

    // Leave nothing behind, and prove that deleting by key reaches the stored cookies.
    let keys: HashSet<Key> = kept.iter().map(|cookie| cookie.key()).collect();
    let (reply, cookies) = mpsc::channel();
    cookie_store::delete(keys, reply).unwrap();
    let left = answer(cookies, start, "deleting the fixture's cookies").await;
    assert!(fixture(&left).is_empty(), "{:?}", fixture(&left));
}

fn child(phase: String, port: u16) -> bool {
    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    Application::new().run(move |cx| {
        cx.set_global(theme::ThemeSettings::default());
        input::init(cx);
        cookie_window::init(cx);
        let setting = phase == "set";
        let urls = if setting {
            [
                format!("http://127.0.0.1:{port}/set"),
                format!("http://localhost:{port}/set"),
            ]
        } else {
            [
                format!("http://127.0.0.1:{port}/show"),
                "about:blank".into(),
            ]
        };
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(480.), px(320.)),
                    cx,
                ))),
                ..Default::default()
            },
            |window, cx| {
                let pages = urls
                    .map(|url| Rc::new(NativePage::new(window, &url).expect("create native page")));
                let task_pages = pages.clone();
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        if setting {
                            set(cx, task_pages, port).await;
                        } else {
                            check(task_pages[0].clone()).await;
                        }
                        result.set(true);
                        cx.update(|_, cx| cx.quit()).unwrap();
                    })
                    .detach();
                    TestWindow { _pages: pages }
                })
            },
        )
        .expect("native test window");
        cx.activate(true);
    });
    passed.get()
}

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST")
        .or_else(|| std::env::var_os("FELIFOX_NATIVE_TEST"))
        .as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        println!("Native cookie test skipped; set FELINAUT_NATIVE_TEST=1 to run.");
        return;
    }
    if let Ok(phase) = std::env::var(PHASE) {
        let port = std::env::var(PORT).ok().and_then(|port| port.parse().ok());
        let passed = child(phase, port.expect("the fixture's port"));
        std::process::exit(if passed { 0 } else { 1 });
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(listener));
    let program = std::env::current_exe().expect("this test's executable");
    for phase in ["set", "check"] {
        let status = std::process::Command::new(&program)
            .env(PHASE, phase)
            .env(PORT, port.to_string())
            .status()
            .expect("run the test again as a child");
        assert!(status.success(), "the {phase} process failed");
    }
    println!(
        "PASS: cookies set by pages appear in the one Cookies window; a confirmed site delete \
         and a single cookie delete reach WebKit; after a restart the cookie with an expiry \
         is back and the session cookie is gone"
    );
}
