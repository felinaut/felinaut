//! Opt-in native integration test. WKWebView requires macOS's main thread.
//! Run: FELIFOX_NATIVE_TEST=1 cargo test --test native_webview

#[allow(dead_code, unused_imports)]
#[path = "../src/audio.rs"]
mod audio;
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

struct TestWindow {
    page: Rc<NativePage>,
    _second_tab: NativePage,
}
impl Render for TestWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let page = self.page.clone();
        div().size_full().child(
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

fn main() {
    if std::env::var("FELIFOX_NATIVE_TEST").as_deref() != Ok("1") {
        println!("Native webview test skipped; set FELIFOX_NATIVE_TEST=1 to run.");
        return;
    }
    // All fixture traffic stays on loopback; exercise the production, URL-only link bridge.
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let base = format!("http://{}", listener.local_addr().unwrap());
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
            let second = request.starts_with("GET /second ");
            let script = if second {
                "document.title = localStorage.getItem('felifox-smoke') === 'yes' ? 'JS SECOND' : 'STORAGE FAILED';"
            } else {
                "localStorage.setItem('felifox-smoke', 'yes'); document.title = 'JS FIRST';"
            };
            let body = format!(
                "<!doctype html><meta charset=utf-8><title>Waiting</title><style>body{{background:#edf0ff;color:#101324}}</style><h1 id=result>JavaScript test</h1><a id=testlink href='/link-destination'><span id=linkchild>Test link</span></a><script>{script}document.querySelector('#result').textContent = document.title;</script>"
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
        let first_url = format!("{base}/first");
        let second_url = format!("{base}/second");
        let handle = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(800.), px(600.)), cx))),
                ..Default::default()
            },
            |window, cx| {
                let page = Rc::new(NativePage::new(window, &first_url).expect("create native page"));
                let second_tab = NativePage::new(window, "about:blank").expect("create inactive tab");
                second_tab.set_visible(false).unwrap();
                page.focus().unwrap();
                page.focus_parent().unwrap();
                let second_page = page.clone();
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        let start = Instant::now();
                        let mut stage = 0;
                        let mut title = String::new();
                        let mut hover_seen = false;
                        let mut hover_cleared = false;
                        let mut background_opens = 0;
                        let mut foreground_open = false;
                        loop {
                            Timer::after(Duration::from_millis(50)).await;
                            let mut finished = false;
                            for event in second_page.drain_events() {
                                match event {
                                    Event::Title(value) => title = value,
                                    Event::Finished(_) => finished = true,
                                    Event::Hover(Some(url)) if url.ends_with("/link-destination") => hover_seen = true,
                                    Event::Hover(None) => hover_cleared = true,
                                    Event::OpenInBackground(url) if url.ends_with("/link-destination") => background_opens += 1,
                                    Event::Open(url) if url.ends_with("/link-destination") => foreground_open = true,
                                    _ => {},
                                }
                            }
                            assert!(start.elapsed() < Duration::from_secs(20), "native test timed out at stage {stage}; title={title:?}");
                            if stage <= 4 && !finished { continue; }
                            if stage == 5 && title != "LINK EVENTS OK" { continue; }
                            match stage {
                                0 => {
                                    assert_eq!(title, "JS FIRST", "JavaScript and DOM title update");
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
                                    second_page.set_visible(false).unwrap();
                                    second_page.set_bounds(Bounds::new(point(px(10.), px(20.)), size(px(640.), px(400.)))).unwrap();
                                    second_page.set_visible(true).unwrap();
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
                                    println!("PASS: WebKit JavaScript, DOM, storage, history, hover status, Cmd-click, middle-click and right-click menu bridge");
                                    result.set(true);
                                    cx.update(|window, cx| { window.remove_window(); cx.quit(); }).unwrap();
                                    break;
                                }
                                _ => unreachable!(),
                            }
                            stage += 1;
                        }
                    }).detach();
                    TestWindow { page, _second_tab: second_tab }
                })
            },
        ).expect("native test window");
        let _ = handle;
        cx.activate(true);
    });
    assert!(passed.get(), "native webview test did not finish");
}
