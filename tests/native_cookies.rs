//! Opt-in native test of sign-in persistence. Loopback traffic only.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_cookies
//!
//! Cookies only prove that they persist when a new process reads them, so this process
//! serves a fixture and runs itself twice as a child. The first child loads a page that sets
//! a cookie with an expiry and a session cookie. The second child, a fresh process, must send
//! the first back and not the second; it then has the fixture expire its cookies.
//! The test binary has its own WebKit storage, apart from Felinaut's.

#[allow(dead_code, unused_imports)]
#[path = "../src/audio.rs"]
mod audio;
#[allow(dead_code, unused_imports)]
#[path = "../src/downloads.rs"]
mod downloads;
#[allow(dead_code, unused_imports)]
#[path = "../src/icons.rs"]
mod icons;
#[allow(dead_code, unused_imports)]
#[path = "../src/load_error.rs"]
mod load_error;
#[allow(dead_code)]
#[path = "../src/webview.rs"]
mod webview;

use gpui::{prelude::*, *};
use std::{
    cell::Cell,
    io::{Read, Write},
    net::TcpListener,
    rc::Rc,
    time::{Duration, Instant},
};
use webview::{Event, NativePage};

const PHASE: &str = "FELINAUT_COOKIE_TEST_PHASE";
const PORT: &str = "FELINAUT_COOKIE_TEST_PORT";
const LIMIT: Duration = Duration::from_secs(30);

struct TestWindow {
    _page: Rc<NativePage>,
}
impl Render for TestWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

/// `/set` sets the fixture's cookies and `/clear` expires them. Every page's title lists the
/// cookies the request had.
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
             Set-Cookie: felinaut_session=s; Path=/\r\n"
        } else if request.starts_with("GET /clear ") {
            "Set-Cookie: felinaut_persist=; Max-Age=0; Path=/\r\n\
             Set-Cookie: felinaut_session=; Max-Age=0; Path=/\r\n"
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

/// The `COOKIES[…]` title of the page's next document.
async fn title(page: &NativePage, start: Instant, stage: &str) -> String {
    loop {
        Timer::after(Duration::from_millis(50)).await;
        assert!(start.elapsed() < LIMIT, "timed out: {stage}");
        for event in page.drain_events() {
            if let Event::Title(title) = event
                && title.starts_with("COOKIES[")
            {
                return title;
            }
        }
    }
}

/// The first process: a page sets the cookies, and the same session sends both back.
async fn set(page: &NativePage, base: &str) {
    let start = Instant::now();
    // A run that failed earlier may have left the persistent cookie behind, so what this
    // first request carried says nothing.
    title(page, start, "/set").await;
    page.navigate(&format!("{base}/show")).unwrap();
    assert_eq!(
        title(page, start, "/show").await,
        "COOKIES[felinaut_persist,felinaut_session]"
    );
}

/// The second process: what did WebKit keep?
async fn check(page: &NativePage, base: &str) {
    let start = Instant::now();
    assert_eq!(
        title(page, start, "/show after a restart").await,
        "COOKIES[felinaut_persist]",
        "the cookie with an expiry must survive a restart, the session cookie must not"
    );
    // Leave nothing behind.
    page.navigate(&format!("{base}/clear")).unwrap();
    title(page, start, "/clear").await;
    page.navigate(&format!("{base}/show")).unwrap();
    assert_eq!(title(page, start, "/show, cleared").await, "COOKIES[]");
}

fn child(phase: String, port: u16) -> bool {
    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    Application::new().run(move |cx| {
        let setting = phase == "set";
        let base = format!("http://127.0.0.1:{port}");
        let first = format!("{base}/{}", if setting { "set" } else { "show" });
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
                let page = Rc::new(NativePage::new(window, &first).expect("create native page"));
                let task_page = page.clone();
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        if setting {
                            set(&task_page, &base).await;
                        } else {
                            check(&task_page, &base).await;
                        }
                        result.set(true);
                        cx.update(|_, cx| cx.quit()).unwrap();
                    })
                    .detach();
                    TestWindow { _page: page }
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
        "PASS: after a restart WebKit sends the cookie with an expiry back and has dropped \
         the session cookie"
    );
}
