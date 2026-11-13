//! Developer-only system-WebKit child for the trusted loopback coexistence fixture.
//!
//! Compile this module only on macOS with the parent's `coexistence` feature. All
//! operations, including destruction, belong on AppKit's main thread. The canvas
//! may update child bounds; callbacks only enqueue events, never re-enter GPUI.
//! Input, creation and destruction run outside GPUI rendering. The parent must call `stop_and_detach` (or drop `Page`)
//! before CEF shutdown and before destroying the GPUI window. No user profile,
//! IPC handler, initialization script, or general navigation API is installed.
//!
//! Navigation policy is not a subresource/network sandbox. The parent owns the
//! trusted fixture, loader-environment scrubbing, and a load deadline: Wry 0.57.1
//! does not expose provisional-load failures. Script evaluation reports dispatch
//! errors, not JavaScript exceptions or successful execution.

use std::{
    cell::Cell,
    ffi::{CStr, CString, c_char, c_int, c_void},
    marker::PhantomData,
    ptr,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
};
use url::Url;
use wry::{NewWindowResponse, PageLoadEvent, WebView, WebViewBuilder, WebViewExtMacOS};

const EVENT_CAPACITY: usize = 128;
const MAX_URL_BYTES: usize = 2048;
const MAX_TEXT_BYTES: usize = 1024;
const MAX_SCRIPT_BYTES: usize = 16 * 1024;
const MAX_EXTENT: f64 = 16_384.;
const SYSTEM_WEBKIT: &str = "/System/Library/Frameworks/WebKit.framework/WebKit";
const SYSTEM_WEBKIT_VERSIONED: &str =
    "/System/Library/Frameworks/WebKit.framework/Versions/A/WebKit";

unsafe extern "C" {
    fn pthread_main_np() -> c_int;
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
    fn felinaut_coexist_click_view(view: *mut c_void, x: f64, y: f64) -> bool;
    fn felinaut_coexist_type_view(view: *mut c_void, text: *const c_char) -> bool;
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_lookUpClass(name: *const c_char) -> *const c_void;
    fn object_getClass(object: *const c_void) -> *const c_void;
    fn class_getSuperclass(class: *const c_void) -> *const c_void;
    fn sel_registerName(name: *const c_char) -> *const c_void;
    fn class_getMethodImplementation(
        class: *const c_void,
        selector: *const c_void,
    ) -> *const c_void;
}

// Darwin's Dl_info layout; no libc/Objective-C crate dependency is needed here.
#[repr(C)]
struct DlInfo {
    name: *const c_char,
    base: *const c_void,
    symbol_name: *const c_char,
    symbol_address: *const c_void,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Loaded,
    Title(String),
    Failed(&'static str),
}

#[derive(Default)]
struct CallbackState {
    active: AtomicBool,
    overflow: AtomicBool,
}

#[derive(Clone)]
struct Emitter {
    sender: SyncSender<Event>,
    state: Arc<CallbackState>,
}

impl Emitter {
    fn emit(&self, event: Event) {
        if self.state.active.load(Ordering::Acquire) && self.sender.try_send(event).is_err() {
            // Never block AppKit callbacks, and never silently lose probe evidence.
            self.state.overflow.store(true, Ordering::Release);
        }
    }
}

pub struct Page {
    webview: Option<WebView>,
    events: Receiver<Event>,
    callbacks: Arc<CallbackState>,
    bounds: Cell<Option<ViewBounds>>,
    source_identity: String,
    // Keep main-thread destruction a property of the type, not just a comment.
    _main_thread: PhantomData<Rc<()>>,
}

impl Page {
    /// Uses the supplied URL unchanged (the parent supplies same-origin `/webkit`).
    /// Creates its own incognito store and an initially hidden, unfocused child.
    pub fn new(window: &gpui::Window, target: &Url) -> Result<Self, String> {
        require_main_thread()?;
        if !valid_target(target) {
            return Err("WebKit probe requires literal-loopback HTTP without credentials".into());
        }
        let (sender, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let callbacks = Arc::new(CallbackState {
            active: AtomicBool::new(true),
            overflow: AtomicBool::new(false),
        });
        let emitter = Emitter {
            sender,
            state: callbacks.clone(),
        };
        let navigation_events = emitter.clone();
        let popup_events = emitter.clone();
        let download_events = emitter.clone();
        let title_events = emitter.clone();
        let navigation_target = target.clone();
        let load_target = target.clone();
        let webview = WebViewBuilder::new()
            .with_incognito(true)
            .with_visible(false)
            .with_focused(false)
            .with_bounds(rect(ViewBounds {
                x: 0.,
                y: 0.,
                width: 1.,
                height: 1.,
            }))
            .with_navigation_handler(move |value| {
                let allowed = allows_navigation(&value, &navigation_target);
                if !allowed {
                    navigation_events.emit(Event::Failed("WebKit navigation left fixture origin"));
                }
                allowed
            })
            .with_new_window_req_handler(move |_, _| {
                popup_events.emit(Event::Failed("WebKit popup denied"));
                NewWindowResponse::Deny
            })
            .with_download_started_handler(move |_, _| {
                download_events.emit(Event::Failed("WebKit download denied"));
                false
            })
            .with_document_title_changed_handler(move |title| {
                title_events.emit(title_event(title));
            })
            .with_on_page_load_handler(move |event, value| {
                if matches!(event, PageLoadEvent::Finished) && value != "about:blank" {
                    if allows_navigation(&value, &load_target) {
                        emitter.emit(Event::Loaded);
                    } else {
                        emitter.emit(Event::Failed("WebKit finished outside fixture origin"));
                    }
                }
            })
            // Do not start fixture loading until the actual store/image is checked.
            .build_as_child(window)
            .map_err(|error| format!("Create WebKit fixture child: {error}"))?;
        let mut page = Self {
            webview: Some(webview),
            events,
            callbacks,
            bounds: Cell::new(None),
            source_identity: String::new(),
            _main_thread: PhantomData,
        };
        // From this point on, every failure goes through Page's stop/detach Drop.
        page.source_identity = verify_native(page.live_view()?)?;
        page.live_view()?
            .load_url(target.as_str())
            .map_err(|error| format!("Load WebKit fixture: {error}"))?;
        Ok(page)
    }

    /// Verified dyld image path for both WKWebView's class and its live stopLoading IMP.
    pub fn source_identity(&self) -> &str {
        &self.source_identity
    }

    /// Drains at most 128 queued events plus an explicit overflow failure.
    pub fn drain(&self) -> Vec<Event> {
        let mut events: Vec<_> = self.events.try_iter().take(EVENT_CAPACITY).collect();
        if self.callbacks.overflow.swap(false, Ordering::AcqRel) {
            events.push(Event::Failed("WebKit callback queue overflowed"));
        }
        events
    }

    /// Window-relative logical top-left bounds. Either zero dimension hides the
    /// child. Origins are bounded to +/-16384, sizes to 0..=16384; positive sizes
    /// round up to whole logical pixels because Wry's macOS adapter uses integers.
    pub fn set_bounds(&self, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
        let view = self.live_view()?;
        let bounds = normalize_bounds(x, y, w, h)?;
        if self.bounds.get() == Some(bounds) {
            return Ok(());
        }
        view.set_visible(false)
            .map_err(|error| format!("Hide WebKit child for layout: {error}"))?;
        self.bounds.set(None);
        view.set_bounds(rect(bounds))
            .map_err(|error| format!("Set WebKit child bounds: {error}"))?;
        if bounds.visible() {
            view.set_visible(true)
                .map_err(|error| format!("Show WebKit child: {error}"))?;
            self.bounds.set(Some(bounds));
        }
        Ok(())
    }

    /// Child-local logical top-left coordinates, strictly inside its visible bounds.
    pub fn click(&self, x: f64, y: f64) -> Result<(), String> {
        let view = self.live_view()?;
        normalize_click(x, y, self.bounds.get())?;
        let native = view.webview();
        let pointer = ptr::from_ref(&*native).cast::<c_void>().cast_mut();
        // SAFETY: retained live WKWebView on the main thread; the parent's bridge
        // sends scoped AppKit input synchronously and must not retain this pointer.
        if unsafe { felinaut_coexist_click_view(pointer, x, y) } {
            Ok(())
        } else {
            Err("Native WebKit child click failed".into())
        }
    }

    /// Bounded, nonempty UTF-8 text without control characters; no trimming or
    /// lossy conversion. Requires a visible child and the bridge's scoped focus.
    pub fn type_text(&self, text: &str) -> Result<(), String> {
        let view = self.live_view()?;
        if self.bounds.get().is_none() {
            return Err("Cannot type into a hidden WebKit child".into());
        }
        let text = normalize_text(text)?;
        let native = view.webview();
        let pointer = ptr::from_ref(&*native).cast::<c_void>().cast_mut();
        // SAFETY: both the retained view and NUL-terminated UTF-8 buffer remain
        // live for this synchronous, main-thread, view-scoped native bridge call.
        if unsafe { felinaut_coexist_type_view(pointer, text.as_ptr()) } {
            Ok(())
        } else {
            Err("Native WebKit child text input failed".into())
        }
    }

    /// TRUSTED CALLER ONLY: pass fixed internal fixture scripts, never page data,
    /// environment/CLI strings, or user input. No script/command IPC is installed.
    /// Success means dispatched, not evaluated successfully; observe fixture titles.
    pub fn evaluate(&self, script: &str) -> Result<(), String> {
        let view = self.live_view()?;
        validate_script(script)?;
        view.evaluate_script(script)
            .map_err(|error| format!("Dispatch WebKit fixture script: {error}"))
    }

    /// Idempotently stops native loading, hides/removes the child while Wry still
    /// owns it, then drops Wry immediately. Call before CEF shutdown/window close.
    pub fn stop_and_detach(&mut self) -> Result<(), String> {
        require_main_thread()?;
        self.callbacks.active.store(false, Ordering::Release);
        self.bounds.set(None);
        if let Some(view) = self.webview.take() {
            let native = view.webview();
            // SAFETY: main thread, with both Wry and a local retain keeping the
            // WKWebView alive. No GPUI calls or application callbacks are made here.
            unsafe { native.stopLoading() };
            native.setHidden(true);
            native.removeFromSuperview();
            drop(view);
        }
        Ok(())
    }

    fn live_view(&self) -> Result<&WebView, String> {
        require_main_thread()?;
        let view = self.webview.as_ref().ok_or("WebKit child is detached")?;
        // Avoid Wry's set_bounds/focus unwraps if the host has already removed it.
        // SAFETY: Wry retains this main-thread NSView; only inspect its live parent.
        if view.webview().window().is_none() || unsafe { view.webview().superview() }.is_none() {
            return Err("WebKit child has no live native parent".into());
        }
        Ok(view)
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // !Send/!Sync plus main-thread-only construction prevents a safe caller
        // from moving this owner to another thread for destruction.
        let result = self.stop_and_detach();
        debug_assert!(result.is_ok(), "WebKit child dropped off the main thread");
    }
}

fn require_main_thread() -> Result<(), String> {
    // SAFETY: pthread_main_np has no preconditions or borrowed output.
    if unsafe { pthread_main_np() } != 0 {
        Ok(())
    } else {
        Err("WebKit coexistence operations require AppKit's main thread".into())
    }
}

fn valid_target(target: &Url) -> bool {
    target.as_str().len() <= MAX_URL_BYTES
        && target.scheme() == "http"
        && target.username().is_empty()
        && target.password().is_none()
        && match target.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        }
}

fn allows_navigation(value: &str, target: &Url) -> bool {
    valid_target(target)
        && (value == "about:blank"
            || (value.len() <= MAX_URL_BYTES
                && Url::parse(value)
                    .is_ok_and(|url| valid_target(&url) && url.origin() == target.origin())))
}

fn title_event(title: String) -> Event {
    if title.len() <= MAX_TEXT_BYTES {
        Event::Title(title)
    } else {
        Event::Failed("Oversized WebKit fixture title")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ViewBounds {
    fn visible(self) -> bool {
        self.width > 0. && self.height > 0.
    }
}

fn normalize_bounds(x: f64, y: f64, w: f64, h: f64) -> Result<ViewBounds, String> {
    if ![x, y, w, h].into_iter().all(f64::is_finite)
        || x.abs() > MAX_EXTENT
        || y.abs() > MAX_EXTENT
        || !(0. ..=MAX_EXTENT).contains(&w)
        || !(0. ..=MAX_EXTENT).contains(&h)
    {
        return Err("WebKit bounds must be finite with bounded origins and sizes 0..=16384".into());
    }
    Ok(ViewBounds {
        x: x.round(),
        y: y.round(),
        width: w.ceil(),
        height: h.ceil(),
    })
}

fn rect(bounds: ViewBounds) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(bounds.x, bounds.y).into(),
        size: wry::dpi::LogicalSize::new(bounds.width, bounds.height).into(),
    }
}

fn normalize_click(x: f64, y: f64, bounds: Option<ViewBounds>) -> Result<(), String> {
    let bounds = bounds.ok_or("Cannot click a hidden WebKit child")?;
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.
        || y < 0.
        || x >= bounds.width
        || y >= bounds.height
    {
        return Err("WebKit click must be finite and inside the visible child".into());
    }
    Ok(())
}

fn normalize_text(text: &str) -> Result<CString, String> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES || text.chars().any(char::is_control) {
        return Err("WebKit text must be 1..=1024 UTF-8 bytes without control characters".into());
    }
    CString::new(text).map_err(|_| "WebKit text contains NUL".into())
}

fn validate_script(script: &str) -> Result<(), String> {
    if script.trim().is_empty() || script.len() > MAX_SCRIPT_BYTES || script.contains('\0') {
        return Err(
            "WebKit fixture script must be nonempty, NUL-free and at most 16384 bytes".into(),
        );
    }
    Ok(())
}

fn system_webkit_image(path: &str) -> bool {
    matches!(path, SYSTEM_WEBKIT | SYSTEM_WEBKIT_VERSIONED)
}

fn verify_native(view: &WebView) -> Result<String, String> {
    require_main_thread()?;
    let native = view.webview();
    // SAFETY: a retained, newly created WKWebView and its configuration, main thread.
    if unsafe { native.configuration().websiteDataStore().isPersistent() } {
        return Err("WebKit coexistence store is unexpectedly persistent".into());
    }
    // SAFETY: linked WebKit/registered classes stay loaded throughout these
    // lookups; constant selector names and the retained view pointer remain live.
    let (class, implementation) = unsafe {
        let class = objc_lookUpClass(c"WKWebView".as_ptr());
        if class.is_null() {
            return Err("WKWebView class is unavailable".into());
        }
        let actual = object_getClass(ptr::from_ref(&*native).cast());
        let mut ancestor = actual;
        while !ancestor.is_null() && ancestor != class {
            ancestor = class_getSuperclass(ancestor);
        }
        if ancestor.is_null() {
            return Err("Wry native child is not a WKWebView".into());
        }
        let implementation =
            class_getMethodImplementation(actual, sel_registerName(c"stopLoading".as_ptr()));
        (class, implementation)
    };
    let class_image = image_for_address(class)?;
    let implementation_image = image_for_address(implementation)?;
    if !system_webkit_image(&class_image)
        || !system_webkit_image(&implementation_image)
        || class_image != implementation_image
    {
        return Err("WKWebView class/implementation is not verified system WebKit".into());
    }
    Ok(class_image)
}

fn image_for_address(address: *const c_void) -> Result<String, String> {
    let mut info = DlInfo {
        name: ptr::null(),
        base: ptr::null(),
        symbol_name: ptr::null(),
        symbol_address: ptr::null(),
    };
    // SAFETY: a live class/IMP address and writable Darwin Dl_info; no dlclose is
    // performed, and dyld's image name is copied before any native owner is dropped.
    if address.is_null()
        || unsafe { dladdr(address, &mut info) } == 0
        || info.name.is_null()
        || info.base.is_null()
    {
        return Err("Cannot resolve WKWebView source image with dladdr".into());
    }
    // SAFETY: successful dladdr returns a loader-owned NUL-terminated image name.
    unsafe { CStr::from_ptr(info.name) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| "WKWebView source image path is not UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_is_exact_blank_or_credential_free_loopback_origin() {
        for base in ["http://127.0.0.1:8765/webkit", "http://[::1]:8765/webkit"] {
            let target = Url::parse(base).unwrap();
            assert!(valid_target(&target));
            assert!(allows_navigation(base, &target));
            assert!(allows_navigation("about:blank", &target));
            assert!(allows_navigation(
                target.join("/other?q=1#ok").unwrap().as_str(),
                &target
            ));
            for value in [
                "",
                "garbage",
                "about:blank#x",
                "about:blank?x",
                "javascript:1",
                "data:text/html,hi",
                "file:///tmp/a",
                "https://127.0.0.1:8765/",
                "http://localhost:8765/",
                "http://example.com:8765/",
                "http://127.0.0.1.example.com:8765/",
                "http://127.0.0.1:8766/",
                "http://u:p@127.0.0.1:8765/",
                "http://u:p@[::1]:8765/",
            ] {
                assert!(!allows_navigation(value, &target), "{base}: {value}");
            }
        }
        let target = Url::parse("http://127.0.0.1:8765/webkit").unwrap();
        assert!(!allows_navigation("http://[::1]:8765/webkit", &target));
        assert!(!allows_navigation("http://127.0.0.2:8765/webkit", &target));
        assert!(!allows_navigation(
            &format!("http://127.0.0.1:8765/{}", "x".repeat(2048)),
            &target
        ));
        let invalid = Url::parse("https://example.com/").unwrap();
        assert!(!valid_target(&invalid));
        assert!(!allows_navigation("about:blank", &invalid));
        assert!(valid_target(&Url::parse("http://127.0.0.1/").unwrap()));
        for invalid in [
            "file:///tmp/a",
            "http://localhost/",
            "http://u@127.0.0.1/",
            "http://192.0.2.1/",
        ] {
            assert!(!valid_target(&Url::parse(invalid).unwrap()));
        }
    }

    #[test]
    fn bounds_normalize_for_wrys_integer_logical_viewport() {
        let bounds = normalize_bounds(10.4, -20.6, 0.1, 99.1).unwrap();
        assert_eq!(
            bounds,
            ViewBounds {
                x: 10.,
                y: -21.,
                width: 1.,
                height: 100.
            }
        );
        assert!(bounds.visible());
        for (w, h) in [(0., 100.), (100., 0.), (0., 0.)] {
            assert!(!normalize_bounds(0., 0., w, h).unwrap().visible());
        }
        assert!(normalize_bounds(-MAX_EXTENT, MAX_EXTENT, MAX_EXTENT, MAX_EXTENT).is_ok());
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, MAX_EXTENT + 1.] {
            assert!(normalize_bounds(bad, 0., 10., 10.).is_err());
            assert!(normalize_bounds(0., bad, 10., 10.).is_err());
            assert!(normalize_bounds(0., 0., bad, 10.).is_err());
            assert!(normalize_bounds(0., 0., 10., bad).is_err());
        }
        assert!(normalize_bounds(0., 0., -0.1, 10.).is_err());
        assert!(normalize_bounds(0., 0., 10., -0.1).is_err());
        assert!(normalize_bounds(-MAX_EXTENT - 1., 0., 10., 10.).is_err());
    }

    #[test]
    fn clicks_remain_strictly_inside_the_visible_child() {
        let bounds = Some(normalize_bounds(100., 200., 300., 400.).unwrap());
        assert!(normalize_click(0., 0., bounds).is_ok());
        assert!(normalize_click(299.9, 399.9, bounds).is_ok());
        assert!(normalize_click(0., 0., None).is_err());
        for (x, y) in [
            (300., 0.),
            (0., 400.),
            (-0.1, 0.),
            (0., -0.1),
            (f64::NAN, 0.),
            (0., f64::INFINITY),
        ] {
            assert!(normalize_click(x, y, bounds).is_err());
        }
    }

    #[test]
    fn input_preserves_utf8_and_spaces_but_rejects_controls_and_excess() {
        for text in ["Hello from Felinaut", " café 🐈 "] {
            assert_eq!(normalize_text(text).unwrap().to_str().unwrap(), text);
        }
        assert!(normalize_text(&"a".repeat(1024)).is_ok());
        assert!(normalize_text(&"é".repeat(512)).is_ok());
        for text in ["", "a\0b", "a\nb", "\t", "\u{7f}", "\u{85}"] {
            assert!(normalize_text(text).is_err());
        }
        assert!(normalize_text(&"a".repeat(1025)).is_err());
        assert!(normalize_text(&"é".repeat(513)).is_err());
    }

    #[test]
    fn script_and_title_limits_are_byte_bounded() {
        assert!(validate_script("document.title = 'fixture';").is_ok());
        assert!(validate_script(&"x".repeat(MAX_SCRIPT_BYTES)).is_ok());
        for script in ["", " \n\t", "x\0y"] {
            assert!(validate_script(script).is_err());
        }
        assert!(validate_script(&"x".repeat(MAX_SCRIPT_BYTES + 1)).is_err());
        assert_eq!(title_event(String::new()), Event::Title(String::new()));
        assert!(matches!(title_event("é".repeat(512)), Event::Title(_)));
        assert!(matches!(title_event("é".repeat(513)), Event::Failed(_)));
    }

    #[test]
    fn provenance_does_not_accept_framework_lookalikes() {
        assert!(system_webkit_image(SYSTEM_WEBKIT));
        assert!(system_webkit_image(SYSTEM_WEBKIT_VERSIONED));
        for path in [
            "",
            "/tmp/WebKit.framework/WebKit",
            "/System/Library/Frameworks/WebKit.framework.evil/WebKit",
            "/System/Library/Frameworks/WebKit.framework/Versions/A/Other",
            "/System/Library/Frameworks/WebKit.framework/../WebKit",
        ] {
            assert!(!system_webkit_image(path));
        }
    }

    #[test]
    fn callback_queue_is_bounded_nonblocking_and_detachment_disables_emission() {
        let (sender, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let state = Arc::new(CallbackState {
            active: AtomicBool::new(true),
            overflow: AtomicBool::new(false),
        });
        let emitter = Emitter {
            sender,
            state: state.clone(),
        };
        for _ in 0..EVENT_CAPACITY {
            emitter.emit(Event::Loaded);
        }
        assert!(!state.overflow.load(Ordering::Acquire));
        emitter.emit(Event::Loaded);
        assert!(state.overflow.swap(false, Ordering::AcqRel));
        assert_eq!(events.try_iter().count(), EVENT_CAPACITY);
        state.active.store(false, Ordering::Release);
        emitter.emit(Event::Loaded);
        assert!(events.try_recv().is_err());
        assert!(!state.overflow.load(Ordering::Acquire));
    }
}
