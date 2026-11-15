//! Opt-in native test of certificate exceptions against loopback TLS servers with a fresh
//! self-signed certificate. Needs `/usr/bin/openssl` and `python3`; nothing leaves 127.0.0.1.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_certificate

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
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};
use webview::{Event, NativePage};

/// Two HTTPS servers on 127.0.0.1 with keep-alive, so WebKit can reuse its connections.
const SERVER: &str = r#"
import http.server, ssl, sys, threading
class Page(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def do_GET(self):
        body = b"<!doctype html><title>TLS OK</title><p>Loopback TLS fixture"
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(sys.argv[1], sys.argv[2])
ports = []
for _ in range(2):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    ports.append(server.server_address[1])
    threading.Thread(target=server.serve_forever, daemon=True).start()
print(*ports, flush=True)
sys.stdin.read()
"#;

/// Stops the TLS servers and removes the certificate, even when the test fails.
struct Fixture {
    server: Child,
    folder: std::path::PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

fn start_servers(folder: &Path) -> (Fixture, u16, u16) {
    std::fs::create_dir_all(folder).expect("certificate folder");
    let (key, cert) = (folder.join("key.pem"), folder.join("cert.pem"));
    let status = Command::new("/usr/bin/openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .args([
            "-subj",
            "/CN=127.0.0.1",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
        ])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run openssl");
    assert!(status.success(), "openssl could not make a certificate");
    let mut server = Command::new("python3")
        .args(["-I", "-c", SERVER])
        .arg(&cert)
        .arg(&key)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run python3");
    let mut line = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut line)
        .expect("server ports");
    let fixture = Fixture {
        server,
        folder: folder.to_owned(),
    };
    let ports: Vec<u16> = line
        .split_whitespace()
        .map(|port| port.parse().unwrap())
        .collect();
    let [first, second] = ports[..] else {
        panic!("expected two ports: {line:?}");
    };
    (fixture, first, second)
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

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST").as_deref() != Some(std::ffi::OsStr::new("1")) {
        println!("Native certificate test skipped; set FELINAUT_NATIVE_TEST=1 to run.");
        return;
    }
    let folder = std::env::temp_dir().join(format!("felinaut-tls-{}", std::process::id()));
    let (fixture, port, other_port) = start_servers(&folder);
    let secure = format!("https://127.0.0.1:{port}/page");
    let resource = format!("https://127.0.0.1:{other_port}/resource");
    // A plain loopback page that loads an image from the second TLS server.
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let plain = format!("http://{}/embeds", listener.local_addr().unwrap());
    let embedded = resource.clone();
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else { break };
            // An idle preconnect must not hold up the real request.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let body = format!("<!doctype html><title>PLAIN</title><img src='{embedded}'>");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });

    let passed = Rc::new(Cell::new(false));
    let result = passed.clone();
    Application::new().run(move |cx| {
        let handle = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(800.), px(600.)),
                    cx,
                ))),
                ..Default::default()
            },
            |window, cx| {
                let page = Rc::new(NativePage::new(window, &secure).expect("create tab"));
                let first = page.clone();
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        let start = Instant::now();
                        let mut stage = 0;
                        let mut stage_started = Instant::now();
                        let mut certificate = None;
                        let mut second: Option<NativePage> = None;
                        let mut hidden: Option<NativePage> = None;
                        let mut used_exception = false;
                        // The first tab finished a load during this stage; its events may come
                        // in separate polls.
                        let mut finished = false;
                        loop {
                            Timer::after(Duration::from_millis(50)).await;
                            assert!(
                                start.elapsed() < Duration::from_secs(30),
                                "native certificate test timed out at stage {stage}; finished={finished} used_exception={used_exception}"
                            );
                            for event in first.drain_events() {
                                match event {
                                    Event::Failed(failure) if stage == 0 => {
                                        assert_eq!(failure.kind, load_error::Kind::Secure, "{failure:?}");
                                        certificate = Some(failure.certificate.expect("an overridable failure carries its certificate"));
                                    }
                                    Event::Finished(_) => finished = true,
                                    Event::UsedException => used_exception = true,
                                    _ => {}
                                }
                            }
                            let second_finished = second.as_ref().is_some_and(|page| {
                                page.drain_events().into_iter().any(|event| matches!(event, Event::Finished(_)))
                            });
                            let hidden_finished = hidden.as_ref().is_some_and(|page| {
                                page.drain_events().into_iter().any(|event| matches!(event, Event::Finished(_)))
                            });
                            assert!(!hidden_finished, "the hidden page loaded a site with an exception");
                            match stage {
                                0 if certificate.is_some() => {
                                    assert!(!webview::has_certificate_exception(&secure));
                                    assert!(!first.uses_accepted_certificate());
                                    // What "Visit this site anyway" does.
                                    webview::allow_certificate(&secure, certificate.clone().unwrap());
                                    assert!(webview::has_certificate_exception(&secure));
                                    first.navigate(&secure).unwrap();
                                }
                                1 if finished => {
                                    assert!(first.uses_accepted_certificate(), "the accepting tab is marked");
                                    let page = cx.update(|window, _| NativePage::new(window, &secure)).unwrap();
                                    second = Some(page.expect("create second tab"));
                                }
                                2 if second_finished => {
                                    // WebKit may reuse the first tab's trusted connection without a
                                    // challenge; the mark comes from the certificate either way.
                                    assert!(second.as_ref().unwrap().uses_accepted_certificate(), "the second tab is marked");
                                    let page = cx.update(|window, _| NativePage::hidden(window, &secure)).unwrap();
                                    hidden = Some(page.expect("create hidden page"));
                                }
                                3 if stage_started.elapsed() > Duration::from_secs(3) => {
                                    let page = hidden.as_ref().unwrap();
                                    assert!(page.document().is_none(), "the hidden page must not load the site");
                                    assert!(!page.uses_accepted_certificate());
                                    // Only the second server's exception lets the image load.
                                    webview::allow_certificate(&resource, certificate.clone().unwrap());
                                    first.navigate(&plain).unwrap();
                                }
                                4 if finished && used_exception => {
                                    assert!(!first.uses_accepted_certificate(), "a plain main frame has no server trust");
                                    println!("PASS: a bad certificate fails with its certificate; once accepted it loads in the accepting tab and a second tab, both marked; the hidden page refuses the site; a page whose image used an exception reports it");
                                    result.set(true);
                                    cx.update(|window, cx| {
                                        window.remove_window();
                                        cx.quit();
                                    })
                                    .unwrap();
                                    break;
                                }
                                _ => continue,
                            }
                            stage += 1;
                            stage_started = Instant::now();
                            finished = false;
                        }
                    })
                    .detach();
                    TestWindow { page }
                })
            },
        );
        let _ = handle.expect("native test window");
        cx.activate(true);
    });
    drop(fixture);
    assert!(passed.get(), "native certificate test did not finish");
}
