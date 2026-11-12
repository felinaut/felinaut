//! Opt-in native check that no page gets the camera, the microphone, the location or
//! notifications without a decision of the user. It never grants a request, so macOS shows no
//! permission dialog and no device is used. All traffic stays on loopback.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_permissions

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
#[allow(dead_code, unused_imports)]
#[path = "../src/permissions.rs"]
mod permissions;
#[allow(dead_code)]
#[path = "../src/webview.rs"]
mod webview;

use gpui::{prelude::*, *};
use std::{
    cell::Cell,
    io::{Read, Write},
    net::TcpListener,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use webview::{Event, NativePage};

/// Asks for the microphone, the location and notifications, and keeps the answers. WebKit
/// holds some requests of a page that is not on display, so each answer has a time limit.
const ASK: &str = r#"
(async () => {
  const limit = promise => Promise.race([
    promise, new Promise(done => setTimeout(() => done('pending'), 3000))]);
  const media = navigator.mediaDevices
    ? await limit(navigator.mediaDevices.getUserMedia({ audio: true }).then(() => 'granted', error => error.name))
    : 'unsupported';
  const place = await limit(new Promise(done => navigator.geolocation.getCurrentPosition(
    () => done('granted'), error => done('error' + error.code))));
  const notes = typeof Notification === 'undefined'
    ? 'unsupported'
    : await limit(Notification.requestPermission().catch(error => error.name));
  window.felinautAnswers = [media, place, notes].join(' ');
})();
"#;

struct TestWindow {
    page: Rc<NativePage>,
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

/// Reads `window.felinautAnswers` into the channel once the page has set it.
fn read_answers(page: &NativePage, answers: &mpsc::Sender<String>) {
    let answers = answers.clone();
    let _ = page.evaluate("window.felinautAnswers || ''", move |json| {
        let _ = answers.send(json.trim_matches('"').to_owned());
    });
}

/// `shown` is false for the hidden page, whose requests WebKit may hold without an answer.
fn check_answers(shown: bool, answers: &str) {
    let [media, place, notes] = answers.split(' ').collect::<Vec<_>>()[..] else {
        panic!("unexpected answers {answers:?} (shown: {shown})");
    };
    assert_ne!(
        media, "granted",
        "the microphone was granted (shown: {shown})"
    );
    // 1 is PERMISSION_DENIED: WebKit refused before it looked for a position.
    assert!(
        place == "error1" || (!shown && place == "pending"),
        "the location was not denied: {place} (shown: {shown})"
    );
    assert!(
        ["denied", "default", "unsupported"].contains(&notes) || (!shown && notes == "pending"),
        "notifications were not denied: {notes} (shown: {shown})"
    );
}

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST")
        .or_else(|| std::env::var_os("FELIFOX_NATIVE_TEST"))
        .as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        println!("Native permission test skipped; set FELINAUT_NATIVE_TEST=1 to run.");
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.read(&mut [0; 4096]);
            let body = "<!doctype html><meta charset=utf-8><title>Permissions</title><p>Fixture";
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
        let url = format!("{base}/");
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(640.), px(480.)),
                    cx,
                ))),
                ..Default::default()
            },
            |window, cx| {
                let page = Rc::new(NativePage::new(window, &url).expect("create native page"));
                let hidden = NativePage::hidden(window, &url).expect("create hidden page");
                let shown = page.clone();
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        let start = Instant::now();
                        let (shown_sender, shown_answers) = mpsc::channel();
                        let (hidden_sender, hidden_answers) = mpsc::channel();
                        let mut loaded = (false, false);
                        let mut asked = false;
                        let mut prompts = 0;
                        let mut done: (Option<String>, Option<String>) = (None, None);
                        loop {
                            Timer::after(Duration::from_millis(50)).await;
                            assert!(
                                start.elapsed() < Duration::from_secs(30),
                                "native permission test timed out: loaded={loaded:?} done={done:?}"
                            );
                            let finished = |event: &Event| matches!(event, Event::Finished(_));
                            loaded.0 |= shown.drain_events().iter().any(finished);
                            loaded.1 |= hidden.drain_events().iter().any(finished);
                            if loaded != (true, true) {
                                continue;
                            }
                            if !asked {
                                asked = true;
                                // Wry alone would answer 1 (grant) here. Felinaut's method
                                // answers 2 (deny) once, for each capture type and page.
                                for capture_type in 0..=3 {
                                    assert_eq!(shown.probe_capture_for_test(capture_type), [2]);
                                    assert_eq!(hidden.probe_capture_for_test(capture_type), [2]);
                                }
                                shown.evaluate_for_test(ASK).unwrap();
                                hidden.evaluate_for_test(ASK).unwrap();
                            }
                            // A request of the page on display waits for the user. Nothing
                            // answers it but this test, which refuses it as "Not Now" does.
                            for request in permissions::native::requests() {
                                assert_eq!(request.page, shown.permission_key(), "a hidden page got a prompt");
                                assert_eq!(request.origin, base);
                                assert_eq!(request.kinds, [permissions::Kind::Microphone]);
                                prompts += 1;
                                permissions::native::answer(request.id, false);
                            }
                            read_answers(&shown, &shown_sender);
                            read_answers(&hidden, &hidden_sender);
                            let ready = |text: &String| !text.is_empty();
                            done.0 = done.0.or(shown_answers.try_iter().find(ready));
                            done.1 = done.1.or(hidden_answers.try_iter().find(ready));
                            let (Some(shown_done), Some(hidden_done)) = (&done.0, &done.1) else {
                                continue;
                            };
                            check_answers(true, shown_done);
                            check_answers(false, hidden_done);
                            assert!(permissions::native::requests().is_empty());
                            println!(
                                "PASS: no grant by default. Page: {shown_done}; hidden page: {hidden_done}; prompts for the page: {prompts}"
                            );
                            result.set(true);
                            cx.update(|window, cx| {
                                window.remove_window();
                                cx.quit();
                            })
                            .unwrap();
                            break;
                        }
                    })
                    .detach();
                    TestWindow { page }
                })
            },
        )
        .expect("native test window");
        cx.activate(true);
    });
    assert!(passed.get(), "native permission test did not finish");
}
