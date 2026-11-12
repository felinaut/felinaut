//! Native pages live on GPUI's UI thread and must be dropped before their parent window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};

use gpui::{Bounds, Pixels};
use wry::{NewWindowResponse, PageLoadEvent, WebView, WebViewBuilder};

use crate::audio::{AudioReport, TabAudio, parse_report};
use crate::load_error::{Exceptions, LoadFailure};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started(String),
    Finished(String),
    Title(String),
    Url(String),
    Open(String),
    OpenInBackground(String),
    Hover(Option<String>),
    /// The user clicked into the page.
    Focused,
    Audio(AudioReport),
    /// The site's icon as a small validated PNG.
    Icon(Vec<u8>),
    /// A top-level load failed before anything was shown; the previous document remains.
    Failed(LoadFailure),
    /// A load ended without finishing: cancelled, or failed after the page was shown.
    Stopped,
}

/// One persistent native child view per tab; hiding it does not discard its history or document.
/// The owner should poll `drain_events` on the UI thread, including for inactive tabs.
pub struct NativePage {
    webview: WebView,
    events: Receiver<Event>,
    last_url: RefCell<String>,
    failure_key: Option<usize>,
}

impl Drop for NativePage {
    fn drop(&mut self) {
        if let Some(key) = self.failure_key {
            FAILURES.with_borrow_mut(|pages| pages.remove(&key));
            crate::downloads::native::detach(key);
        }
    }
}

impl NativePage {
    /// Creates an initially hidden, unfocused child. Set its canvas bounds before showing it.
    pub fn new(window: &gpui::Window, url: &str) -> Result<Self, String> {
        check_url(url)?;
        let (sender, events) = mpsc::channel();
        let load_sender = sender.clone();
        let title_sender = sender.clone();
        let link_sender = sender.clone();
        let failure_sender = sender.clone();
        let mut builder = WebViewBuilder::new();
        if agent_enabled() {
            builder =
                builder.with_initialization_script_for_main_only(include_str!("agent.js"), true);
        }
        let webview = builder
            .with_initialization_script_for_main_only(include_str!("link_interactions.js"), false)
            .with_initialization_script_for_main_only(include_str!("audio_control.js"), false)
            .with_initialization_script_for_main_only(include_str!("page_icon.js"), true)
            .with_ipc_handler(move |request| {
                let body = request.body();
                let event = parse_link_event(body)
                    .or_else(|| parse_icon_event(body))
                    .or_else(|| {
                        (body.len() <= 64)
                            .then(|| parse_report(body))
                            .flatten()
                            .map(Event::Audio)
                    });
                if let Some(event) = event {
                    let _ = link_sender.send(event);
                }
            })
            .with_visible(false)
            .with_focused(false)
            .with_navigation_handler(|url| allowed_url(&url))
            .with_new_window_req_handler(move |url, _| {
                if allowed_url(&url) {
                    let _ = sender.send(Event::Open(url));
                }
                // Main owns tab creation; never let WebKit spawn an external window.
                NewWindowResponse::Deny
            })
            .with_document_title_changed_handler(move |title| {
                let _ = title_sender.send(Event::Title(title));
            })
            .with_on_page_load_handler(move |event, url| {
                // Wry has no load-failure callback. Finished means an actual native finish,
                // not a timeout or a failed provisional navigation.
                let event = match event {
                    PageLoadEvent::Started => Event::Started(url),
                    PageLoadEvent::Finished => Event::Finished(url),
                };
                let _ = load_sender.send(event);
            })
            .with_url(url)
            .build_as_child(window)
            .map_err(|error| error.to_string())?;
        let failure_key = watch_failures(&webview, failure_sender);
        {
            use wry::WebViewExtMacOS;
            crate::downloads::native::attach(&webview.webview(), failure_key.is_some());
        }

        Ok(Self {
            webview,
            events,
            last_url: RefCell::new(String::new()),
            failure_key,
        })
    }

    /// A page the user never sees, for reading another site with the shared WebKit logins:
    /// no injected scripts, no new windows, only load events. It stays hidden and offscreen.
    pub fn hidden(window: &gpui::Window, url: &str) -> Result<Self, String> {
        check_url(url)?;
        let (sender, events) = mpsc::channel();
        let webview = WebViewBuilder::new()
            .with_visible(false)
            .with_focused(false)
            // A real viewport: rows that never get a layout may render empty.
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(-20_000., -20_000.).into(),
                size: wry::dpi::LogicalSize::new(1024., 768.).into(),
            })
            .with_navigation_handler(|url| allowed_url(&url))
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
            .with_on_page_load_handler(move |event, url| {
                if matches!(event, PageLoadEvent::Finished) {
                    let _ = sender.send(Event::Finished(url));
                }
            })
            .with_url(url)
            .build_as_child(window)
            .map_err(|error| error.to_string())?;
        {
            // A page nobody sees must never save a file.
            use wry::WebViewExtMacOS;
            crate::downloads::native::attach(&webview.webview(), false);
        }
        Ok(Self {
            webview,
            events,
            last_url: RefCell::new(String::new()),
            failure_key: None,
        })
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native link interaction test")]
    pub fn evaluate_for_test(&self, script: &str) -> Result<(), String> {
        self.webview
            .evaluate_script(script)
            .map_err(|error| error.to_string())
    }

    /// Applies a tab's volume, mute and EQ to the current document and all its frames.
    pub fn apply_audio(&self, audio: TabAudio) -> Result<(), String> {
        self.webview
            .evaluate_script(&audio.script())
            .map_err(|error| error.to_string())
    }

    /// WebKit's page zoom; the page keeps it across navigations.
    pub fn zoom(&self) -> f64 {
        use wry::WebViewExtMacOS;
        // SAFETY: a property read on a live WKWebView owned by this page, on the main thread.
        unsafe { self.webview.webview().pageZoom() }
    }

    pub fn set_zoom(&self, zoom: f64) -> Result<(), String> {
        self.webview.zoom(zoom).map_err(|error| error.to_string())
    }

    pub fn navigate(&self, url: &str) -> Result<(), String> {
        check_url(url)?;
        self.webview
            .load_url(url)
            .map_err(|error| error.to_string())
    }

    pub fn back(&self) -> Result<(), String> {
        self.webview.go_back().map_err(|error| error.to_string())
    }

    pub fn forward(&self) -> Result<(), String> {
        self.webview.go_forward().map_err(|error| error.to_string())
    }

    pub fn reload(&self) -> Result<(), String> {
        self.webview.reload().map_err(|error| error.to_string())
    }

    /// Uses WKWebView's native history, not a JavaScript or application-side history mirror.
    pub fn can_go_back(&self) -> bool {
        self.webview.can_go_back().unwrap_or(false)
    }

    pub fn can_go_forward(&self) -> bool {
        self.webview.can_go_forward().unwrap_or(false)
    }

    pub fn is_loading(&self) -> bool {
        use wry::WebViewExtMacOS;
        // SAFETY: a property read on a live WKWebView owned by this page, on the main thread.
        unsafe { self.webview.webview().isLoading() }
    }

    /// The URL and title on display; none after a failed first load.
    pub fn document(&self) -> Option<(String, String)> {
        use wry::WebViewExtMacOS;
        let url = self.current_url().filter(|url| !url.is_empty())?;
        // SAFETY: a property read on a live WKWebView owned by this page, on the main thread.
        let title = unsafe { self.webview.webview().title() }
            .map(|title| title.to_string())
            .unwrap_or_default();
        Some((url, title))
    }

    pub fn set_visible(&self, visible: bool) -> Result<(), String> {
        self.webview
            .set_visible(visible)
            .map_err(|error| error.to_string())
    }

    pub fn focus(&self) -> Result<(), String> {
        self.webview.focus().map_err(|error| error.to_string())
    }

    pub fn focus_parent(&self) -> Result<(), String> {
        self.webview
            .focus_parent()
            .map_err(|error| error.to_string())
    }

    /// Bounds are window-relative logical pixels, as supplied by the GPUI canvas.
    pub fn set_bounds(&self, bounds: Bounds<Pixels>) -> Result<(), String> {
        self.webview
            .set_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(
                    f64::from(bounds.origin.x),
                    f64::from(bounds.origin.y),
                )
                .into(),
                size: wry::dpi::LogicalSize::new(
                    f64::from(bounds.size.width),
                    f64::from(bounds.size.height),
                )
                .into(),
            })
            .map_err(|error| error.to_string())
    }

    pub fn drain_events(&self) -> Vec<Event> {
        let mut events: Vec<_> = self.events.try_iter().collect();
        // Poll the committed native URL: the navigation-policy callback also sees
        // subframes and attempted navigations, and misses history.pushState changes.
        if let Some(url) = self.current_url() {
            let mut last_url = self.last_url.borrow_mut();
            if !url.is_empty() && *last_url != url {
                last_url.clone_from(&url);
                events.push(Event::Url(url));
            }
        }
        events
    }

    /// Runs a script. The callback gets the JSON text of its result, or nothing if it threw.
    /// Wry drops the script, and the callback, while the first page still loads.
    pub fn evaluate(
        &self,
        script: &str,
        callback: impl Fn(String) + Send + 'static,
    ) -> Result<(), String> {
        self.webview
            .evaluate_script_with_callback(script, callback)
            .map_err(|error| error.to_string())
    }

    /// Sends a real left click to the page. `x` and `y` are viewport coordinates from
    /// JavaScript. The page sees trusted events, unlike events that a script makes.
    pub fn native_click(&self, x: f64, y: f64) -> Result<(), String> {
        use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
        use wry::WebViewExtMacOS;
        let view = self.webview.webview();
        let window = view.window().ok_or("The page has no window")?;
        let y = if view.isFlipped() {
            y
        } else {
            view.bounds().size.height - y
        };
        let location = view.convertPoint_toView(objc2_foundation::NSPoint::new(x, y), None);
        let time = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
        for (kind, clicks) in [
            (NSEventType::MouseMoved, 0),
            (NSEventType::LeftMouseDown, 1),
            (NSEventType::LeftMouseUp, 1),
        ] {
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind,
                location,
                NSEventModifierFlags::empty(),
                time,
                window.windowNumber(),
                None,
                0,
                clicks,
                1.0,
            )
            .ok_or("AppKit could not make the mouse event")?;
            match kind {
                NSEventType::MouseMoved => view.mouseMoved(&event),
                NSEventType::LeftMouseDown => view.mouseDown(&event),
                _ => view.mouseUp(&event),
            }
        }
        Ok(())
    }

    /// Inserts text at the selection of the focused field, as the keyboard does.
    pub fn native_text(&self, text: &str) -> Result<(), String> {
        use wry::WebViewExtMacOS;
        let view = self.webview.webview();
        let window = view.window().ok_or("The page has no window")?;
        window.makeFirstResponder(Some(&view));
        let text = objc2_foundation::NSString::from_str(text);
        // An NSNotFound location: replace the selection.
        let range = objc2_foundation::NSRange::new(objc2_foundation::NSNotFound as usize, 0);
        // SAFETY: WKWebView implements NSTextInputClient; this is its text insertion method,
        // called on the main thread with a live string.
        let _: () =
            unsafe { objc2::msg_send![&*view, insertText: &*text, replacementRange: range] };
        Ok(())
    }

    /// Sends a real key press to the page. `key` is a DOM key name or one character.
    pub fn native_key(&self, key: &str) -> Result<(), String> {
        use objc2_app_kit::{NSEvent, NSEventType};
        use wry::WebViewExtMacOS;
        let (modifiers, name) = native_modifiers(key);
        let (code, characters) = native_key(name).ok_or_else(|| format!("Unknown key: {key}"))?;
        // One character has key code 0, and a shortcut reads the key code.
        if !modifiers.is_empty() && name.chars().count() == 1 {
            return Err(format!("Modifiers need a named key: {key}"));
        }
        let view = self.webview.webview();
        let window = view.window().ok_or("The page has no window")?;
        window.makeFirstResponder(Some(&view));
        let characters = objc2_foundation::NSString::from_str(&characters);
        let time = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
        for kind in [NSEventType::KeyDown, NSEventType::KeyUp] {
            let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                kind,
                objc2_foundation::NSPoint::new(0., 0.),
                modifiers,
                time,
                window.windowNumber(),
                None,
                &characters,
                &characters,
                false,
                code,
            )
            .ok_or("AppKit could not make the key event")?;
            if !modifiers.is_empty() {
                // A key with a modifier goes into the event queue and takes the route of a real
                // key: AppKit first offers it to the window as a key equivalent. Felinaut must
                // be the active app.
                let app: *mut objc2::runtime::AnyObject =
                    unsafe { objc2::msg_send![objc2::class!(NSApplication), sharedApplication] };
                // SAFETY: the main thread, with a live event.
                let _: () = unsafe { objc2::msg_send![app, postEvent: &*event, atStart: false] };
            } else if kind == NSEventType::KeyDown {
                view.keyDown(&event);
            } else {
                view.keyUp(&event);
            }
        }
        Ok(())
    }

    /// Takes WebKit's own snapshot of the visible page. The callback gets PNG bytes.
    pub fn screenshot(&self, callback: impl Fn(Result<Vec<u8>, String>) + 'static) {
        use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
        use wry::WebViewExtMacOS;
        let block = block2::RcBlock::new(
            move |image: *mut NSImage, _: *mut objc2_foundation::NSError| {
                // SAFETY: WebKit gives a valid image or null, on the main thread.
                let png = unsafe { image.as_ref() }
                    .and_then(|image| image.TIFFRepresentation())
                    .and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff))
                    .and_then(|bitmap| {
                        // SAFETY: an empty property dictionary is valid for PNG.
                        unsafe {
                            bitmap.representationUsingType_properties(
                                NSBitmapImageFileType::PNG,
                                &objc2_foundation::NSDictionary::new(),
                            )
                        }
                    })
                    .map(|png| png.to_vec())
                    .ok_or_else(|| "WebKit could not take the snapshot".to_string());
                callback(png);
            },
        );
        // SAFETY: a live WKWebView on the main thread; WebKit copies the block.
        unsafe {
            self.webview
                .webview()
                .takeSnapshotWithConfiguration_completionHandler(None, &block)
        };
    }

    /// WKWebView has no URL before its first commit and after a failed first load; Wry's
    /// `WebView::url` unwraps it and aborts the app, so read it here.
    fn current_url(&self) -> Option<String> {
        use wry::WebViewExtMacOS;
        let webview = self.webview.webview();
        // SAFETY: plain property reads on a live WKWebView owned by this page, on the main thread.
        let url = unsafe { webview.URL() }?;
        let absolute = url.absoluteString()?;
        Some(absolute.to_string())
    }
}

thread_local! {
    /// Each page's event channel, by its WKWebView's address.
    static FAILURES: RefCell<HashMap<usize, Sender<Event>>> = RefCell::default();
    static EXCEPTIONS: RefCell<Exceptions> = RefCell::default();
}

pub fn allow_certificate(url: &str, certificate: Vec<u8>) {
    EXCEPTIONS.with_borrow_mut(|exceptions| exceptions.allow(url, certificate));
}

pub fn has_certificate_exception(url: &str) -> bool {
    EXCEPTIONS.with_borrow(|exceptions| exceptions.covers(url))
}

/// `SecTrustRef`'s pointee, so `msg_send!` can check the Objective-C type encoding.
#[repr(C)]
struct SecTrust {
    _private: [u8; 0],
}
// SAFETY: matches the `^{__SecTrust=}` encoding of `serverTrust` and `credentialForTrust:`.
unsafe impl objc2::RefEncode for SecTrust {
    const ENCODING_REF: objc2::Encoding =
        objc2::Encoding::Pointer(&objc2::Encoding::Struct("__SecTrust", &[]));
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecTrustGetCertificateAtIndex(trust: *const SecTrust, index: isize) -> *const c_void;
    fn SecCertificateCopyData(certificate: *const c_void) -> *mut objc2_foundation::NSData;
}

/// The server's leaf certificate (DER), read without evaluating trust, which could hit the network.
fn leaf_certificate(trust: *const SecTrust) -> Option<Vec<u8>> {
    // SAFETY: `trust` is a live SecTrustRef or null. The leaf is owned by the trust (Get rule);
    // the copied CFData, toll-free bridged to NSData, is released when the `Retained` drops.
    unsafe {
        let leaf = SecTrustGetCertificateAtIndex(trust.as_ref()?, 0);
        if leaf.is_null() {
            return None;
        }
        objc2::rc::Retained::from_raw(SecCertificateCopyData(leaf)).map(|data| data.to_vec())
    }
}

/// The credential for a certificate the user accepted; `None` leaves the challenge to WebKit.
fn accepted_credential(
    challenge: &objc2::runtime::AnyObject,
) -> Option<objc2::rc::Retained<objc2::runtime::AnyObject>> {
    use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{NSString, ns_string};
    // SAFETY: property reads on a live NSURLAuthenticationChallenge and its protection space,
    // on the main thread, with the types Foundation declares.
    unsafe {
        let space: Retained<AnyObject> = msg_send![challenge, protectionSpace];
        let method: Retained<NSString> = msg_send![&*space, authenticationMethod];
        if &*method != ns_string!("NSURLAuthenticationMethodServerTrust") {
            return None;
        }
        let host: Retained<NSString> = msg_send![&*space, host];
        let port: isize = msg_send![&*space, port];
        let trust: *mut SecTrust = msg_send![&*space, serverTrust];
        let certificate = leaf_certificate(trust)?;
        let allowed = EXCEPTIONS.with_borrow(|exceptions| {
            exceptions
                .allows(&host.to_string(), u16::try_from(port).ok()?, &certificate)
                .then_some(())
        });
        allowed?;
        msg_send![class!(NSURLCredential), credentialForTrust: trust]
    }
}

/// Adds failure and certificate methods to Wry's navigation delegate; failures go to `sender`.
fn watch_failures(webview: &WebView, sender: Sender<Event>) -> Option<usize> {
    use objc2::runtime::{AnyObject, Imp, Sel};
    use objc2::{msg_send, rc::Retained, sel};
    use wry::WebViewExtMacOS;

    type Callback = unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut AnyObject,
        *mut AnyObject,
        *mut objc2_foundation::NSError,
    );
    unsafe extern "C-unwind" fn failed(
        _: *mut AnyObject,
        selector: Sel,
        webview: *mut AnyObject,
        _: *mut AnyObject,
        error: *mut objc2_foundation::NSError,
    ) {
        let provisional = selector == sel!(webView:didFailProvisionalNavigation:withError:);
        // SAFETY: WebKit passes a live error, or null, on the main thread.
        report_failure(webview as usize, unsafe { error.as_ref() }, provisional);
    }
    type Handler = block2::Block<dyn Fn(isize, *mut AnyObject)>;
    type ChallengeCallback = unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut AnyObject,
        *mut AnyObject,
        *mut Handler,
    );
    unsafe extern "C-unwind" fn challenge(
        _: *mut AnyObject,
        _: Sel,
        _: *mut AnyObject,
        challenge: *mut AnyObject,
        handler: *mut Handler,
    ) {
        // SAFETY: WebKit passes a live challenge and completion handler on the main thread,
        // and expects the handler to be called exactly once.
        let handler = unsafe { &*handler };
        match unsafe { challenge.as_ref() }.and_then(accepted_credential) {
            // NSURLSessionAuthChallengeUseCredential
            Some(credential) => handler.call((0, Retained::as_ptr(&credential).cast_mut())),
            // NSURLSessionAuthChallengePerformDefaultHandling
            None => handler.call((1, std::ptr::null_mut())),
        }
    }

    let view = webview.webview();
    // SAFETY: a property read on a live WKWebView, on the main thread. Wry keeps the delegate
    // alive (WKWebView only holds it weakly) for as long as the WebView.
    let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![&*view, navigationDelegate] };
    let delegate = delegate?;
    let class = delegate.class();
    // SAFETY: `class` is Wry's registered delegate class. Adding a method only succeeds if the
    // class lacks it, so Wry's own methods are never replaced and later pages add nothing. Each
    // function matches its type encoding: void, self, _cmd, then three objects (`@`), the last
    // one a block (`@?`) for the challenge.
    unsafe {
        let class = class as *const _ as *mut _;
        let failed = std::mem::transmute::<Callback, Imp>(failed);
        for selector in [
            sel!(webView:didFailProvisionalNavigation:withError:),
            sel!(webView:didFailNavigation:withError:),
        ] {
            objc2::ffi::class_addMethod(class, selector, failed, c"v@:@@@".as_ptr());
        }
        objc2::ffi::class_addMethod(
            class,
            sel!(webView:didReceiveAuthenticationChallenge:completionHandler:),
            std::mem::transmute::<ChallengeCallback, Imp>(challenge),
            c"v@:@@@?".as_ptr(),
        );
    }
    // WebKit caches which methods a delegate implements when it is set, so set it again.
    // SAFETY: the same live delegate, on the main thread.
    let _: () = unsafe { msg_send![&*view, setNavigationDelegate: &*delegate] };
    let key = Retained::as_ptr(&view) as usize;
    FAILURES.with_borrow_mut(|pages| pages.insert(key, sender));
    Some(key)
}

/// Runs inside a WebKit callback, so it only queues an event for the UI.
fn report_failure(key: usize, error: Option<&objc2_foundation::NSError>, provisional: bool) {
    let failure = error.filter(|_| provisional).and_then(|error| {
        let info = error.userInfo();
        let url = info
            .objectForKey(objc2_foundation::ns_string!("NSErrorFailingURLKey"))
            .and_then(|url| url.downcast::<objc2_foundation::NSURL>().ok())
            .and_then(|url| url.absoluteString())
            .map(|url| url.to_string())
            .unwrap_or_default();
        let domain = error.domain().to_string();
        let code = error.code() as i64;
        let mut failure = crate::load_error::failure(
            &domain,
            code,
            &url,
            &error.localizedDescription().to_string(),
        )?;
        // A bad date, issuer or name can be overridden; a failed handshake cannot.
        if domain == "NSURLErrorDomain" && (-1204..=-1201).contains(&code) {
            failure.certificate = info
                .objectForKey(objc2_foundation::ns_string!(
                    "NSURLErrorFailingURLPeerTrustErrorKey"
                ))
                .and_then(|trust| leaf_certificate(objc2::rc::Retained::as_ptr(&trust).cast()));
        }
        Some(failure)
    });
    let event = failure.map_or(Event::Stopped, Event::Failed);
    FAILURES.with_borrow(|pages| {
        if let Some(sender) = pages.get(&key) {
            let _ = sender.send(event);
        }
    });
}

/// The content bridge has no privileged commands: only link status and web-tab requests.
fn parse_link_event(message: &str) -> Option<Event> {
    if message.len() > 8_256 {
        return None;
    }
    let (kind, value) = message.strip_prefix("felifox-link:")?.split_once(':')?;
    if value.is_empty() {
        return match kind {
            "hover" => Some(Event::Hover(None)),
            "focus" => Some(Event::Focused),
            _ => None,
        };
    }
    if value.chars().any(char::is_control) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    let url = url.to_string();
    match kind {
        "hover" => Some(Event::Hover(Some(url))),
        "open" => Some(Event::Open(url)),
        "background" => Some(Event::OpenInBackground(url)),
        _ => None,
    }
}

/// Icons arrive as `data:image/png;base64,…`; anything but a small PNG is dropped.
fn parse_icon_event(message: &str) -> Option<Event> {
    crate::icons::png_from_data_url(message.strip_prefix("felifox-icon:")?).map(Event::Icon)
}

/// Gives Command+arrow keys to the focused page. AppKit offers each Command key to GPUI as a
/// key equivalent first, and the text input context of GPUI consumes the arrows. Thus WebKit
/// did not move the caret to the start or the end of a line. Call this once, on the main thread.
#[cfg(target_os = "macos")]
pub fn forward_command_arrows() {
    use objc2::{ClassType, runtime::NSObjectProtocol};
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
    let handler = block2::RcBlock::new(|event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit gives a live event on the main thread.
        let key = unsafe { event.as_ref() };
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return event.as_ptr();
        };
        // Key codes 123 to 126 are the arrows.
        if (123..=126).contains(&key.keyCode())
            && key.modifierFlags().contains(NSEventModifierFlags::Command)
            && let Some(responder) = key.window(mtm).and_then(|window| window.firstResponder())
            && responder.isKindOfClass(objc2_web_kit::WKWebView::class())
        {
            responder.keyDown(key);
            return std::ptr::null_mut();
        }
        event.as_ptr()
    });
    // SAFETY: the handler only reads the event. The monitor stays for the life of the app.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
    };
    std::mem::forget(monitor);
}

/// Agent control is opt-in. The new setting takes precedence, including an explicit disable.
/// Keep the legacy name for existing local automation without enabling either by default.
pub fn agent_enabled() -> bool {
    agent_flag(
        std::env::var_os("FELINAUT_AGENT").as_deref(),
        std::env::var_os("FELIFOX_AGENT").as_deref(),
    )
}

fn agent_flag(current: Option<&std::ffi::OsStr>, legacy: Option<&std::ffi::OsStr>) -> bool {
    current.or(legacy).is_some_and(|value| value == "1")
}

/// Splits `Meta+Shift+ArrowLeft` into the modifier flags and the key name.
fn native_modifiers(mut key: &str) -> (objc2_app_kit::NSEventModifierFlags, &str) {
    use objc2_app_kit::NSEventModifierFlags as Flags;
    let mut flags = Flags::empty();
    'prefixes: loop {
        for (prefix, flag) in [
            ("Meta+", Flags::Command),
            ("Alt+", Flags::Option),
            ("Shift+", Flags::Shift),
            ("Control+", Flags::Control),
        ] {
            if let Some(rest) = key.strip_prefix(prefix).filter(|rest| !rest.is_empty()) {
                flags |= flag;
                key = rest;
                continue 'prefixes;
            }
        }
        return (flags, key);
    }
}

/// The macOS virtual key code and the characters for a DOM key name.
fn native_key(key: &str) -> Option<(u16, String)> {
    let (code, character) = match key {
        "Enter" => (36, '\r'),
        "Tab" => (48, '\t'),
        "Escape" => (53, '\u{1b}'),
        "Backspace" => (51, '\u{7f}'),
        "Space" | " " => (49, ' '),
        "Delete" => (117, '\u{f728}'),
        "ArrowLeft" => (123, '\u{f702}'),
        "ArrowRight" => (124, '\u{f703}'),
        "ArrowDown" => (125, '\u{f701}'),
        "ArrowUp" => (126, '\u{f700}'),
        "Home" => (115, '\u{f729}'),
        "End" => (119, '\u{f72b}'),
        "PageUp" => (116, '\u{f72c}'),
        "PageDown" => (121, '\u{f72d}'),
        // ponytail: one character has key code 0, thus `event.code` is always "KeyA".
        // Add a layout table if a site reads `code`, or for modifiers with a character.
        _ => {
            let mut characters = key.chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                return None;
            }
            (0, character)
        }
    };
    Some((code, character.to_string()))
}

/// The pointer in the window's content, top-left origin, or `None` outside it. Read from AppKit
/// because GPUI hears no mouse moves while the pointer is over a native page.
#[cfg(target_os = "macos")]
pub fn pointer(window: &gpui::Window) -> Option<gpui::Point<f32>> {
    use objc2_app_kit::NSView;
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // Fully qualified: `Window` has an inherent `window_handle` returning GPUI's own handle.
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    // SAFETY: GPUI hands out its live content view, and we are on the main thread.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let location = view.window()?.mouseLocationOutsideOfEventStream();
    let point = view.convertPoint_fromView(location, None);
    let size = view.bounds().size;
    let y = if view.isFlipped() {
        point.y
    } else {
        size.height - point.y
    };
    ((0.0..=size.width).contains(&point.x) && (0.0..=size.height).contains(&y))
        .then(|| gpui::point(point.x as f32, y as f32))
}
#[cfg(not(target_os = "macos"))]
pub fn pointer(_: &gpui::Window) -> Option<gpui::Point<f32>> {
    None
}

fn allowed_url(url: &str) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http")
            || scheme.eq_ignore_ascii_case("https")
            || scheme.eq_ignore_ascii_case("about")
    })
}

fn check_url(url: &str) -> Result<(), String> {
    if allowed_url(url) {
        Ok(())
    } else {
        Err("Only http, https, and about navigation is allowed".into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn renamed_agent_flag_preserves_opt_in_and_honors_explicit_disable() {
        use std::ffi::OsStr;
        let value = |text: &'static str| Some(OsStr::new(text));
        for (current, legacy, expected) in [
            (None, None, false),
            (None, value("1"), true),
            (None, value("true"), false),
            (value("1"), None, true),
            (value("1"), value("0"), true),
            (value("0"), value("1"), false),
            (value(""), value("1"), false),
            (value("true"), value("1"), false),
        ] {
            assert_eq!(super::agent_flag(current, legacy), expected);
        }
    }

    #[test]
    fn link_bridge_is_bounded_and_accepts_only_web_links() {
        use super::{Event, parse_link_event};
        assert_eq!(
            parse_link_event("felifox-link:hover:https://example.com/a?q=1"),
            Some(Event::Hover(Some("https://example.com/a?q=1".into())))
        );
        assert_eq!(
            parse_link_event("felifox-link:hover:"),
            Some(Event::Hover(None))
        );
        assert_eq!(
            parse_link_event("felifox-link:focus:"),
            Some(Event::Focused)
        );
        assert_eq!(
            parse_link_event("felifox-link:focus:https://x.example"),
            None
        );
        assert!(matches!(
            parse_link_event("felifox-link:background:https://example.com"),
            Some(Event::OpenInBackground(_))
        ));
        for message in [
            "felifox-link:open:javascript:alert(1)",
            "felifox-link:open:file:///etc/passwd",
            "felifox-link:open:about:blank",
            "felifox-link:delete:https://example.com",
            "felifox-link:hover:https://example.com/\n",
            "unknown",
        ] {
            assert_eq!(parse_link_event(message), None, "{message}");
        }
        assert_eq!(
            parse_link_event(&format!(
                "felifox-link:hover:https://example.com/{}",
                "x".repeat(9000)
            )),
            None
        );
    }

    #[test]
    fn icon_bridge_accepts_only_small_pngs() {
        use super::{Event, parse_icon_event};
        let pixel = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        assert!(matches!(
            parse_icon_event(&format!("felifox-icon:{pixel}")),
            Some(Event::Icon(bytes)) if bytes.len() == 70
        ));
        for message in [
            "felifox-icon:data:image/svg+xml;base64,PHN2Zz4=",
            "felifox-icon:https://example.com/favicon.ico",
            "felifox-link:hover:https://example.com",
        ] {
            assert_eq!(parse_icon_event(message), None, "{message}");
        }
    }

    #[test]
    fn native_keys_are_named_keys_or_one_character() {
        use super::native_key;
        assert_eq!(native_key("Enter"), Some((36, "\r".into())));
        assert_eq!(native_key("é"), Some((0, "é".into())));
        assert_eq!(native_key("Control+a"), None);
        use objc2_app_kit::NSEventModifierFlags as Flags;
        assert_eq!(
            super::native_modifiers("Meta+Shift+ArrowLeft"),
            (Flags::Command | Flags::Shift, "ArrowLeft")
        );
        assert_eq!(super::native_modifiers("+"), (Flags::empty(), "+"));
        assert_eq!(super::native_modifiers("Alt+"), (Flags::empty(), "Alt+"));
        assert_eq!(native_key(""), None);
    }

    #[test]
    fn permits_only_web_and_about_schemes() {
        for url in ["http://example.com", "HTTPS://example.com", "about:blank"] {
            assert!(super::allowed_url(url), "{url}");
        }
        for url in [
            "javascript:alert(1)",
            "data:text/html,test",
            "file:///etc/passwd",
            "mailto:test@example.com",
            "custom:open",
            "example.com",
            "//example.com",
            " https://example.com",
            "ht\ntps://example.com",
            "",
        ] {
            assert!(!super::allowed_url(url), "{url}");
        }
    }
}
