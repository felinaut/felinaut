//! Opt-in, loopback-only native script lifecycle, cancellation, scope and result-bound checks.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --locked --test native_page_requests
//! Requires harness=false and engine-webkit-system; this is not IPC/full-engine coverage.

#[allow(dead_code, unused_imports)]
#[path = "../src/audio.rs"]
mod audio;
#[allow(dead_code, unused_imports)]
#[path = "../src/comparison.rs"]
mod comparison;
#[allow(dead_code, unused_imports)]
#[path = "../src/downloads.rs"]
mod downloads;
#[allow(dead_code, unused_imports)]
#[path = "../src/engine_build.rs"]
mod engine_build;
#[allow(dead_code, unused_imports)]
#[path = "../src/icons.rs"]
mod icons;
#[allow(dead_code, unused_imports)]
#[path = "../src/load_error.rs"]
mod load_error;
#[allow(dead_code, unused_imports)]
#[path = "../src/page.rs"]
mod page;
#[allow(dead_code)]
#[path = "../src/webview.rs"]
mod webview;

use gpui::{prelude::*, *};
use page::{Event, Page, RequestScope, ScriptError};
use std::{
    cell::RefCell,
    ffi::OsStr,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Read, Write},
    net::TcpListener,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const TEST_BUDGET: Duration = Duration::from_secs(20);
const WATCHDOG_BUDGET: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
type PageOwner = Rc<RefCell<Option<Page>>>;
type Check = Result<(), String>;

struct TestWindow {
    page: PageOwner,
}

impl Render for TestWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Canvas closures can outlive a render. Clone only the removable owner, never Page.
        let owner = self.page.clone();
        div().size_full().child(
            canvas(
                move |bounds, _, _| {
                    if let Some(page) = owner.borrow().as_ref() {
                        page.set_bounds(bounds).expect("native bounds");
                        page.set_visible(true).expect("native visibility");
                    }
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    }
}

fn isolated_home() -> PathBuf {
    let home = PathBuf::from(format!(
        "/private/tmp/felinaut-compare-requests-{}",
        std::process::id()
    ));
    // create, not create_dir_all: refuse to reuse an existing profile or symlink.
    DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .expect("create fresh private comparison home");
    assert_eq!(home.canonicalize().expect("canonical home"), home);
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))
        .expect("private home permissions");
    let mut marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(home.join(".felinaut-comparison"))
        .expect("fresh comparison marker");
    marker
        .write_all(b"felinaut-comparison-v1\n")
        .expect("write comparison marker");
    // SAFETY: main calls this before starting any threads or initializing GPUI/WebKit.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("CFFIXED_USER_HOME", &home);
        std::env::set_var("FELINAUT_COMPARISON_HOME", &home);
        std::env::set_var("FELINAUT_COMPARISON", "system");
        std::env::remove_var("FELINAUT_EXPECTED_WEBKIT_UUID");
    }
    comparison::initialize().expect("initialize isolated, incognito system comparison");
    assert!(comparison::policy().enabled());
    comparison::policy()
        .verify_runtime(&page::backend_info())
        .expect("verified system WebKit runtime before native checks");
    eprintln!(
        "Native page requests: retaining isolated home {} for inspection; helper exit is not verified.",
        home.display()
    );
    home
}

struct FixtureServer {
    base: String,
    stop: Sender<()>,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl FixtureServer {
    fn start(deadline: Instant) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback fixture listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let base = format!(
            "http://{}",
            listener.local_addr().expect("loopback address")
        );
        let (stop, stopped) = mpsc::channel();
        let task = std::thread::spawn(move || {
            let mut connections = 0;
            while Instant::now() < deadline && connections < 64 {
                match stopped.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }
                let mut stream = match listener.accept() {
                    Ok((stream, peer)) => {
                        if !peer.ip().is_loopback() {
                            continue;
                        }
                        stream
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        match stopped.recv_timeout(Duration::from_millis(10)) {
                            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        }
                    }
                    Err(error) => return Err(error),
                };
                connections += 1;
                stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                stream.set_write_timeout(Some(Duration::from_millis(250)))?;
                let mut request = [0; 4096];
                let mut length = 0;
                while length < request.len() && Instant::now() < deadline {
                    match stream.read(&mut request[length..]) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => length += count,
                    }
                    if request[..length].windows(4).any(|part| part == b"\r\n\r\n") {
                        break;
                    }
                }
                let (status, body) = if request[..length].starts_with(b"GET /first HTTP/") {
                    ("200 OK", fixture("first"))
                } else if request[..length].starts_with(b"GET /second HTTP/") {
                    ("200 OK", fixture("second"))
                } else {
                    ("404 Not Found", String::new())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                // WebKit can cancel the first connection during navigation.
                let _ = stream.write_all(response.as_bytes());
            }
            Ok(())
        });
        Self {
            base,
            stop,
            task: Some(task),
        }
    }

    fn finish(mut self) {
        let _ = self.stop.send(());
        self.task
            .take()
            .expect("fixture server task")
            .join()
            .expect("fixture server panicked")
            .expect("fixture server failed");
    }
}

fn fixture(name: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8>\
         <meta http-equiv=Content-Security-Policy content=\"default-src 'none'; script-src 'unsafe-inline'\">\
         <title>Request fixture</title><script>window.requestFixture = '{name}';</script>"
    )
}

fn with_page<T>(
    owner: &PageOwner,
    action: impl FnOnce(&Page) -> Result<T, String>,
) -> Result<T, String> {
    let borrowed = owner.borrow();
    action(borrowed.as_ref().ok_or("native page already removed")?)
}

fn require(condition: bool, message: &str) -> Check {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn within_budget(deadline: Instant, stage: &str) -> Check {
    require(
        Instant::now() < deadline,
        &format!("native page requests exceeded 20s budget: {stage}"),
    )
}

fn scope(owner: &PageOwner) -> Result<RequestScope, String> {
    with_page(owner, |page| {
        page.request_scope()
            .ok_or("native request scope missing".into())
    })
}

fn drain(owner: &PageOwner, expected: Option<&str>) -> Result<bool, String> {
    with_page(owner, |page| {
        let mut finished = false;
        for event in page.drain_events() {
            match event {
                Event::Finished(url) if expected == Some(url.as_str()) => finished = true,
                Event::Failed(_) => return Err("loopback fixture load failed".into()),
                _ => {}
            }
        }
        Ok(finished)
    })
}

async fn wait_loaded(owner: &PageOwner, url: &str, deadline: Instant) -> Check {
    loop {
        within_budget(deadline, "waiting for native Finished event")?;
        if drain(owner, Some(url))? {
            return Ok(());
        }
        Timer::after(POLL).await;
    }
}

struct Probe {
    received: Receiver<Result<String, ScriptError>>,
}

impl Probe {
    fn dispatch(owner: &PageOwner, script: &str) -> Result<Self, String> {
        let (send, received) = mpsc::channel();
        with_page(owner, |page| {
            page.evaluate(
                script,
                Box::new(move |result| {
                    // Callbacks only enqueue: no GPUI, assertions or page access here.
                    let _ = send.send(result);
                }),
            )
        })?;
        Ok(Self { received })
    }

    fn quiet(&self) -> Check {
        match self.received.try_recv() {
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => Ok(()),
            Ok(_) => Err("script callback delivered more than once".into()),
        }
    }

    fn expect_now(&self, expected: Result<&str, ScriptError>) -> Check {
        let actual = self
            .received
            .try_recv()
            .map_err(|_| "expected synchronous cancellation callback".to_owned())?;
        self.check(actual, expected)?;
        self.quiet()
    }

    fn check(
        &self,
        actual: Result<String, ScriptError>,
        expected: Result<&str, ScriptError>,
    ) -> Check {
        // Do not print an unexpectedly large result, even on failure.
        match (actual, expected) {
            (Ok(actual), Ok(expected)) if actual == expected => Ok(()),
            (Err(actual), Err(expected)) if actual == expected => Ok(()),
            (Ok(actual), _) => Err(format!(
                "unexpected successful script result ({} bytes)",
                actual.len()
            )),
            (Err(ScriptError::Backend(message)), _) => Err(format!(
                "unexpected script backend error ({} bytes)",
                message.len()
            )),
            (Err(actual), _) => Err(format!("unexpected script error: {actual}")),
        }
    }

    async fn expect(
        &self,
        owner: &PageOwner,
        expected: Result<&str, ScriptError>,
        deadline: Instant,
    ) -> Check {
        loop {
            within_budget(deadline, "waiting for script callback")?;
            drain(owner, None)?;
            match self.received.try_recv() {
                Ok(actual) => {
                    self.check(actual, expected)?;
                    return self.quiet();
                }
                Err(TryRecvError::Disconnected) => {
                    return Err("accepted script lost its callback".into());
                }
                Err(TryRecvError::Empty) => {
                    Timer::after(POLL).await;
                }
            }
        }
    }
}

async fn exercise(
    main: &PageOwner,
    hidden: &PageOwner,
    first_url: &str,
    second_url: &str,
    deadline: Instant,
) -> Check {
    wait_loaded(main, first_url, deadline).await?;
    let normal = Probe::dispatch(main, "42")?;
    normal.expect(main, Ok("42"), deadline).await?;
    println!("PASS: native script lifecycle — loaded first fixture and evaluated 42");

    let first_scope = scope(main)?;
    require(first_scope.is_current(), "loaded first scope is stale")?;
    with_page(main, |page| {
        require(
            page.navigate("file:///felinaut-request-fixture-blocked")
                .is_err(),
            "file navigation was accepted",
        )
    })?;
    require(
        first_scope.is_current() && first_scope.matches(&scope(main)?),
        "rejected navigation changed the request scope",
    )?;

    // Native evaluation is asynchronous. Do not yield between dispatch and navigation:
    // the old completion must lose to document invalidation, not become a new result.
    let cancelled = Probe::dispatch(main, "'late-first'")?;
    with_page(main, |page| page.navigate(second_url))?;
    cancelled.expect_now(Err(ScriptError::DocumentChanged))?;
    require(
        !first_scope.is_current(),
        "navigation left old scope current",
    )?;
    require(
        !first_scope.matches(&scope(main)?),
        "old scope matches the replacement document",
    )?;

    wait_loaded(main, second_url, deadline).await?;
    let second_scope = scope(main)?;
    require(second_scope.is_current(), "loaded second scope is stale")?;
    let second = Probe::dispatch(main, "window.requestFixture")?;
    second.expect(main, Ok("\"second\""), deadline).await?;
    cancelled.quiet()?;
    normal.quiet()?;
    println!(
        "PASS: native script cancellation/scopes — rejected file URL preserves scope; navigation cancels exactly once with DocumentChanged; second fixture result is independent"
    );

    wait_loaded(hidden, second_url, deadline).await?;
    let hidden_scope = scope(hidden)?;
    require(hidden_scope.is_current(), "loaded hidden scope is stale")?;
    require(
        !hidden_scope.matches(&second_scope) && !second_scope.matches(&hidden_scope),
        "different pages at the same fixture URL share a request scope",
    )?;
    let hidden_normal = Probe::dispatch(hidden, "42")?;
    hidden_normal.expect(hidden, Ok("42"), deadline).await?;
    with_page(hidden, |page| {
        require(
            Rc::strong_count(page) == 1,
            "hidden Page has an extra Rc owner",
        )
    })?;
    let closed = Probe::dispatch(hidden, "'late-hidden'")?;
    drop(hidden.borrow_mut().take());
    closed.expect_now(Err(ScriptError::PageClosed))?;
    require(!hidden_scope.is_current(), "closed hidden scope is current")?;
    require(
        !hidden_scope.matches(&hidden_scope),
        "closed scope still matches itself",
    )?;
    require(
        second_scope.is_current(),
        "closing hidden page invalidated main",
    )?;
    println!(
        "PASS: native script close/scopes — independent loaded hidden page; last Page Rc drop synchronously delivers PageClosed and invalidates its scope"
    );

    let oversized = Probe::dispatch(main, "'x'.repeat(1048577)")?;
    oversized
        .expect(main, Err(ScriptError::ResultTooLarge), deadline)
        .await?;
    let after_bound = Probe::dispatch(main, "42")?;
    after_bound.expect(main, Ok("42"), deadline).await?;

    // Give queued native completions another run-loop turn after the real loads/results.
    // Ledger expiry and long timeout behavior are covered by unit tests, not native waits.
    Timer::after(Duration::from_millis(100)).await;
    within_budget(deadline, "checking late completions")?;
    drain(main, None)?;
    for probe in [
        &normal,
        &cancelled,
        &second,
        &hidden_normal,
        &closed,
        &oversized,
        &after_bound,
    ] {
        probe.quiet()?;
    }
    require(
        second_scope.is_current() && second_scope.matches(&scope(main)?),
        "script completion changed the current document scope",
    )?;
    println!(
        "PASS: native script result bounds — oversized result is ResultTooLarge, subsequent evaluation succeeds, no duplicate/late callback deliveries"
    );
    Ok(())
}

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST").as_deref() != Some(OsStr::new("1")) {
        println!(
            "Native page requests test skipped; set FELINAUT_NATIVE_TEST=1 to run on a macOS desktop. No native checks ran."
        );
        return;
    }
    let started = Instant::now();
    let deadline = started + TEST_BUDGET;
    let _retained_home = isolated_home();
    let (watchdog_stop, watchdog_rx) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if let Err(mpsc::RecvTimeoutError::Timeout) =
            watchdog_rx.recv_timeout(WATCHDOG_BUDGET.saturating_sub(started.elapsed()))
        {
            eprintln!("FAIL: native page requests process watchdog exceeded 30s");
            std::process::exit(1);
        }
    });
    let server = FixtureServer::start(deadline);
    let first_url = format!("{}/first", server.base);
    let second_url = format!("{}/second", server.base);
    let outcome = Rc::new(RefCell::new(None::<Check>));
    let result = outcome.clone();
    // harness=false keeps Application and all native page operations on the main thread.
    Application::new().run(move |cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(640.), px(420.)),
                    cx,
                ))),
                ..Default::default()
            },
            |window, cx| {
                let owner = Rc::new(RefCell::new(Some(
                    page::create(window, &first_url).expect("create isolated main page"),
                )));
                let hidden = Rc::new(RefCell::new(Some(
                    page::create_hidden(window, &second_url).expect("create isolated hidden page"),
                )));
                let driver = owner.clone();
                cx.new(|cx: &mut Context<TestWindow>| {
                    cx.spawn_in(window, async move |_, cx| {
                        let checked =
                            exercise(&driver, &hidden, &first_url, &second_url, deadline).await;
                        // Also clean up on a failed check; child views must die before NSWindow.
                        drop(hidden.borrow_mut().take());
                        let final_scope = scope(&driver);
                        drop(driver.borrow_mut().take());
                        let checked = checked.and_then(|()| {
                            require(
                                !final_scope?.is_current(),
                                "main Page survived owner removal (renderer retained a Page Rc)",
                            )
                        });
                        *result.borrow_mut() = Some(checked);
                        cx.update(|window, cx| {
                            window.remove_window();
                            cx.quit();
                        })
                        .expect("close native request test window");
                    })
                    .detach();
                    TestWindow { page: owner }
                })
            },
        )
        .expect("native request test window");
        cx.activate(true);
    });
    server.finish();
    let checked = outcome
        .borrow_mut()
        .take()
        .unwrap_or_else(|| Err("native request test quit before completing checks".into()));
    let _ = watchdog_stop.send(());
    watchdog.join().expect("request test watchdog panicked");
    if let Err(error) = checked {
        panic!("FAIL: native page requests: {error}");
    }
    assert!(
        started.elapsed() < TEST_BUDGET,
        "native page requests exceeded the total 20s budget"
    );
    println!("PASS: native page script lifecycle/cancellation, document scopes and result bounds");
}
