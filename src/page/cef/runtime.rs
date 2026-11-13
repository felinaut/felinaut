//! Application-wide CEF runtime, with per-page asynchronous native ownership.
//!
//! Initialize before `Application::new`, poll on the UI drain (100 ms), and keep
//! polling after a quit request until `shutdown_completed`. Never call shutdown
//! from GPUI rendering or the serial main GCD queue. No website gets a host API.
#[path = "runtime/callbacks.rs"]
mod callbacks;
#[path = "runtime/policy.rs"]
mod policy;

use super::{CefPage, Notifications, Presentation, evaluation::Evaluation};
use crate::page::{
    self, BackendInfo, Capabilities, Event, PageBackend, RequestScope, RuntimeIdentity,
    RuntimeSource, ScriptCallback,
};
use cef::{
    Browser, CefString, ImplBrowser, ImplBrowserHost, RequestContext, RequestContextSettings,
    library_loader::LibraryLoader,
};
use cef_host::{self as host, lifecycle::Lifecycle, pump};
use gpui::{Bounds, Pixels};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const PAGE_LIMIT: usize = 128;
const CREATION_TIMEOUT: Duration = Duration::from_secs(30);
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static CONTEXT_READY: AtomicBool = AtomicBool::new(false);
static CALLBACK_FAILED: AtomicBool = AtomicBool::new(false);
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static SHUTDOWN_COMPLETED: AtomicBool = AtomicBool::new(false);
thread_local! { static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) }; }

struct Runtime {
    // Must outlive ALL CEF references and cef::shutdown, including failed startup.
    loader: Option<LibraryLoader>,
    context: Option<RequestContext>,
    pages: Vec<Rc<Entry>>,
    cleanup_scheduled: bool,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if self.loader.is_some() {
            // If a host bypasses the quit barrier, dropping the loader or native
            // parents here would race live CEF callbacks. Preserve their owners
            // until process exit rather than unloading an active engine.
            std::mem::forget(self.loader.take());
            std::mem::forget(self.context.take());
            std::mem::forget(std::mem::take(&mut self.pages));
        }
    }
}

/// Fixed-size mailbox: one creation reference and monotonic close notifications.
/// Lifecycle callbacks never mutate the runtime registry or call GPUI.
#[derive(Default)]
struct Mailbox {
    browser: RefCell<Option<Browser>>,
    created: Cell<bool>,
    close_ready: Cell<bool>,
    closed: Cell<bool>,
    failed: Cell<Option<&'static str>>,
    load: Cell<policy::LoadCycle>,
    renderer_failed: std::sync::Arc<AtomicBool>,
}
struct Entry {
    native: RefCell<Option<Rc<NativePresentation>>>,
    browser: RefCell<Option<Browser>>,
    page: RefCell<Option<CefPage>>,
    mailbox: Rc<Mailbox>,
    notifications: Notifications,
    evaluation: Evaluation,
    lifecycle: Cell<Lifecycle>,
    dispatched: Cell<bool>,
    error: RefCell<Option<String>>,
    reported_error: Cell<bool>,
    url: String,
    created_at: Instant,
}
impl Entry {
    fn change(&self, update: impl FnOnce(&mut Lifecycle)) {
        let mut state = self.lifecycle.get();
        update(&mut state);
        self.lifecycle.set(state);
    }
    fn fail(&self, message: impl Into<String>) {
        if self.error.borrow().is_none() {
            *self.error.borrow_mut() = Some(message.into());
        }
        self.request_close();
    }
    fn request_close(&self) {
        // Logical close stops presentation/hit testing immediately. Ownership stays
        // retained until the asynchronous DoClose/OnBeforeClose sequence completes.
        if let Some(native) = self.native.borrow().as_ref() {
            let _ = native.container.set_visible(false);
        }
        self.change(Lifecycle::request_close);
        self.notifications.closed();
        self.evaluation.close();
        self.page.borrow_mut().take();
    }
    fn status(&self) -> Result<bool, String> {
        if let Some(error) = self.error.borrow().as_ref() {
            return Err(error.clone());
        }
        if self.lifecycle.get().closing() {
            return Err("CEF page is closing".into());
        }
        Ok(self.page.borrow().is_some())
    }
    fn with_page<T>(&self, f: impl FnOnce(&CefPage) -> Result<T, String>) -> Result<T, String> {
        self.status()?;
        let page = self.page.borrow();
        f(page.as_ref().ok_or("CEF page creation is pending")?)
    }
    fn native(&self) -> Result<Rc<NativePresentation>, String> {
        self.status()?;
        self.native
            .borrow()
            .clone()
            .ok_or("CEF presentation is closed".into())
    }
}
struct NativePresentation {
    container: host::Container,
}
impl Presentation for NativePresentation {
    fn set_visible(&self, visible: bool) -> Result<(), String> {
        self.container.set_visible(visible)
    }
    fn focus_parent(&self) -> Result<(), String> {
        self.container.focus_parent()
    }
    fn set_bounds(&self, bounds: Bounds<Pixels>) -> Result<(), String> {
        super::validate_bounds(bounds)?;
        self.container.set_bounds(
            f64::from(bounds.origin.x),
            f64::from(bounds.origin.y),
            f64::from(bounds.size.width),
            f64::from(bounds.size.height),
        )
    }
}

/// This proxy is the browser's page. Dropping it requests close, but the registry
/// retains its Entry through accepted creation and asynchronous OnBeforeClose.
struct PendingPage(Rc<Entry>);
impl PageBackend for PendingPage {
    fn creation_ready(&self) -> Result<bool, String> {
        self.0.status()
    }
    fn backend_info(&self) -> BackendInfo {
        backend_info()
    }
    fn navigate(&self, url: &str) -> Result<(), String> {
        self.0.with_page(|p| p.navigate(url))
    }
    fn back(&self) -> Result<(), String> {
        self.0.with_page(PageBackend::back)
    }
    fn forward(&self) -> Result<(), String> {
        self.0.with_page(PageBackend::forward)
    }
    fn reload(&self) -> Result<(), String> {
        self.0.with_page(PageBackend::reload)
    }
    fn can_go_back(&self) -> bool {
        self.0.with_page(|p| Ok(p.can_go_back())).unwrap_or(false)
    }
    fn can_go_forward(&self) -> bool {
        self.0
            .with_page(|p| Ok(p.can_go_forward()))
            .unwrap_or(false)
    }
    fn is_loading(&self) -> bool {
        self.0.with_page(|p| Ok(p.is_loading())).unwrap_or(false)
    }
    fn document(&self) -> Option<(String, String)> {
        self.0.with_page(|p| Ok(p.document())).ok().flatten()
    }
    fn set_visible(&self, visible: bool) -> Result<(), String> {
        self.0.native()?.set_visible(visible)
    }
    fn set_bounds(&self, bounds: Bounds<Pixels>) -> Result<(), String> {
        self.0.native()?.set_bounds(bounds)
    }
    fn focus_parent(&self) -> Result<(), String> {
        if self.0.status()? {
            self.0.with_page(PageBackend::focus_parent)
        } else {
            self.0.native()?.focus_parent()
        }
    }
    fn focus(&self) -> Result<(), String> {
        self.0.with_page(PageBackend::focus)
    }
    fn evaluate(&self, script: &str, callback: ScriptCallback) -> Result<(), String> {
        self.0.with_page(|p| p.evaluate(script, callback))
    }
    fn request_scope(&self) -> Option<RequestScope> {
        self.0.with_page(|p| Ok(p.request_scope())).ok().flatten()
    }
    fn drain_events(&self) -> Vec<Event> {
        if let Some(error) = self.0.error.borrow().as_ref() {
            if self.0.reported_error.replace(true) {
                return Vec::new();
            }
            return vec![Event::Failed(crate::load_error::LoadFailure {
                url: self.0.url.clone(),
                kind: crate::load_error::Kind::Other,
                detail: error.clone(),
                certificate: None,
            })];
        }
        self.0
            .page
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, PageBackend::drain_events)
    }
}
impl Drop for PendingPage {
    fn drop(&mut self) {
        self.0.request_close();
    }
}
fn backend_info() -> BackendInfo {
    BackendInfo {
        id: "cef",
        engine_family: "Chromium",
        runtime_version: Some("156.0.26 (Chromium 156.0.8078.88)".into()),
        runtime_identity: RuntimeIdentity {
            source: RuntimeSource::ExternalFramework,
            image_uuid: None,
        },
        capabilities: Capabilities::default(),
    }
}

struct Configuration {
    bundle: PathBuf,
    framework: PathBuf,
    helper: PathBuf,
    cache: PathBuf,
    mock_keychain: bool,
}
fn configuration() -> Result<Configuration, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let macos = exe.parent().ok_or("CEF requires a packaged executable")?;
    let contents = macos.parent().ok_or("CEF requires an application bundle")?;
    let bundle = contents
        .parent()
        .ok_or("CEF requires an application bundle")?
        .to_path_buf();
    if macos.file_name().is_none_or(|p| p != "MacOS")
        || contents.file_name().is_none_or(|p| p != "Contents")
        || !contents.join("Info.plist").is_file()
    {
        return Err("CEF requires the prepared Felinaut app bundle, not a loose executable".into());
    }
    let framework = contents.join("Frameworks/Chromium Embedded Framework.framework");
    let helper_name = format!(
        "{} Helper",
        exe.file_name()
            .and_then(|s| s.to_str())
            .ok_or("CEF executable name is not UTF-8")?
    );
    let helper = contents
        .join("Frameworks")
        .join(format!("{helper_name}.app/Contents/MacOS/{helper_name}"));
    if !framework.join("Chromium Embedded Framework").is_file() || !helper.is_file() {
        return Err("CEF framework or sandboxed helper is missing from the app bundle".into());
    }
    // Same startup profile used by the browser, with a separate engine subtree.
    // No legacy WebKit path is changed, and no persistent CEF cache is selected.
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("CEF requires HOME")?);
    let mock_keychain = std::env::var("FELINAUT_NATIVE_TEST").as_deref() == Ok("1");
    if mock_keychain {
        let cocoa = PathBuf::from(
            std::env::var_os("CFFIXED_USER_HOME")
                .ok_or("CEF native test requires CFFIXED_USER_HOME")?,
        );
        let fresh = host::test_profile::validate(true, &home, &cocoa)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(fresh.join(".felinaut-cef-test-claimed"))
            .map_err(|_| "CEF native test HOME was already claimed")?;
    }
    let profile = crate::session::default_path()
        .and_then(|p| p.parent().map(ToOwned::to_owned))
        .ok_or("CEF startup profile is unavailable")?;
    let cache = policy::cache_root(&profile)?;
    Ok(Configuration {
        bundle,
        framework,
        helper,
        cache,
        mock_keychain,
    })
}
fn path_string(path: &std::path::Path) -> Result<CefString, String> {
    Ok(CefString::from(
        path.to_str().ok_or("CEF path is not UTF-8")?,
    ))
}

/// Must run on the AppKit main thread before `Application::new`. No SDK/runtime
/// acquisition occurs here. Failure is explicit and never selects WebKit instead.
pub fn initialize() -> Result<(), String> {
    if !host::is_main_thread() {
        return Err("CEF initialization requires the AppKit main thread".into());
    }
    if INITIALIZED.load(Ordering::Acquire)
        || SHUTTING_DOWN.load(Ordering::Acquire)
        || SHUTDOWN_COMPLETED.load(Ordering::Acquire)
    {
        return Err("CEF runtime cannot be initialized twice or restarted".into());
    }
    let config = configuration()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let loader = LibraryLoader::new(&exe, false);
    if !loader.load() {
        return Err("Could not load the bundled CEF framework".into());
    }
    if !host::verify_runtime(&config.framework.join("Chromium Embedded Framework"))
        || cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0).is_null()
    {
        return Err("Bundled CEF runtime does not match the pinned SDK/API".into());
    }
    // Finish fallible path conversion before installing the pump/application.
    let settings = cef::Settings {
        no_sandbox: 0,
        external_message_pump: 1,
        multi_threaded_message_loop: 0,
        command_line_args_disabled: 1,
        root_cache_path: path_string(&config.cache)?,
        log_severity: cef::LogSeverity::DISABLE,
        main_bundle_path: path_string(&config.bundle)?,
        framework_dir_path: path_string(&config.framework)?,
        browser_subprocess_path: path_string(&config.helper)?,
        ..Default::default()
    };

    host::bootstrap()?;
    if !host::verify_protocols() || !pump::start_cef(pump_cef) {
        return Err("CEF application protocols or external pump setup failed".into());
    }

    let mut app = callbacks::RuntimeApp::new(config.mock_keychain);
    let args = cef::args::Args::new();
    if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        pump::stop();
        return Err(format!(
            "CEF initialization failed (exit code {})",
            cef::get_exit_code()
        ));
    }
    drop(app);
    RUNTIME.with_borrow_mut(|slot| {
        *slot = Some(Runtime {
            loader: Some(loader),
            context: None,
            pages: Vec::new(),
            cleanup_scheduled: false,
        })
    });
    INITIALIZED.store(true, Ordering::Release);
    pump::schedule(0);
    Ok(())
}

pub fn available() -> bool {
    INITIALIZED.load(Ordering::Acquire)
        && !SHUTTING_DOWN.load(Ordering::Acquire)
        && !CALLBACK_FAILED.load(Ordering::Acquire)
}
pub fn shutdown_completed() -> bool {
    SHUTDOWN_COMPLETED.load(Ordering::Acquire)
}

/// Returns a real asynchronous page proxy. `creation_ready` becomes true only
/// after OnAfterCreated and CefPage::attach. Pending layout/visibility is retained.
pub fn create(window: &gpui::Window, url: &str, hidden: bool) -> Result<page::Page, String> {
    if !host::is_main_thread() || !available() {
        return Err("CEF runtime is not available on this thread".into());
    }
    if !policy::navigation(url) {
        return Err("CEF initial URL was rejected by native policy".into());
    }
    if !host::verify_application() {
        return Err("CEF/GPUI application boundary is invalid".into());
    }
    let full = RUNTIME.with_borrow(|r| r.as_ref().is_none_or(|r| r.pages.len() >= PAGE_LIMIT));
    if full {
        return Err("CEF page limit reached, including pending closes".into());
    }
    let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("CEF requires an AppKit window".into());
    };
    // SAFETY: GPUI's live borrowed NSView is used synchronously on AppKit's main
    // thread; Container retains that view AND its window before returning.
    let container = unsafe { host::Container::new(handle.ns_view.as_ptr()) }?;
    container.set_visible(!hidden)?;
    let notifications = Notifications::default();
    let entry = Rc::new(Entry {
        native: RefCell::new(Some(Rc::new(NativePresentation { container }))),
        browser: RefCell::new(None),
        page: RefCell::new(None),
        mailbox: Rc::new(Mailbox::default()),
        evaluation: Evaluation::new(notifications.script_requests()),
        notifications,
        lifecycle: Cell::new(Lifecycle::default()),
        dispatched: Cell::new(false),
        error: RefCell::new(None),
        reported_error: Cell::new(false),
        url: url.into(),
        created_at: Instant::now(),
    });
    RUNTIME.with_borrow_mut(|r| {
        r.as_mut()
            .expect("initialized runtime")
            .pages
            .push(entry.clone())
    });
    Ok(Rc::new(PendingPage(entry)))
}

pub fn begin_shutdown() {
    if !host::is_main_thread() || !INITIALIZED.load(Ordering::Acquire) {
        return;
    }
    SHUTTING_DOWN.store(true, Ordering::Release);
    let pages = RUNTIME.with_borrow(|r| r.as_ref().map(|r| r.pages.clone()).unwrap_or_default());
    for entry in pages {
        entry.request_close();
    }
}

/// Drain only from GPUI's UI task, not rendering/native callbacks. Actual CEF
/// pumping uses the native run loop. Call this even when no CEF tab is visible.
pub fn poll() {
    if !host::is_main_thread() {
        return;
    }
    if shutdown_completed() {
        if host::close_requested() {
            host::finish_termination();
        }
        return;
    }
    if !INITIALIZED.load(Ordering::Acquire) {
        return;
    }
    if host::close_requested() || CALLBACK_FAILED.load(Ordering::Acquire) {
        begin_shutdown();
    }
    let ready = CONTEXT_READY.load(Ordering::Acquire);
    if ready && !SHUTTING_DOWN.load(Ordering::Acquire) {
        let needs_context =
            RUNTIME.with_borrow(|r| r.as_ref().is_some_and(|r| r.context.is_none()));
        if needs_context {
            // Empty cache_path: a single explicitly in-memory context, not the
            // global/default context. All CEF tabs share it; WK never does.
            let mut handler = callbacks::RuntimeContext::new();
            let context = cef::request_context_create_context(
                Some(&RequestContextSettings::default()),
                Some(&mut handler),
            );
            if context.is_none() {
                CALLBACK_FAILED.store(true, Ordering::Release);
                begin_shutdown();
            }
            RUNTIME.with_borrow_mut(|r| {
                if let Some(r) = r.as_mut() {
                    r.context = context;
                }
            });
        }
    }
    let (pages, context) = RUNTIME.with_borrow(|r| {
        r.as_ref()
            .map(|r| (r.pages.clone(), r.context.clone()))
            .unwrap_or_default()
    });
    for entry in &pages {
        if entry.mailbox.renderer_failed.swap(false, Ordering::AcqRel) {
            let mut cycle = entry.mailbox.load.get();
            cycle.failed();
            entry.mailbox.load.set(cycle);
            entry.evaluation.invalidate();
        }
        if let Some(error) = entry.mailbox.failed.take() {
            entry.fail(error);
        }
        if entry.mailbox.created.replace(false) {
            entry.change(Lifecycle::created);
            let browser = entry.mailbox.browser.borrow_mut().take();
            if let Some(browser) = browser {
                if !entry.lifecycle.get().closing() {
                    let native = entry.native.borrow().clone().expect("pending native owner");
                    match native.container.layout_children().and_then(|_| {
                        CefPage::attach(
                            browser.clone(),
                            backend_info(),
                            entry.notifications.clone(),
                            entry.evaluation.clone(),
                            native,
                            policy::navigation,
                        )
                    }) {
                        Ok(page) => *entry.page.borrow_mut() = Some(page),
                        Err(error) => entry.fail(error),
                    }
                }
                *entry.browser.borrow_mut() = Some(browser);
            } else {
                entry.fail("CEF creation callback did not contain a browser");
            }
        }
        if entry.mailbox.close_ready.replace(false) {
            entry.change(Lifecycle::close_ready);
            entry.notifications.closed();
            entry.evaluation.close();
            entry.page.borrow_mut().take();
        }
        if entry.mailbox.closed.get() {
            entry.change(Lifecycle::closed);
            entry.notifications.closed();
            entry.evaluation.close();
            entry.page.borrow_mut().take();
            entry.browser.borrow_mut().take();
            entry.mailbox.browser.borrow_mut().take();
            entry.native.borrow_mut().take();
            continue;
        }
        if !entry.dispatched.get()
            && !entry.lifecycle.get().closing()
            && ready
            && let Some(mut context) = context.clone()
        {
            entry.dispatched.set(true);
            let native = entry.native.borrow().clone().expect("pending native owner");
            let window = cef::WindowInfo {
                runtime_style: cef::RuntimeStyle::ALLOY,
                ..Default::default()
            }
            .set_as_child(
                native.container.view().cast(),
                &cef::Rect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            );
            let mut client = callbacks::RuntimeClient::new(
                entry.mailbox.clone(),
                entry.notifications.clone(),
                entry.evaluation.clone(),
            );
            if cef::browser_host_create_browser(
                Some(&window),
                Some(&mut client),
                Some(&CefString::from(entry.url.as_str())),
                Some(&cef::BrowserSettings::default()),
                None,
                Some(&mut context),
            ) == 1
            {
                entry.change(Lifecycle::accepted);
            } else {
                entry.fail("CEF rejected asynchronous browser creation");
            }
        }
        if entry.page.borrow().is_none()
            && !entry.lifecycle.get().closing()
            && entry.created_at.elapsed() >= CREATION_TIMEOUT
        {
            // An accepted native create cannot be cancelled or freed early. Keep
            // the owner and close the eventual browser, even after this error.
            entry.fail("CEF page creation exceeded its deadline");
        }
        if entry.lifecycle.get().should_dispatch()
            && let Some(browser) = entry.browser.borrow().as_ref()
            && let Some(host) = browser.host()
        {
            entry.change(Lifecycle::dispatched);
            host.close_browser(1);
        }
        if entry.lifecycle.get().should_detach()
            && let Some(native) = entry.native.borrow().as_ref()
        {
            if native.container.detach().is_ok() {
                entry.change(Lifecycle::detached);
            } else {
                entry.fail("CEF custom native close failed");
            }
        }
    }
    // Drop temporary Browser/RequestContext clones before scheduling shutdown.
    drop(context);
    drop(pages);
    let schedule = RUNTIME.with_borrow_mut(|r| {
        let Some(r) = r.as_mut() else {
            return false;
        };
        r.pages.retain(|entry| {
            !(entry.lifecycle.get().closing() && entry.lifecycle.get().releasable())
        });
        if SHUTTING_DOWN.load(Ordering::Acquire) && r.pages.is_empty() && !r.cleanup_scheduled {
            r.context.take();
            r.cleanup_scheduled = true;
            true
        } else {
            false
        }
    });
    if schedule && !host::defer_cleanup(shutdown_cef) {
        RUNTIME.with_borrow_mut(|r| {
            if let Some(r) = r.as_mut() {
                r.cleanup_scheduled = false;
            }
        });
    }
}
extern "C" fn pump_cef() {
    if INITIALIZED.load(Ordering::Acquire)
        && std::panic::catch_unwind(cef::do_message_loop_work).is_err()
    {
        CALLBACK_FAILED.store(true, Ordering::Release);
    }
}
extern "C" fn shutdown_cef() {
    let result = std::panic::catch_unwind(|| {
        // This callback runs after the poll borrow and any native pump returned.
        // Never unload while a CEF callback, browser, or context is retained.
        INITIALIZED.store(false, Ordering::Release);
        pump::stop();
        cef::shutdown();
        RUNTIME.with_borrow_mut(|r| {
            if let Some(mut r) = r.take() {
                r.loader.take();
            }
        });
        SHUTDOWN_COMPLETED.store(true, Ordering::Release);
        if host::close_requested() {
            host::finish_termination();
        }
    });
    if result.is_err() {
        // Do not terminate/unload an engine whose shutdown did not complete.
        CALLBACK_FAILED.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> Rc<Entry> {
        let notifications = Notifications::default();
        Rc::new(Entry {
            native: RefCell::new(None),
            browser: RefCell::new(None),
            page: RefCell::new(None),
            mailbox: Rc::new(Mailbox::default()),
            evaluation: Evaluation::new(notifications.script_requests()),
            notifications,
            lifecycle: Cell::new(Lifecycle::default()),
            dispatched: Cell::new(false),
            error: RefCell::new(None),
            reported_error: Cell::new(false),
            url: "https://fixture.test/".into(),
            created_at: Instant::now(),
        })
    }

    #[test]
    fn pending_proxy_is_not_ready_and_failed_dispatch_is_silent() {
        let entry = pending();
        let page = PendingPage(entry.clone());
        assert_eq!(page.creation_ready(), Ok(false));
        assert!(page.navigate("https://fixture.test/next").is_err());
        assert!(
            page.evaluate("1", Box::new(|_| panic!("unaccepted callback")))
                .is_err()
        );
        assert!(page.request_scope().is_none());
        assert!(!page.can_go_back());
        assert!(page.drain_events().is_empty());
        assert!(page.document().is_none());
    }

    #[test]
    fn dropping_accepted_pending_proxy_invalidates_requests_but_preserves_close_owner() {
        let entry = pending();
        entry.change(Lifecycle::accepted);
        let scope = entry.notifications.script_requests().scope();
        drop(PendingPage(entry.clone()));
        assert!(!scope.is_current());
        assert!(entry.lifecycle.get().closing());
        assert!(!entry.lifecycle.get().releasable());
        assert!(!entry.lifecycle.get().should_dispatch());
        entry.change(Lifecycle::created);
        assert!(entry.lifecycle.get().should_dispatch());
        entry.change(Lifecycle::closed);
        assert!(entry.lifecycle.get().releasable());
    }

    #[test]
    fn creation_failure_is_explicit_single_delivery_and_does_not_release_accepted_creation() {
        let entry = pending();
        entry.change(Lifecycle::accepted);
        let page = PendingPage(entry.clone());
        entry.fail("creation failed");
        entry.fail("later error must not overwrite the first");
        assert_eq!(page.creation_ready(), Err("creation failed".into()));
        assert!(
            matches!(page.drain_events().as_slice(), [Event::Failed(f)] if f.detail == "creation failed")
        );
        assert!(page.drain_events().is_empty());
        assert!(!entry.lifecycle.get().releasable());
        assert!(page.set_visible(true).is_err());
        assert!(page.reload().is_err());
    }
}
