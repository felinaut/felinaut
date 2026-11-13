//! Opt-in native integration test. WKWebView requires macOS's main thread.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_webview

#[allow(dead_code, unused_imports)]
#[path = "../src/audio.rs"]
mod audio;
#[allow(dead_code, unused_imports)]
#[path = "../src/dialogs.rs"]
mod dialogs;
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
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use webview::{Event, NativePage};

struct TestWindow {
    page: Rc<NativePage>,
    _failed_tab: Rc<NativePage>,
    _hidden: [NativePage; 2],
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
    // What a hidden page got from its dialogs.
    let hidden_answers = Arc::new(Mutex::new(String::new()));
    let reported_answers = hidden_answers.clone();
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
            if request.starts_with("GET /download ") {
                // The server asks for a name outside the downloads folder.
                served_downloads.fetch_add(1, Ordering::SeqCst);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"../../fixture.bin\"\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture");
                continue;
            }
            if let Some(answers) = request.strip_prefix("GET /hidden-result?") {
                *reported_answers.lock().unwrap() =
                    answers.split(' ').next().unwrap_or_default().to_owned();
            }
            if request.starts_with("GET /hidden-dialogs ") {
                let body = "<!doctype html><script>alert('a'); fetch('/hidden-result?' + confirm('b') + '-' + prompt('c', 'd'));</script>";
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                continue;
            }
            if request.starts_with("GET /load-dialog ") {
                // The page asks while it loads, before the browser's poll sees the new document.
                let body =
                    "<!doctype html><script>document.title = 'LOAD ' + confirm('load?');</script>";
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                continue;
            }
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
        let download_url = format!("{base}/download");
        let folder = folder.clone();
        let download_requests = download_requests.clone();
        let hidden_answers = hidden_answers.clone();
        let origin = base.clone();
        let hidden_dialogs_url = format!("{base}/hidden-dialogs");
        // The same fixture server under another name: another origin for WebKit.
        let other_origin = base.replace("127.0.0.1", "localhost");
        let load_dialog_url = format!("{other_origin}/load-dialog");
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
                // Another one asks for dialogs; it must get the default answers at once.
                let hidden_dialogs = NativePage::hidden(window, &hidden_dialogs_url).expect("create hidden page");
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
                        let mut failure = None;
                        let mut downloads = Vec::new();
                        let mut background_tab = Some(second_tab);
                        let mut dialogs_seen = Vec::new();
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
                                    Event::Hover(Some(url)) if url.ends_with("/link-destination") => hover_seen = true,
                                    Event::Hover(None) => hover_cleared = true,
                                    Event::OpenInBackground(url) if url.ends_with("/link-destination") => background_opens += 1,
                                    Event::Open(url) if url.ends_with("/link-destination") => foreground_open = true,
                                    _ => {},
                                }
                            }
                            // Answer the fixture's dialogs as a user would; 'leave?' stays on show.
                            let dialog = second_page.dialog();
                            if let Some(shown) = &dialog {
                                let request = &shown.request;
                                let reply = |accepted, text: &str| dialogs::Reply { accepted, text: text.into() };
                                let answer = match request.message.as_str() {
                                    "one" | "two" | "load?" => Some((reply(true, ""), false)),
                                    "three" | "five" => Some((reply(false, "ignored"), false)),
                                    "four" => Some((reply(true, "typed"), false)),
                                    "six" => Some((reply(true, &request.default_text), true)),
                                    _ => None,
                                };
                                if let Some((reply, suppress)) = answer {
                                    let site = if request.message == "load?" { &other_origin } else { &origin };
                                    assert_eq!(request.title(), format!("{site} says"));
                                    dialogs_seen.push((request.kind, request.message.clone(), request.default_text.clone(), shown.can_suppress));
                                    second_page.answer_dialog(shown.id, reply, suppress);
                                }
                            }
                            let leave_shown = dialog.is_some_and(|shown| shown.request.message == "leave?");
                            assert!(start.elapsed() < Duration::from_secs(30), "native test timed out at stage {stage}; title={title:?}; dialogs={dialogs_seen:?}");
                            if stage <= 4 && !finished { continue; }
                            if stage == 5 && (title != "LINK EVENTS OK" || failure.is_none()) { continue; }
                            let downloaded = downloads.iter().any(|event| matches!(event, downloads::Event::Finished { .. }));
                            if stage == 6 && !(downloaded && download_requests.load(Ordering::SeqCst) >= 2) { continue; }
                            let background_dialog = background_tab.as_ref().and_then(|tab| tab.dialog());
                            if stage == 7 && !(title.starts_with("DIALOGS ") && background_dialog.is_some()) { continue; }
                            if (stage == 8 || stage == 13) && !finished { continue; }
                            if stage == 9 && !title.starts_with("STOPPED ") { continue; }
                            if stage == 10 && !title.starts_with("LOAD ") { continue; }
                            if stage == 11 && dialogs_seen.len() < 2 { continue; }
                            if stage == 12 && !leave_shown { continue; }
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
                                    // One dialog of each kind and answer, then a dialog the user stopped.
                                    second_page.evaluate_for_test(r#"
                                        setTimeout(() => {
                                          alert('one');
                                          const answers = [confirm('two'), confirm('three'), prompt('four', 'start'), prompt('five', 'x'), prompt('six', 'kept'), confirm('seven')];
                                          document.title = 'DIALOGS ' + JSON.stringify(answers);
                                        }, 0);
                                    "#).unwrap();
                                    background_tab.as_ref().unwrap().evaluate_for_test("setTimeout(() => alert('background'), 0)").unwrap();
                                }
                                7 => {
                                    assert_eq!(title, r#"DIALOGS [true,false,"typed",null,"kept",false]"#);
                                    use dialogs::Kind::{Alert, Confirm, Prompt};
                                    let expected = [
                                        (Alert, "one", "", false),
                                        (Confirm, "two", "", true),
                                        (Confirm, "three", "", true),
                                        (Prompt, "four", "start", true),
                                        (Prompt, "five", "x", true),
                                        (Prompt, "six", "kept", true),
                                    ].map(|(kind, message, text, can_suppress)| (kind, message.to_owned(), text.to_owned(), can_suppress));
                                    assert_eq!(dialogs_seen, expected, "'seven' came after the user stopped the dialogs");
                                    // A tab in the background keeps its dialog until someone looks. Closing
                                    // the tab must answer it: WebKit raises an exception for a lost handler.
                                    assert_eq!(background_dialog.unwrap().request.message, "background");
                                    background_tab = None;
                                    dialogs_seen.clear();
                                    second_page.reload().unwrap();
                                }
                                8 => {
                                    second_page.evaluate_for_test("setTimeout(() => { document.title = 'STOPPED ' + confirm('refused'); }, 0)").unwrap();
                                }
                                9 => {
                                    // A reload by the user stays on the origin: its dialogs stay stopped.
                                    assert_eq!(title, "STOPPED false");
                                    assert_eq!(second_page.dialog(), None);
                                    // The user goes to another origin, and that page asks while it loads.
                                    second_page.navigate(&load_dialog_url).unwrap();
                                }
                                10 => {
                                    // A dialog of a script that runs while the page loads waits for
                                    // its answer: the commit of its own document must not end it.
                                    assert_eq!(title, "LOAD true");
                                    // The page loads a new document itself, with one dialog again.
                                    second_page.evaluate_for_test("location.reload()").unwrap();
                                }
                                11 => {
                                    // The count of the origin goes on in the new document.
                                    let offers: Vec<_> = dialogs_seen.iter().map(|seen| (seen.1.as_str(), seen.3)).collect();
                                    assert_eq!(offers, [("load?", false), ("load?", true)]);
                                    second_page.evaluate_for_test("setTimeout(() => confirm('leave?'), 0)").unwrap();
                                }
                                12 => {
                                    assert!(second_page.dialog().unwrap().can_suppress);
                                    // A navigation answers the waiting dialog, so the page can run it.
                                    second_page.reload().unwrap();
                                    assert_eq!(second_page.dialog(), None);
                                }
                                13 => {
                                    assert_eq!(*hidden_answers.lock().unwrap(), "false-null", "a hidden page gets the default answers");
                                    println!("PASS: WebKit JavaScript, DOM, storage, history, failed-load error event and URL polling, hover status, Cmd-click, middle-click and right-click menu bridge, a download that stays in the downloads folder while a hidden page's is refused, and alert, confirm and prompt answers, with dialogs stopped by the user for their origin across documents, closed with their tab, ended by a navigation, asked while the page loads and refused for a hidden page");
                                    result.set(true);
                                    cx.update(|window, cx| { window.remove_window(); cx.quit(); }).unwrap();
                                    break;
                                }
                                _ => unreachable!(),
                            }
                            stage += 1;
                        }
                    }).detach();
                    TestWindow { page, _failed_tab: failed_tab, _hidden: [hidden, hidden_dialogs] }
                })
            },
        ).expect("native test window");
        let _ = handle;
        cx.activate(true);
    });
    assert!(passed.get(), "native webview test did not finish");
}
