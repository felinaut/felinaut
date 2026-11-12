//! Opt-in native per-tab audio test. WKWebView requires macOS's main thread.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_audio
//!
//! Two loopback origins serve a quiet looping tone: one same-origin (routed through Web Audio,
//! boostable) and one cross-origin without CORS (volume fallback, reported as limited).

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

use audio::{AudioReport, PageSound, Preset, TabAudio};
use gpui::{prelude::*, *};
use std::{
    cell::Cell,
    io::{Read, Write},
    net::TcpListener,
    rc::Rc,
    time::{Duration, Instant},
};
use webview::{Event, NativePage};

/// One second of a quiet 440 Hz sine as 8 kHz mono 16-bit PCM.
fn tone() -> Vec<u8> {
    let rate = 8_000u32;
    let samples: Vec<i16> = (0..rate)
        .map(|i| ((i as f32 * 440. * std::f32::consts::TAU / rate as f32).sin() * 1500.) as i16)
        .collect();
    let data_len = samples.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

/// A tiny HTTP server with byte-range support, which WebKit's media loader requires.
fn serve(listener: TcpListener, page: String) {
    let wav = tone();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let Ok(count) = stream.read(&mut request) else {
                continue;
            };
            let request = String::from_utf8_lossy(&request[..count]).to_string();
            let response = if request.starts_with("GET /tone.wav") {
                let range = request.lines().find_map(|line| {
                    let value = line.to_ascii_lowercase();
                    let spec = value.strip_prefix("range: bytes=")?.trim().to_string();
                    let (start, end) = spec.split_once('-')?;
                    let start: usize = start.parse().ok()?;
                    let end: usize = end.parse().unwrap_or(wav.len() - 1).min(wav.len() - 1);
                    Some((start, end))
                });
                let (status, start, end) = match range {
                    Some((start, end)) if start <= end => ("206 Partial Content", start, end),
                    _ => ("200 OK", 0, wav.len() - 1),
                };
                let mut head = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: audio/wav\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n",
                    end - start + 1
                );
                if status.starts_with("206") {
                    head += &format!("Content-Range: bytes {start}-{end}/{}\r\n", wav.len());
                }
                head += "\r\n";
                let mut bytes = head.into_bytes();
                bytes.extend_from_slice(&wav[start..=end]);
                bytes
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                )
                .into_bytes()
            };
            let _ = stream.write_all(&response);
        }
    });
}

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

const CHECK: &str = r#"
(() => {
  const local = document.getElementById('local');
  const remote = document.getElementById('remote');
  let ownedByBrowser = false;
  try { new AudioContext().createMediaElementSource(local); } catch (e) { ownedByBrowser = e.name === 'InvalidStateError'; }
  const page = new AudioContext();
  const destination = page.destination;
  const wrapped = destination instanceof GainNode && destination.maxChannelCount > 0;
  const oscillator = page.createOscillator();
  let connected = true;
  try { oscillator.connect(destination); } catch (_) { connected = false; }
  const offline = new OfflineAudioContext(1, 128, 8000).destination instanceof AudioDestinationNode;
  document.title = [
    'routed=' + ownedByBrowser,
    'localVolume=' + local.volume,
    'remoteVolume=' + remote.volume,
    'wrapped=' + wrapped,
    'connected=' + connected,
    'offline=' + offline,
    'playing=' + (!local.paused && !remote.paused),
  ].join(' ');
})();
"#;

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST")
        .or_else(|| std::env::var_os("FELIFOX_NATIVE_TEST"))
        .as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        println!("Native audio test skipped; set FELINAUT_NATIVE_TEST=1 to run.");
        return;
    }
    let remote_listener = TcpListener::bind("127.0.0.1:0").expect("remote listener");
    let remote = format!("http://{}", remote_listener.local_addr().unwrap());
    serve(remote_listener, String::new());
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let base = format!("http://{}", listener.local_addr().unwrap());
    serve(
        listener,
        format!(
            "<!doctype html><meta charset=utf-8><title>Waiting</title>\
             <audio id=local src='/tone.wav' loop></audio>\
             <audio id=remote src='{remote}/tone.wav' loop></audio>\
             <script>Promise.all([local.play(), remote.play()]).then(() => document.title = 'PLAYING', e => document.title = 'PLAY FAILED ' + e.name);</script>"
        ),
    );

    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    Application::new().run(move |cx| {
        let url = format!("{base}/");
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(640.), px(420.)), cx))),
                ..Default::default()
            },
            |window, cx| {
                let page = Rc::new(NativePage::new(window, &url).expect("create native page"));
                let driver = page.clone();
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        let start = Instant::now();
                        let mut stage = 0;
                        let mut title = String::new();
                        let mut hello = false;
                        let mut sound = PageSound::default();
                        loop {
                            Timer::after(Duration::from_millis(50)).await;
                            for event in driver.drain_events() {
                                match event {
                                    Event::Title(value) => title = value,
                                    Event::Audio(AudioReport::Hello) => hello = true,
                                    Event::Audio(AudioReport::Frame { id, audible, limited }) => sound.report(&id, audible, limited),
                                    _ => {}
                                }
                            }
                            assert!(start.elapsed() < Duration::from_secs(25), "audio test timed out at stage {stage}; title={title:?}");
                            match stage {
                                0 => {
                                    assert!(!title.starts_with("PLAY FAILED"), "media autoplay: {title}");
                                    if !(hello && title == "PLAYING" && sound.audible()) { continue; }
                                    assert!(!sound.limited(), "default settings must not limit or touch media");
                                    let boosted = TabAudio { preset: Preset::Voice, ..TabAudio::default().with_percent(250) };
                                    driver.apply_audio(boosted).unwrap();
                                }
                                1 => {
                                    // Routing waits for the browser's AudioContext to start.
                                    if !sound.limited() { continue; }
                                    driver.apply_audio(TabAudio::default().with_percent(50)).unwrap();
                                    driver.evaluate_for_test(CHECK).unwrap();
                                }
                                2 => {
                                    if !title.starts_with("routed=") { continue; }
                                    assert_eq!(
                                        title,
                                        "routed=true localVolume=1 remoteVolume=0.5 wrapped=true connected=true offline=true playing=true",
                                        "same-origin media is boosted through Web Audio; cross-origin media scales volume"
                                    );
                                    assert!(sound.audible(), "playing media stays audible");
                                    driver.apply_audio(TabAudio::default()).unwrap();
                                    driver.evaluate_for_test("document.title = 'remote=' + document.getElementById('remote').volume").unwrap();
                                }
                                3 => {
                                    if !title.starts_with("remote=") { continue; }
                                    assert_eq!(title, "remote=1", "resetting restores the page's own volume");
                                    if sound.limited() { continue; }
                                    driver.evaluate_for_test("document.querySelectorAll('audio').forEach(a => a.pause())").unwrap();
                                }
                                4 => {
                                    if sound.audible() { continue; }
                                    println!("PASS: tab audio hello, audible reports, Web Audio boost routing, cross-origin fallback, page AudioContext wrapping and reset");
                                    result.set(true);
                                    cx.update(|window, cx| { window.remove_window(); cx.quit(); }).unwrap();
                                    break;
                                }
                                _ => unreachable!(),
                            }
                            stage += 1;
                        }
                    }).detach();
                    TestWindow { page }
                })
            },
        ).expect("native test window");
        cx.activate(true);
    });
    assert!(passed.get(), "native audio test did not finish");
}
