//! Trusted-loopback native prototype, deliberately separate from the browser adapter.
#[path = "runtime_policy.rs"]
mod policy;

use cef::*;
use felinaut_cef_probe::{self as native, pump};
use gpui::{
    Application, Bounds, Context, IntoElement, Render, Timer, WindowBounds, WindowOptions, canvas,
    div, prelude::*, px, rgb, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::{Cell, RefCell},
    ffi::{CString, c_void},
    path::PathBuf,
    process::ExitCode,
    rc::Rc as StdRc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn felinaut_cef_verify_protocols() -> bool;
    fn felinaut_cef_defer_cleanup(callback: extern "C" fn()) -> bool;
    fn felinaut_cef_attach_container(parent: *mut c_void) -> *mut c_void;
    fn felinaut_cef_resize_container(x: f64, y: f64, width: f64, height: f64) -> bool;
    fn felinaut_cef_resize_window(width: f64, height: f64) -> bool;
    fn felinaut_cef_click(x: f64, y: f64) -> bool;
    fn felinaut_cef_type_ascii(text: *const std::ffi::c_char) -> bool;
    fn felinaut_cef_release_container() -> bool;
}

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static TEST_PASSED: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_COMPLETED: AtomicBool = AtomicBool::new(false);

static CALLBACK_FAILURE: AtomicBool = AtomicBool::new(false);
thread_local! { static BROWSER: RefCell<Option<Browser>> = const { RefCell::new(None) }; }

#[derive(Debug)]
enum Event {
    ContextReady,
    Created,
    Loaded(i32),
    Title(String),
    CloseReady,
    Closed,
    Failed(&'static str),
}
fn emit(events: &SyncSender<Event>, event: Event) {
    if events.try_send(event).is_err() {
        CALLBACK_FAILURE.store(true, Ordering::Release);
    }
}
fn browser() -> Option<Browser> {
    BROWSER.with_borrow(Clone::clone)
}

extern "C" fn pump_cef() {
    if INITIALIZED.load(Ordering::Acquire)
        && std::panic::catch_unwind(cef::do_message_loop_work).is_err()
    {
        CALLBACK_FAILURE.store(true, Ordering::Release);
    }
}

extern "C" fn shutdown_cef() {
    let result = std::panic::catch_unwind(|| {
        INITIALIZED.store(false, Ordering::Release);
        pump::stop();
        println!("CEF shutdown starting outside the serial main queue");
        cef::shutdown();
        SHUTDOWN_COMPLETED.store(true, Ordering::Release);
        if !TEST_PASSED.load(Ordering::Acquire) {
            crate::fail("CEF fixture checks failed; native engine shutdown completed");
        }
        println!(
            "PASS: CEF OnBeforeClose, callback release, pump cancellation and shutdown before GPUI termination"
        );
        if !native::finish_termination() {
            crate::fail("Could not release GPUI termination barrier");
        }
    });
    if result.is_err() {
        crate::fail("Native CEF shutdown trampoline panicked");
    }
}

wrap_app! {
    struct ProbeApp { events: SyncSender<Event> }
    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(ProbeProcess::new(self.events.clone()))
        }
        fn on_before_command_line_processing(&self, _process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            if let Some(command) = command_line {
                // Disposable loopback fixture only: never access the user's Keychain.
                // Chromium's fake test key is public, not production secret storage.
                let switch = CefString::from("disable-features");
                let existing = CefString::from(&command.switch_value(Some(&switch))).to_string();
                let disabled = policy::fixture_disabled_features(&existing);
                command.append_switch_with_value(Some(&switch), Some(&CefString::from(disabled.as_str())));
                for name in ["disable-notifications", "use-mock-keychain", "disable-background-networking", "disable-component-update", "no-first-run", "no-default-browser-check", "no-proxy-server"] {
                    command.append_switch(Some(&CefString::from(name)));
                }
            }
        }
    }
}
wrap_browser_process_handler! {
    struct ProbeProcess { events: SyncSender<Event> }
    impl BrowserProcessHandler {
        fn on_context_initialized(&self) { emit(&self.events, Event::ContextReady); }
        fn on_schedule_message_pump_work(&self, delay_ms: i64) { pump::schedule(delay_ms); }
        fn on_before_child_process_launch(&self, _command_line: Option<&mut CommandLine>) {
            eprintln!("CEF_HELPER_LAUNCH");
        }
    }
}
wrap_client! {
    struct ProbeClient { events: SyncSender<Event>, target: url::Url }
    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(ProbeLife::new(self.events.clone())) }
        fn load_handler(&self) -> Option<LoadHandler> { Some(ProbeLoad::new(self.events.clone())) }
        fn display_handler(&self) -> Option<DisplayHandler> { Some(ProbeDisplay::new(self.events.clone())) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(ProbeRequest::new(self.events.clone(), self.target.clone())) }
        fn download_handler(&self) -> Option<DownloadHandler> { Some(ProbeDownload::new()) }
    }
}
wrap_life_span_handler! {
    struct ProbeLife { events: SyncSender<Event> }
    impl LifeSpanHandler {
        fn on_after_created(&self, value: Option<&mut Browser>) {
            if let Some(value) = value {
                BROWSER.with_borrow_mut(|slot| *slot = Some(value.clone()));
                emit(&self.events, Event::Created);
            } else { emit(&self.events, Event::Failed("Missing created browser")); }
        }
        fn do_close(&self, _browser: Option<&mut Browser>) -> i32 {
            // Keep GPUI's top-level window. The owner tears down only the retained
            // CEF container after this callback returns, then awaits OnBeforeClose.
            emit(&self.events, Event::CloseReady);
            1
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            BROWSER.with_borrow_mut(|slot| { slot.take(); });
            emit(&self.events, Event::Closed);
        }
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: i32, _target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, _target_disposition: WindowOpenDisposition, _user_gesture: i32, _popup_features: Option<&PopupFeatures>, _window_info: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 { 1 }
    }
}
wrap_load_handler! {
    struct ProbeLoad { events: SyncSender<Event> }
    impl LoadHandler {
        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, status: i32) {
            if frame.is_some_and(|frame| frame.is_main() != 0) { emit(&self.events, Event::Loaded(status)); }
        }
        fn on_load_error(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, error: Errorcode, _text: Option<&CefString>, _url: Option<&CefString>) {
            if error != Errorcode::ABORTED && frame.is_some_and(|frame| frame.is_main() != 0) { emit(&self.events, Event::Failed("Main-frame load failed")); }
        }
    }
}
wrap_display_handler! {
    struct ProbeDisplay { events: SyncSender<Event> }
    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(title) = title {
                let value = title.to_string();
                if value.len() <= 1024 { emit(&self.events, Event::Title(value)); }
                else { emit(&self.events, Event::Failed("Oversized fixture title")); }
            }
        }
    }
}
wrap_request_handler! {
    struct ProbeRequest { events: SyncSender<Event>, target: url::Url }
    impl RequestHandler {
        fn on_before_browse(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _gesture: i32, _redirect: i32) -> i32 {
            let allowed = request.is_some_and(|request| policy::allows_navigation(&CefString::from(&request.url()).to_string(), &self.target));
            if !allowed { emit(&self.events, Event::Failed("Navigation left the trusted fixture origin")); }
            i32::from(!allowed)
        }
        fn resource_request_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, _navigation: i32, _download: i32, _initiator: Option<&CefString>, _disable_default: Option<&mut i32>) -> Option<ResourceRequestHandler> {
            Some(ProbeResource::new(self.events.clone(), self.target.clone()))
        }
        fn on_render_process_terminated(&self, _browser: Option<&mut Browser>, _status: TerminationStatus, _code: i32, _text: Option<&CefString>) {
            emit(&self.events, Event::Failed("CEF renderer terminated unexpectedly"));
        }
    }
}
wrap_resource_request_handler! {
    struct ProbeResource { events: SyncSender<Event>, target: url::Url }
    impl ResourceRequestHandler {
        fn on_before_resource_load(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _callback: Option<&mut Callback>) -> ReturnValue {
            if request.is_some_and(|request| policy::allows_resource(&CefString::from(&request.url()).to_string(), &self.target)) {
                ReturnValue::CONTINUE
            } else {
                emit(&self.events, Event::Failed("Resource request left the trusted fixture origin"));
                ReturnValue::CANCEL
            }
        }
        fn on_resource_redirect(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, _response: Option<&mut Response>, new_url: Option<&mut CefString>) {
            if let Some(value) = new_url
                && !policy::allows_resource(&value.to_string(), &self.target)
            {
                // Replace the disallowed network target with a non-network URL.
                // Any subsequent resource-load callback also fails closed.
                *value = CefString::from("about:blank");
                emit(&self.events, Event::Failed("Resource redirect left the trusted fixture origin"));
            }
        }
    }
}
wrap_download_handler! {
    struct ProbeDownload;
    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _method: Option<&CefString>) -> i32 { 0 }
    }
}

#[cfg(feature = "coexistence")]
type WebKitOwner = StdRc<RefCell<Option<crate::coexistence_webkit::Page>>>;

struct ProbeWindow {
    coexistence: bool,
    #[cfg(feature = "coexistence")]
    webkit: WebKitOwner,
}
impl Render for ProbeWindow {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        let coexistence = self.coexistence;
        #[cfg(feature = "coexistence")]
        let webkit = self.webkit.clone();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x0b1026))
            .text_color(rgb(0xf7f6ff))
            .child(div().h(px(50.)).p_3().child(if coexistence {
                "Felinaut · CEF + system WKWebView · isolated loopback coexistence"
            } else {
                "Felinaut · Chromium / CEF · trusted-loopback prototype"
            }))
            .child(
                canvas(
                    move |bounds, _, _| {
                        if native::close_requested() {
                            return;
                        }
                        let x = f64::from(bounds.origin.x);
                        let y = f64::from(bounds.origin.y);
                        let width = f64::from(bounds.size.width);
                        let height = f64::from(bounds.size.height);
                        let cef_width = if coexistence {
                            (width / 2.).floor()
                        } else {
                            width
                        };
                        #[cfg(feature = "coexistence")]
                        if coexistence
                            && let Some(page) = webkit.borrow().as_ref()
                            && page
                                .set_bounds(x + cef_width, y, width - cef_width, height)
                                .is_err()
                        {
                            CALLBACK_FAILURE.store(true, Ordering::Release);
                        }
                        // SAFETY: main-thread render/layout only updates the retained native
                        // container. It never pumps CEF or re-enters GPUI from a callback.
                        let ok = unsafe { felinaut_cef_resize_container(x, y, cef_width, height) };
                        if !ok {
                            CALLBACK_FAILURE.store(true, Ordering::Release);
                        }
                    },
                    |_, _, _, _| {},
                )
                .flex_1()
                .size_full(),
            )
    }
}

fn configuration() -> Result<(url::Url, PathBuf, PathBuf), String> {
    let target = policy::target_url(
        &std::env::var("FELINAUT_CEF_PROBE_URL").map_err(|_| "Missing fixture URL")?,
    )?;
    let path = |name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .ok_or_else(|| format!("Missing {name}"))
    };
    let home = policy::profile(
        &path("FELINAUT_CEF_PROBE_HOME")?,
        &path("HOME")?,
        &path("CFFIXED_USER_HOME")?,
    )?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let bundle = executable
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or("Expected the probe app bundle")?
        .to_path_buf();
    let framework = bundle.join(
        "Contents/Frameworks/Chromium Embedded Framework.framework/Chromium Embedded Framework",
    );
    if !bundle.join("Contents/Info.plist").is_file() || !framework.is_file() {
        return Err("Use the prepared probe bundle, not a loose executable".into());
    }
    std::fs::create_dir(home.join("cef-cache")).map_err(|_| "CEF probe cache must be fresh")?;
    Ok((target, home, bundle))
}

pub fn run() -> ExitCode {
    run_with_mode(false)
}

#[cfg(feature = "coexistence")]
pub fn run_coexistence() -> ExitCode {
    run_with_mode(true)
}

#[cfg(feature = "coexistence")]
fn cef_storage_report() -> Result<(), String> {
    let frame = browser()
        .and_then(|browser| browser.main_frame())
        .ok_or("CEF main frame unavailable for storage check")?;
    frame.execute_java_script(
        Some(&CefString::from("window.probe.report('storage')")),
        None,
        0,
    );
    Ok(())
}

fn run_with_mode(coexistence: bool) -> ExitCode {
    let (mut target, home, bundle) = match configuration() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("FAIL: {error}");
            return ExitCode::FAILURE;
        }
    };
    if coexistence {
        target.set_path("/cef");
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(45));
        eprintln!("FAIL: CEF probe exceeded its 45-second watchdog");
        std::process::exit(1);
    });
    // The maintained loader uses this bundle-relative path and loads at runtime,
    // not as an eager dependency before helper sandbox initialization.
    let _loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), false);
    if !_loader.load() {
        eprintln!("FAIL: CEF framework could not be loaded under existing system protections");
        return ExitCode::FAILURE;
    }
    if api_hash(sys::CEF_API_VERSION_LAST, 0).is_null() {
        eprintln!("FAIL: CEF API negotiation failed");
        return ExitCode::FAILURE;
    }
    if let Err(error) = native::bootstrap() {
        eprintln!("FAIL: {error}");
        return ExitCode::FAILURE;
    }
    // SAFETY: the owned main-thread singleton implements the protocols from the exact SDK.
    if !unsafe { felinaut_cef_verify_protocols() } || !pump::start_cef(pump_cef) {
        eprintln!("FAIL: CEF application protocols or pump setup failed");
        return ExitCode::FAILURE;
    }
    let (events, incoming) = mpsc::sync_channel(128);
    let mut app = ProbeApp::new(events.clone());
    let settings = Settings {
        no_sandbox: 0,
        external_message_pump: 1,
        multi_threaded_message_loop: 0,
        command_line_args_disabled: 1,
        root_cache_path: CefString::from(home.join("cef-cache").to_str().unwrap()),
        log_file: CefString::from(home.join("cef.log").to_str().unwrap()),
        log_severity: LogSeverity::WARNING,
        main_bundle_path: CefString::from(bundle.to_str().unwrap()),
        framework_dir_path: CefString::from(bundle.join("Contents/Frameworks/Chromium Embedded Framework.framework").to_str().unwrap()),
        browser_subprocess_path: CefString::from(bundle.join("Contents/Frameworks/felinaut-cef-probe Helper.app/Contents/MacOS/felinaut-cef-probe Helper").to_str().unwrap()),
        ..Default::default()
    };
    let args = args::Args::new();
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        pump::stop();
        eprintln!(
            "FAIL: CEF initialization rejected the native configuration (exit code {})",
            get_exit_code()
        );
        return ExitCode::FAILURE;
    }
    drop(app);
    INITIALIZED.store(true, Ordering::Release);
    pump::schedule(0);
    println!(
        "CEF initialized with sandbox requested and OS notifications disabled; helper evidence is verified separately by the runner"
    );
    let parent = StdRc::new(Cell::new(std::ptr::null_mut()));
    let container = parent.clone();
    #[cfg(feature = "coexistence")]
    let webkit: WebKitOwner = StdRc::new(RefCell::new(None));
    #[cfg(feature = "coexistence")]
    let window_webkit = webkit.clone();
    #[cfg(feature = "coexistence")]
    let mut webkit_target = target.clone();
    #[cfg(feature = "coexistence")]
    webkit_target.set_path("/webkit");
    Application::new().run(move |cx| {
        if !native::verify_application() {
            crate::fail("CEF changed the GPUI application/delegate boundary");
        }
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    if coexistence {
                        size(px(1100.), px(700.))
                    } else {
                        size(px(900.), px(650.))
                    },
                    cx,
                ))),
                ..Default::default()
            },
            move |window, cx| {
                let handle = match window.window_handle().unwrap().as_raw() {
                    RawWindowHandle::AppKit(handle) => handle.ns_view.as_ptr(),
                    _ => crate::fail("Expected a native AppKit content view"),
                };
                // SAFETY: borrowed GPUI view is live on-main. Native code retains parent/container until CEF closes.
                container.set(unsafe { felinaut_cef_attach_container(handle) });
                if container.get().is_null() {
                    crate::fail("Could not attach CEF container");
                }
                #[cfg(feature = "coexistence")]
                if coexistence {
                    match crate::coexistence_webkit::Page::new(window, &webkit_target) {
                        Ok(page) => {
                            println!(
                                "WKWebView source: {}; nonpersistent store verified",
                                page.source_identity()
                            );
                            *window_webkit.borrow_mut() = Some(page);
                        }
                        Err(error) => {
                            eprintln!("WebKit coexistence creation failed: {error}");
                            CALLBACK_FAILURE.store(true, Ordering::Release);
                        }
                    }
                    if !native::verify_application() {
                        CALLBACK_FAILURE.store(true, Ordering::Release);
                    }
                }
                window.on_window_should_close(cx, |_, cx| {
                    cx.quit();
                    false
                });
                cx.new(|_| ProbeWindow {
                    coexistence,
                    #[cfg(feature = "coexistence")]
                    webkit: window_webkit,
                })
            },
        )
        .unwrap_or_else(|_| crate::fail("Could not open GPUI CEF window"));
        cx.activate(true);
        cx.on_app_quit(|_| async {
            println!("PASS: original GPUI shutdown callback after CEF shutdown");
        })
        .detach();
        cx.spawn(async move |cx| {
            let started = Instant::now();
            let mut context_ready = false;
            let mut requested = false;
            let mut accepted = false;
            let mut loaded = false;
            let mut title = String::new();
            let mut closing = false;
            let mut close_gate = policy::CloseGate::default();
            let mut closed = false;
            let mut detached = false;
            let mut stage = 0;
            let mut action_at = Instant::now();
            let mut failure = None;
            #[cfg(feature = "coexistence")]
            let mut coex = crate::coexistence_policy::Probe::new();
            #[cfg(feature = "coexistence")]
            let mut webkit_loaded = false;
            #[cfg(feature = "coexistence")]
            let mut coex_complete = false;

            loop {
                Timer::after(Duration::from_millis(15)).await;
                for event in incoming.try_iter() {
                    match event {
                        Event::ContextReady => {
                            println!("CEF context ready");
                            context_ready = true;
                        }
                        Event::Created => {
                            println!("CEF native child created");
                            close_gate.created();
                        }
                        Event::Loaded(200) => {
                            println!("CEF main frame loaded (HTTP 200)");
                            loaded = true;
                        }
                        Event::Loaded(_) => failure = Some("Fixture returned a non-200 status"),
                        Event::Title(value) => {
                            println!("CEF fixture title event ({} bytes)", value.len());
                            #[cfg(feature = "coexistence")]
                            if coexistence
                                && value.starts_with("COEX:")
                                && let Err(error) =
                                    coex.update(crate::coexistence_policy::Engine::Cef, &value)
                            {
                                failure = Some(error);
                            }
                            title = value;
                        }
                        Event::CloseReady => {
                            println!("CEF custom close ready");
                            // SAFETY: DoClose has returned; custom host teardown removes only the CEF view.
                            if !unsafe { felinaut_cef_release_container() } {
                                failure = Some("CEF container teardown failed");
                            }
                            detached = true;
                        }
                        Event::Closed => {
                            println!("CEF OnBeforeClose received");
                            closed = true;
                        }
                        Event::Failed(error) => {
                            eprintln!("CEF callback failed: {error}");
                            failure = Some(error);
                        }
                    }
                }
                #[cfg(feature = "coexistence")]
                if coexistence && let Some(page) = webkit.borrow().as_ref() {
                    for event in page.drain() {
                        match event {
                            crate::coexistence_webkit::Event::Loaded => webkit_loaded = true,
                            crate::coexistence_webkit::Event::Title(value) => {
                                if value.starts_with("COEX:")
                                    && let Err(error) = coex
                                        .update(crate::coexistence_policy::Engine::WebKit, &value)
                                {
                                    failure = Some(error);
                                }
                            }
                            crate::coexistence_webkit::Event::Failed(error) => {
                                failure = Some(error)
                            }
                        }
                    }
                }
                if CALLBACK_FAILURE.load(Ordering::Acquire) {
                    failure = Some("Callback queue, pump, or native layout failed");
                }
                if context_ready && !requested && failure.is_none() && !native::close_requested() {
                    requested = true;
                    let window = WindowInfo {
                        runtime_style: RuntimeStyle::ALLOY,
                        ..Default::default()
                    }
                    .set_as_child(
                        parent.get().cast(),
                        &Rect {
                            x: 0,
                            y: 0,
                            width: if coexistence { 550 } else { 900 },
                            height: if coexistence { 650 } else { 600 },
                        },
                    );
                    let mut client = ProbeClient::new(events.clone(), target.clone());
                    let mut context = request_context_create_context(
                        Some(&RequestContextSettings::default()),
                        None,
                    );
                    if context.is_none() {
                        failure = Some("Could not create isolated CEF request context");
                    } else {
                        accepted = browser_host_create_browser(
                            Some(&window),
                            Some(&mut client),
                            Some(&CefString::from(target.as_str())),
                            Some(&BrowserSettings::default()),
                            None,
                            context.as_mut(),
                        ) == 1;
                        if !accepted {
                            failure = Some("CEF rejected asynchronous browser creation");
                        }
                    }
                }

                if started.elapsed() > Duration::from_secs(32) {
                    failure = Some("CEF fixture checks exceeded their deadline");
                }
                #[cfg(feature = "coexistence")]
                if coexistence && failure.is_none() && !closing && loaded && webkit_loaded {
                    use crate::coexistence_policy::Action;
                    while let Some(action) = coex.next_action() {
                        let result: Result<(), String> = match action {
                            Action::Loaded => {
                                println!(
                                    "PASS: system WKWebView and CEF loaded in one GPUI window"
                                );
                                Ok(())
                            }
                            Action::CheckStorage => cef_storage_report().and_then(|_| {
                                webkit
                                    .borrow()
                                    .as_ref()
                                    .ok_or("Missing WebKit page")?
                                    .evaluate("window.probe.report('storage')")
                            }),
                            Action::StorageVerified => {
                                println!("PASS: WebKit and CEF storage contexts stayed isolated");
                                Ok(())
                            }
                            Action::WebKitClick(x, y) => webkit
                                .borrow()
                                .as_ref()
                                .ok_or("Missing WebKit page".into())
                                .and_then(|page| page.click(x, y)),
                            Action::WebKitType(text) => webkit
                                .borrow()
                                .as_ref()
                                .ok_or("Missing WebKit page".into())
                                .and_then(|page| page.type_text(text)),
                            Action::CefClick(x, y) => {
                                if unsafe { felinaut_cef_click(x, y) } {
                                    Ok(())
                                } else {
                                    Err("Native CEF coexistence click failed".into())
                                }
                            }
                            Action::CefType(text) => {
                                let text = CString::new(text).unwrap();
                                if unsafe { felinaut_cef_type_ascii(text.as_ptr()) } {
                                    Ok(())
                                } else {
                                    Err("Native CEF coexistence text failed".into())
                                }
                            }
                            Action::CefReport => cef_storage_report(),
                            Action::FocusVerified => {
                                println!(
                                    "PASS: native focus and input moved WebKit to CEF to WebKit"
                                );
                                Ok(())
                            }
                            Action::Resize => {
                                if unsafe { felinaut_cef_resize_window(1200., 760.) } {
                                    Ok(())
                                } else {
                                    Err("Coexistence window resize failed".into())
                                }
                            }
                            Action::ResizeVerified => {
                                println!("PASS: both engine viewports followed the GPUI resize");
                                Ok(())
                            }
                            Action::Complete => {
                                coex_complete = true;
                                action_at = Instant::now();
                                Ok(())
                            }
                        };
                        if let Err(error) = result {
                            eprintln!("Coexistence action failed: {error}");
                            failure = Some("Native coexistence action failed");
                            break;
                        }
                    }
                    if coex_complete && stage != 7 && action_at.elapsed() > Duration::from_secs(2) {
                        cx.update(|cx| cx.quit()).unwrap();
                        stage = 7;
                    }
                }
                if !coexistence && failure.is_none() && !closing && loaded {
                    if stage == 0 && title.starts_with("CEF_PROBE:ready:") {
                        println!(
                            "READY: real CEF loopback document loaded and JavaScript executed"
                        );
                        stage = 1;
                        action_at = Instant::now();
                    } else if stage == 1 && action_at.elapsed() > Duration::from_millis(200) {
                        // SAFETY: native events target only this retained child/window, outside GPUI borrows.
                        if !unsafe { felinaut_cef_click(100., 150.) } {
                            failure = Some("Native CEF input focus failed");
                        }
                        stage = 2;
                    } else if stage == 2 && title == "CEF_PROBE:focus:input" {
                        let text = CString::new("Hello from Felinaut").unwrap();
                        if !unsafe { felinaut_cef_type_ascii(text.as_ptr()) } {
                            failure = Some("Native CEF typing failed");
                        }
                        stage = 3;
                    } else if stage == 3 && title == "CEF_PROBE:typing:19" {
                        if !unsafe { felinaut_cef_click(110., 220.) } {
                            failure = Some("Native CEF button click failed");
                        }
                        stage = 4;
                    } else if stage == 4 && title == "CEF_PROBE:input:true:Hello from Felinaut" {
                        println!("PASS: native text input and trusted pointer click");
                        if !unsafe { felinaut_cef_resize_window(1000., 720.) } {
                            failure = Some("Native window resize failed");
                        }
                        stage = 5;
                    } else if stage == 5 && title == "CEF_PROBE:resized:1000" {
                        println!("PASS: CEF viewport followed the GPUI/native container resize");
                        stage = 6;
                        action_at = Instant::now();
                    } else if stage == 6 && action_at.elapsed() > Duration::from_secs(2) {
                        cx.update(|cx| cx.quit()).unwrap();
                        stage = 7;
                    }
                }
                if failure.is_some() && !native::close_requested() {
                    cx.update(|cx| cx.quit()).unwrap();
                }
                if native::close_requested() && !closing {
                    closing = true;
                    close_gate.request();
                }
                #[cfg(feature = "coexistence")]
                if coexistence
                    && closing
                    && let Some(mut page) = webkit.borrow_mut().take()
                {
                    if let Err(error) = page.stop_and_detach() {
                        eprintln!("WebKit detach failed: {error}");
                        failure = Some("WebKit coexistence teardown failed");
                    } else {
                        println!("PASS: WebKit detached before CEF shutdown");
                    }
                    drop(page);
                }
                if close_gate.should_dispatch()
                    && let Some(browser) = browser()
                    && let Some(host) = browser.host()
                {
                    host.close_browser(0);
                    close_gate.dispatched();
                }
                if closing && (closed || !accepted) {
                    if !detached {
                        unsafe {
                            felinaut_cef_release_container();
                        }
                    }
                    if let Some(error) = failure {
                        eprintln!("CEF fixture check failed: {error}");
                    }
                    TEST_PASSED.store(failure.is_none() && stage == 7, Ordering::Release);
                    // SAFETY: deferred native-loop work owns this process-lifetime callback;
                    // no GPUI borrow or serial GCD task surrounds the shutdown wait.
                    if !unsafe { felinaut_cef_defer_cleanup(shutdown_cef) } {
                        crate::fail("Could not schedule native CEF shutdown");
                    }
                    return;
                }
            }
        })
        .detach();
    });
    if !SHUTDOWN_COMPLETED.load(Ordering::Acquire) {
        crate::fail("GPUI exited before native CEF shutdown completed");
    }
    ExitCode::SUCCESS
}
