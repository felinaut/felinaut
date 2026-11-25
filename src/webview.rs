//! Native pages live on GPUI's UI thread and must be dropped before their parent window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};

use gpui::{Bounds, Pixels};
use objc2::{
    msg_send,
    rc::Retained,
    runtime::{AnyObject, Sel},
    sel,
};
use wry::{NewWindowResponse, PageLoadEvent, WebView, WebViewBuilder};

use crate::audio::{AudioReport, TabAudio, parse_report};
use crate::identity;
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
    /// Whether the last `find` matched.
    Found(bool),
    /// The page got a find shortcut and did not take it: 0 is ⌘F, 1 is ⌘G and -1 is ⇧⌘G.
    FindKey(i8),
    /// A frame or resource of the page was trusted with a certificate the user accepted.
    /// Connections WebKit reuses without a challenge are not reported.
    UsedException,
}

/// One persistent native child view per tab; hiding it does not discard its history or document.
/// The owner should poll `drain_events` on the UI thread, including for inactive tabs.
pub struct NativePage {
    webview: WebView,
    events: Receiver<Event>,
    last_url: RefCell<String>,
    failure_key: Option<usize>,
    /// A tab page's own view, sized to the page area: a docked Web Inspector stays in it, and
    /// hides with its tab.
    container: Option<Retained<objc2_app_kit::NSView>>,
    /// Whether the page is on show. Its message handler reads this: a hidden tab must not
    /// stop the swipe of the page that the user sees.
    shown: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Drop for NativePage {
    fn drop(&mut self) {
        if let Some(key) = self.failure_key {
            FAILURES.with_borrow_mut(|pages| pages.remove(&key));
            crate::downloads::native::detach(key);
        }
        // Denies the requests that still wait, while the WKWebView is alive.
        crate::permissions::native::detach(self.permission_key());
        if let Some(container) = &self.container {
            container.removeFromSuperview();
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
        let blocked_sender = sender.clone();
        let shown = std::rc::Rc::new(std::cell::Cell::new(false));
        let shown_to_handler = shown.clone();
        let mut builder = WebViewBuilder::new().with_user_agent(identity::user_agent());
        if agent_enabled() {
            builder =
                builder.with_initialization_script_for_main_only(include_str!("agent.js"), true);
        }
        let webview = builder
            .with_devtools(true)
            // In each frame: the keys go to the frame that has the focus.
            .with_initialization_script(include_str!("find_keys.js"))
            .with_initialization_script_for_main_only(include_str!("link_interactions.js"), false)
            .with_initialization_script_for_main_only(include_str!("audio_control.js"), false)
            .with_initialization_script_for_main_only(include_str!("page_icon.js"), true)
            // In each frame: a frame can scroll sideways too.
            .with_initialization_script(include_str!("swipe_scroll.js"))
            .with_ipc_handler(move |request| {
                let body = request.body();
                if body == crate::swipe::PAGE_SCROLLED {
                    if shown_to_handler.get() {
                        page_scrolled();
                    }
                    return;
                }
                let event = parse_link_event(body)
                    .or_else(|| parse_icon_event(body))
                    .or_else(|| parse_find_key(body))
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
            .with_navigation_handler(move |url| {
                let main_frame = MAIN_FRAME.get();
                let decision =
                    EXCEPTIONS.with_borrow(|exceptions| navigation(exceptions, &url, main_frame));
                match decision {
                    Navigation::Allow => true,
                    Navigation::Refuse => false,
                    Navigation::Report(failure) => {
                        let _ = blocked_sender.send(Event::Failed(failure));
                        false
                    }
                }
            })
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
            let view = webview.webview();
            crate::downloads::native::attach(&view, failure_key.is_some());
            crate::permissions::native::attach(&view, true);
            // Keep the in-app inspector but stop Safari's Develop menu attaching from outside.
            // SAFETY: a live WKWebView on the main thread; the setter exists on macOS 13.3+.
            unsafe {
                if msg_send![&*view, respondsToSelector: objc2::sel!(setInspectable:)] {
                    let () = msg_send![&*view, setInspectable: false];
                }
            }
        }
        let container = contain(&webview);

        Ok(Self {
            webview,
            events,
            last_url: RefCell::new(String::new()),
            failure_key,
            container,
            shown,
        })
    }

    /// A page the user never sees, for reading another site with the shared WebKit logins:
    /// no injected scripts, no new windows, only load events. It stays hidden and offscreen.
    pub fn hidden(window: &gpui::Window, url: &str) -> Result<Self, String> {
        check_url(url)?;
        let (sender, events) = mpsc::channel();
        let webview = WebViewBuilder::new()
            .with_user_agent(identity::user_agent())
            .with_visible(false)
            .with_focused(false)
            .with_devtools(false)
            // A real viewport: rows that never get a layout may render empty.
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(-20_000., -20_000.).into(),
                size: wry::dpi::LogicalSize::new(1024., 768.).into(),
            })
            // WebKit can reuse a connection a tab trusted with an exception without asking
            // this page, so never request a site with one; this also covers redirects and frames.
            .with_navigation_handler(|url| {
                EXCEPTIONS.with_borrow(|exceptions| {
                    navigation(exceptions, &url, false) == Navigation::Allow
                        && !exceptions.covers(&url)
                })
            })
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
            // Nor use the camera or the microphone.
            crate::permissions::native::attach(&webview.webview(), false);
        }
        Ok(Self {
            webview,
            events,
            last_url: RefCell::new(String::new()),
            failure_key: None,
            container: None,
            shown: Default::default(),
        })
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native link interaction test")]
    pub fn evaluate_for_test(&self, script: &str) -> Result<(), String> {
        self.webview
            .evaluate_script(script)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native permission test")]
    pub fn probe_capture_for_test(&self, capture_type: isize) -> Vec<isize> {
        use wry::WebViewExtMacOS;
        crate::permissions::native::probe(&self.webview.webview(), capture_type)
    }

    /// Applies a tab's volume, mute and EQ to the current document and all its frames.
    pub fn apply_audio(&self, audio: TabAudio) -> Result<(), String> {
        self.webview
            .evaluate_script(&audio.script())
            .map_err(|error| error.to_string())
    }

    /// The key of this page in `permissions::native`: its WKWebView's address.
    pub fn permission_key(&self) -> usize {
        use wry::WebViewExtMacOS;
        objc2::rc::Retained::as_ptr(&self.webview.webview()) as usize
    }

    /// Whether the page uses the camera and the microphone now.
    pub fn capture(&self) -> (bool, bool) {
        use wry::WebViewExtMacOS;
        crate::permissions::native::capture(&self.webview.webview())
    }

    pub fn stop_capture(&self) {
        use wry::WebViewExtMacOS;
        crate::permissions::native::stop_capture(&self.webview.webview());
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

    /// Selects and scrolls to the next match, and reports `Event::Found`. WebKit starts at the
    /// current selection and skips it only when it already is a match, so a longer or shorter
    /// query refines the current match, in frames too, and repeating a query steps to the next.
    pub fn find(&self, text: &str, backwards: bool) {
        use objc2::MainThreadOnly;
        use objc2_web_kit::{WKFindConfiguration, WKFindResult};
        use wry::WebViewExtMacOS;
        let sender = self
            .failure_key
            .and_then(|key| FAILURES.with_borrow(|pages| pages.get(&key).cloned()));
        let block = block2::RcBlock::new(move |result: std::ptr::NonNull<WKFindResult>| {
            // SAFETY: WebKit passes a live result on the main thread.
            let found = unsafe { result.as_ref().matchFound() };
            if let Some(sender) = &sender {
                let _ = sender.send(Event::Found(found));
            }
        });
        let webview = self.webview.webview();
        // SAFETY: a live WKWebView on the main thread; WebKit copies the block.
        unsafe {
            let configuration = WKFindConfiguration::new(webview.mtm());
            configuration.setBackwards(backwards);
            webview.findString_withConfiguration_completionHandler(
                &objc2_foundation::NSString::from_str(text),
                Some(&configuration),
                &block,
            );
        }
    }

    /// Clears the selection that the last match left, in the page and its frames. The script
    /// runs in WebKit's client world, where the page's own scripts can neither replace the
    /// functions it calls nor see it run; the page can still observe `selectionchange`.
    pub fn deselect(&self) {
        use objc2::MainThreadOnly;
        use objc2_web_kit::WKContentWorld;
        use wry::WebViewExtMacOS;
        let webview = self.webview.webview();
        // SAFETY: a live WKWebView on the main thread; no frame means the main frame, and no
        // completion handler means the result is dropped.
        unsafe {
            webview.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                &objc2_foundation::NSString::from_str(DESELECT),
                None,
                &WKContentWorld::defaultClientWorld(webview.mtm()),
                None,
            );
        }
    }

    /// Opens WebKit's Web Inspector where it was last (docked in the page or in its own
    /// window), or closes it if open.
    pub fn toggle_devtools(&self) {
        use wry::WebViewExtMacOS;
        let view = self.webview.webview();
        // SAFETY: a live WKWebView on the main thread.
        if let Some(inspector) = unsafe { inspector_of(&view) } {
            // SAFETY: the page's own `_WKInspector`, on the main thread.
            unsafe { toggle_inspector(&inspector) };
        }
    }

    /// The page's `isInspectable` (macOS 13.3+): whether Safari's Develop menu may attach.
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native Web Inspector test")]
    pub fn inspectable_for_test(&self) -> Option<bool> {
        use wry::WebViewExtMacOS;
        let view = self.webview.webview();
        // SAFETY: a live WKWebView on the main thread; the getter is checked first.
        responds(&view, sel!(isInspectable)).then(|| unsafe { msg_send![&*view, isInspectable] })
    }

    /// Whether WebKit's developer extras (Inspect Element, the inspector) are on.
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native Web Inspector test")]
    pub fn developer_extras_for_test(&self) -> bool {
        use wry::WebViewExtMacOS;
        let view = self.webview.webview();
        // SAFETY: a live WKWebView's configuration and preferences, on the main thread.
        unsafe {
            let configuration: Retained<AnyObject> = msg_send![&*view, configuration];
            let preferences: Retained<AnyObject> = msg_send![&*configuration, preferences];
            assert!(responds(&preferences, sel!(_developerExtrasEnabled)));
            msg_send![&*preferences, _developerExtrasEnabled]
        }
    }

    /// The tab page's container view and where the WKWebView sits in it.
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native Web Inspector test")]
    pub fn container_for_test(&self) -> Option<ContainerForTest> {
        use wry::WebViewExtMacOS;
        let container = self.container.as_ref()?;
        let view = self.webview.webview();
        // SAFETY: views this page owns, on the main thread.
        let parent = unsafe { container.superview() }?;
        let frame = container.frame();
        Some(ContainerForTest {
            hidden: container.isHidden(),
            frame: (
                frame.origin.x,
                frame.origin.y,
                frame.size.width,
                frame.size.height,
            ),
            parent_height: parent.frame().size.height,
            parent_flipped: parent.isFlipped(),
            holds_page: unsafe { view.superview() }.as_deref() == Some(&**container),
            page_hidden: view.isHidden(),
            page_fills: view.frame() == container.bounds(),
        })
    }

    /// The page's Web Inspector, once WebKit has made one.
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native Web Inspector test")]
    pub fn inspector_for_test(&self) -> Option<InspectorForTest> {
        use wry::WebViewExtMacOS;
        let view = self.webview.webview();
        // SAFETY: a live WKWebView and its `_WKInspector`, on the main thread.
        unsafe {
            let inspector = inspector_of(&view)?;
            let page: Option<Retained<objc2_app_kit::NSView>> =
                msg_send![&*inspector, inspectorWebView];
            let in_container = match (&page, &self.container) {
                (Some(page), Some(container)) => page.isDescendantOf(container),
                _ => false,
            };
            Some(InspectorForTest {
                visible: msg_send![&*inspector, isVisible],
                detached: inspector_is_detached(&inspector),
                front: msg_send![&*inspector, isFront],
                in_container,
                window_class: page
                    .and_then(|page| page.window())
                    .map(|window| window.class().name().to_string_lossy().into_owned()),
            })
        }
    }

    /// Docks (`attach`) or detaches the open Web Inspector.
    #[cfg(test)]
    #[allow(dead_code, reason = "Used by the opt-in native Web Inspector test")]
    pub fn dock_inspector_for_test(&self, docked: bool) {
        use wry::WebViewExtMacOS;
        let view = self.webview.webview();
        // SAFETY: a live WKWebView and its `_WKInspector`, on the main thread.
        unsafe {
            let inspector = inspector_of(&view).expect("an inspector");
            if docked {
                let () = msg_send![&*inspector, attach];
            } else {
                let () = msg_send![&*inspector, detach];
            }
        }
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

    /// Whether the page came with a certificate the user accepted, even over a reused connection.
    pub fn uses_accepted_certificate(&self) -> bool {
        self.served_certificate()
            .is_some_and(|leaf| EXCEPTIONS.with_borrow(|exceptions| exceptions.accepted(&leaf)))
    }

    fn served_certificate(&self) -> Option<Vec<u8>> {
        use wry::WebViewExtMacOS;
        // SAFETY: a property read on a live WKWebView owned by this page, on the main thread;
        // the trust (or null) belongs to the view and is only read before this returns.
        let trust: *mut SecTrust =
            unsafe { objc2::msg_send![&*self.webview.webview(), serverTrust] };
        leaf_certificate(trust)
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
        self.shown.set(visible);
        if let Some(container) = &self.container {
            container.setHidden(!visible);
            return Ok(());
        }
        self.webview
            .set_visible(visible)
            .map_err(|error| error.to_string())
    }

    /// Whether the pointer is over this page, for a swipe in a split view.
    pub fn under_pointer(&self) -> bool {
        let Some(container) = self.container.as_ref().filter(|_| self.shown.get()) else {
            return false;
        };
        let Some(window) = container.window() else {
            return false;
        };
        let point =
            container.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
        let size = container.bounds().size;
        (0. ..size.width).contains(&point.x) && (0. ..size.height).contains(&point.y)
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
        if let Some(container) = &self.container {
            // SAFETY: a view this page owns, on the main thread.
            let parent = unsafe { container.superview() }.ok_or("The page has no parent view")?;
            let (x, y) = (f64::from(bounds.origin.x), f64::from(bounds.origin.y));
            let (width, height) = (f64::from(bounds.size.width), f64::from(bounds.size.height));
            let y = container_y(y, height, parent.frame().size.height, parent.isFlipped());
            container.setFrame(objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(x, y),
                objc2_foundation::NSSize::new(width, height),
            ));
            return Ok(());
        }
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
        if !modifiers.is_empty() && code == 0 {
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
    static FORGET_CHOSEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the navigation Wry's policy handler is deciding targets a main frame. Wry's
    /// handler gets only the URL; `watch_failures` sets this around it.
    static MAIN_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    #[cfg(test)]
    static SUBFRAME_DECISIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many frame (not main-frame) navigations the policy hook has seen, for the native test.
#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native webview test")]
pub fn subframe_decisions() -> usize {
    SUBFRAME_DECISIONS.get()
}

pub fn allow_certificate(url: &str, certificate: Vec<u8>) {
    EXCEPTIONS.with_borrow_mut(|exceptions| exceptions.allow(url, certificate));
}

pub fn is_blocked(url: &str) -> bool {
    EXCEPTIONS.with_borrow(|exceptions| exceptions.blocked(url))
}

/// Try Again on an error page: lifts a forgotten site's block, trusting no certificate.
pub fn retry(failure: &LoadFailure) {
    EXCEPTIONS.with_borrow_mut(|exceptions| exceptions.retry(failure));
}

pub fn forget_certificate_exceptions() {
    EXCEPTIONS.with_borrow_mut(Exceptions::forget);
}

/// Blocks the site of a page that Forget closed. See `Exceptions::block`.
pub fn block_site(url: &str) {
    EXCEPTIONS.with_borrow_mut(|exceptions| exceptions.block(url));
}

/// Whether the user accepted a bad certificate for the site of `url`, so the hidden page,
/// which carries the logged-in sessions, must not load it.
pub fn has_certificate_exception(url: &str) -> bool {
    EXCEPTIONS.with_borrow(|exceptions| exceptions.covers(url))
}

/// Explains the Not secure mark in a sheet, which is drawn above the native pages.
pub fn explain_not_secure(window: &gpui::Window, host: &str) {
    use objc2_app_kit::{NSAlert, NSAlertSecondButtonReturn, NSView};
    use objc2_foundation::{NSString, ns_string};
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let (RawWindowHandle::AppKit(handle), Some(mtm)) =
        (handle.as_raw(), objc2::MainThreadMarker::new())
    else {
        return;
    };
    // SAFETY: GPUI hands out its live content view, and we are on the main thread.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let Some(ns_window) = view.window() else {
        return;
    };
    let alert = NSAlert::new(mtm);
    alert.setMessageText(ns_string!("This connection isn't private"));
    alert.setInformativeText(&NSString::from_str(&format!(
        "You chose to trust an invalid certificate for {host}, in all tabs, until \
         Felinaut quits. Anyone on your network could read or change what you send, including \
         passwords."
    )));
    alert.addButtonWithTitle(ns_string!("Keep"));
    alert.addButtonWithTitle(ns_string!("Forget Certificate Exceptions"));
    // AppKit calls this on the main thread; `take_forget_chosen` hands it to the UI drain.
    let chosen = block2::RcBlock::new(|response| {
        FORGET_CHOSEN.set(response == NSAlertSecondButtonReturn);
    });
    alert.beginSheetModalForWindow_completionHandler(&ns_window, Some(&chosen));
}

pub fn take_forget_chosen() -> bool {
    FORGET_CHOSEN.take()
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
        webview: *mut AnyObject,
        challenge: *mut AnyObject,
        handler: *mut Handler,
    ) {
        // SAFETY: WebKit passes a live challenge and completion handler on the main thread,
        // and expects the handler to be called exactly once.
        let handler = unsafe { &*handler };
        // The hidden live-folder page has no failure key, so it never answers a challenge with
        // an exception. That alone is not enough: see `NativePage::hidden`.
        let visible = FAILURES.with_borrow(|pages| pages.contains_key(&(webview as usize)));
        match unsafe { challenge.as_ref() }
            .filter(|_| visible)
            .and_then(accepted_credential)
        {
            Some(credential) => {
                // The page's `serverTrust` is its main frame's; tell the tab about the rest.
                FAILURES.with_borrow(|pages| {
                    if let Some(sender) = pages.get(&(webview as usize)) {
                        let _ = sender.send(Event::UsedException);
                    }
                });
                // NSURLSessionAuthChallengeUseCredential
                handler.call((0, Retained::as_ptr(&credential).cast_mut()));
            }
            // NSURLSessionAuthChallengePerformDefaultHandling
            None => handler.call((1, std::ptr::null_mut())),
        }
    }

    type PolicyHandler = block2::Block<dyn Fn(isize, *mut AnyObject)>;
    type PolicyCallback = unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut AnyObject,
        *mut AnyObject,
        *mut AnyObject,
        *mut PolicyHandler,
    );
    /// WebKit prefers this variant when the delegate has it. It notes whether the main frame
    /// navigates, then lets Wry's own method decide, and hands its policy on with the same
    /// preferences.
    unsafe extern "C-unwind" fn decide(
        delegate: *mut AnyObject,
        _: Sel,
        webview: *mut AnyObject,
        action: *mut AnyObject,
        preferences: *mut AnyObject,
        handler: *mut PolicyHandler,
    ) {
        // SAFETY: WebKit passes a live delegate, web view, navigation action, preferences and
        // completion handler on the main thread. The handler is copied (retained) so that it
        // outlives this call if Wry ever answers later; Wry calls `forward` exactly once.
        let handler = unsafe { &*handler }.copy();
        let main_frame = targets_main_frame(unsafe { action.as_ref() }.and_then(|action| {
            // SAFETY: `targetFrame` is a nullable WKFrameInfo; `isMainFrame` returns a BOOL.
            let frame: Option<Retained<AnyObject>> = unsafe { msg_send![action, targetFrame] };
            frame.map(|frame| unsafe { msg_send![&*frame, isMainFrame] })
        }));
        // SAFETY: a live WKWebpagePreferences or null; retained for as long as `forward` lives.
        let preferences = unsafe { Retained::retain(preferences) };
        let forward = block2::RcBlock::new(move |policy: isize| {
            let preferences = preferences
                .as_ref()
                .map_or(std::ptr::null_mut(), |p| Retained::as_ptr(p).cast_mut());
            handler.call((policy, preferences));
        });
        #[cfg(test)]
        SUBFRAME_DECISIONS.set(SUBFRAME_DECISIONS.get() + usize::from(!main_frame));
        let outer = MAIN_FRAME.replace(main_frame);
        // SAFETY: Wry's delegate implements this selector with these argument types (objects,
        // then a block taking WKNavigationActionPolicy, an NSInteger), on the main thread.
        let _: () = unsafe {
            msg_send![
                delegate,
                webView: webview,
                decidePolicyForNavigationAction: action,
                decisionHandler: &*forward
            ]
        };
        MAIN_FRAME.set(outer);
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
    // one a block (`@?`) for the challenge, or four with a block last for the policy.
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
        objc2::ffi::class_addMethod(
            class,
            sel!(webView:decidePolicyForNavigationAction:preferences:decisionHandler:),
            std::mem::transmute::<PolicyCallback, Imp>(decide),
            c"v@:@@@@?".as_ptr(),
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
        if crate::load_error::offers_override(&domain, code, &url) {
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
/// A find shortcut that the page did not take, from `find_keys.js`. A page can send this
/// message itself; it only opens the find bar.
fn parse_find_key(message: &str) -> Option<Event> {
    match message.strip_prefix("felifox-find:")? {
        "0" => Some(Event::FindKey(0)),
        "1" => Some(Event::FindKey(1)),
        "-1" => Some(Event::FindKey(-1)),
        _ => None,
    }
}

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

/// Whether the focused page gets a key before GPUI. `command`, `control` and `option` are the
/// modifiers of the key.
/// - Command+arrows (key codes 123 to 126): the text input context of GPUI consumes them, thus
///   WebKit did not move the caret to the start or the end of a line.
/// - ⌘F, ⌘G and ⇧⌘G (key codes 3 and 5): a page can have a find of its own. WebKit does not
///   give a key that the page ignored back to AppKit, so `find_keys.js` reports that key
///   (`Event::FindKey`), and the find bar opens then.
fn page_gets_key_first(key_code: u16, command: bool, control: bool, option: bool) -> bool {
    command
        && ((123..=126).contains(&key_code) || (matches!(key_code, 3 | 5) && !control && !option))
}

/// Gives the keys of `page_gets_key_first` to the focused page. AppKit offers each Command key
/// to GPUI as a key equivalent first. Call this once, on the main thread.
#[cfg(target_os = "macos")]
pub fn forward_page_keys() {
    use objc2::{ClassType, runtime::NSObjectProtocol};
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
    let handler = block2::RcBlock::new(|event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit gives a live event on the main thread.
        let key = unsafe { event.as_ref() };
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return event.as_ptr();
        };
        let flags = key.modifierFlags();
        if page_gets_key_first(
            key.keyCode(),
            flags.contains(NSEventModifierFlags::Command),
            flags.contains(NSEventModifierFlags::Control),
            flags.contains(NSEventModifierFlags::Option),
        ) && let Some(responder) = key.window(mtm).and_then(|window| window.firstResponder())
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

/// What the swipe monitor needs between events. AppKit delivers events on the main thread only.
#[cfg(target_os = "macos")]
struct SwipeWatch {
    /// The browser window; swipes over other windows, like Settings, are left alone.
    window: objc2::rc::Weak<objc2_app_kit::NSWindow>,
    sender: Sender<crate::swipe::Step>,
    tracker: crate::swipe::Tracker,
}

#[cfg(target_os = "macos")]
thread_local! {
    static SWIPES: RefCell<Option<SwipeWatch>> = const { RefCell::new(None) };
}

thread_local! {
    /// Wakes the UI task waiting in `Swipes::next`.
    static SWIPE_WAKER: RefCell<Option<std::task::Waker>> = const { RefCell::new(None) };
}

/// Back and forward steps from trackpad swipes, in the order they were made.
pub struct Swipes(Receiver<crate::swipe::Step>);

impl Swipes {
    /// The next step, once one is made; `None` when the monitor is gone. Waking the UI task
    /// schedules it on the main run loop: it never runs inside the event monitor itself.
    pub async fn next(&self) -> Option<crate::swipe::Step> {
        std::future::poll_fn(|cx| match self.0.try_recv() {
            Ok(step) => std::task::Poll::Ready(Some(step)),
            Err(mpsc::TryRecvError::Disconnected) => std::task::Poll::Ready(None),
            Err(mpsc::TryRecvError::Empty) => {
                // The monitor sends on this same thread, so nothing arrives in between.
                SWIPE_WAKER.set(Some(cx.waker().clone()));
                std::task::Poll::Pending
            }
        })
        .await
    }
}

/// Watches the trackpad for swipes that go back or forward in `window`; see `swipe` for which
/// ones. Events are only read, never consumed, except a three-finger swipe that made a step.
/// Call this on the main thread; a second call moves the watch to the new window.
#[cfg(target_os = "macos")]
pub fn watch_swipes(window: &gpui::Window) -> Option<Swipes> {
    use objc2_app_kit::{NSEvent, NSEventMask, NSView};
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    // SAFETY: GPUI hands out its live content view, and we are on the main thread.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let ns_window = view.window()?;
    let (sender, steps) = mpsc::channel();
    let first = SWIPES.with_borrow_mut(|watch| {
        watch
            .replace(SwipeWatch {
                window: objc2::rc::Weak::from_retained(&ns_window),
                sender,
                tracker: Default::default(),
            })
            .is_none()
    });
    if first {
        let handler = block2::RcBlock::new(|event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit gives a live event on the main thread, and keeps it alive for the
            // duration of this call; the handler only reads it.
            let consumed = swipe_event(unsafe { event.as_ref() });
            if consumed {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        // SAFETY: the handler upholds the monitor's contract: it returns the event it was
        // given or null. The monitor stays for the life of the app.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::ScrollWheel | NSEventMask::Swipe,
                &handler,
            )
        };
        std::mem::forget(monitor);
    }
    Some(Swipes(steps))
}
#[cfg(not(target_os = "macos"))]
pub fn watch_swipes(_: &gpui::Window) -> Option<Swipes> {
    None
}

/// Feeds one scroll or swipe event to the tracker; true when it should go no further.
#[cfg(target_os = "macos")]
fn swipe_event(event: &objc2_app_kit::NSEvent) -> bool {
    use crate::swipe::{Phase, Sample, swipe_step};
    use objc2_app_kit::{NSEvent, NSEventPhase, NSEventType, NSWindow};
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return false;
    };
    let sent = SWIPES.with_borrow_mut(|watch| {
        let Some(watch) = watch.as_mut() else {
            return false;
        };
        let Some(own) = watch.window.load() else {
            return false;
        };
        // Trackpad events belong to the window under the pointer. Events posted without one,
        // like synthetic ones, carry screen coordinates instead.
        let ours = match event.window(mtm) {
            Some(window) => window == own,
            None => {
                let screen = event.locationInWindow();
                NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(screen, 0, mtm)
                    == own.windowNumber()
            }
        };
        if !ours {
            return false;
        }
        let step = match event.r#type() {
            NSEventType::ScrollWheel => {
                // Mouse wheels have no fingers to lift, so no gesture to end.
                if !event.hasPreciseScrollingDeltas() {
                    return false;
                }
                let phase = Phase::from_appkit(event.phase().0);
                // The setting matters only when a gesture begins.
                let allowed =
                    phase == Phase::Began && NSEvent::isSwipeTrackingFromScrollEventsEnabled();
                watch.tracker.feed(Sample {
                    dx: event.scrollingDeltaX(),
                    dy: event.scrollingDeltaY(),
                    phase,
                    momentum: event.momentumPhase() != NSEventPhase::None,
                    allowed,
                })
            }
            NSEventType::Swipe => swipe_step(event.deltaX(), event.deltaY()),
            _ => None,
        };
        step.is_some_and(|step| watch.sender.send(step).is_ok())
    });
    if !sent {
        return false;
    }
    // Waking only schedules the UI task on the main run loop; it does not run it here.
    if let Some(waker) = SWIPE_WAKER.take() {
        waker.wake();
    }
    // Scroll events still reach the page or chrome under the pointer.
    event.r#type() == NSEventType::Swipe
}

/// A page reported that the sideways scroll under way was its own, so the swipe does not also
/// navigate. Pages are untrusted: all one can do with this is keep its own swipe from
/// navigating, as a page that handles wheel events can in any browser.
fn page_scrolled() {
    #[cfg(target_os = "macos")]
    SWIPES.with(|watch| {
        // The page's message arrives from the run loop, never from inside the monitor; should
        // that ever change, dropping the report is better than a panic.
        if let Ok(mut watch) = watch.try_borrow_mut()
            && let Some(watch) = watch.as_mut()
        {
            watch.tracker.page_scrolled();
        }
    });
}

/// Moves a tab's page into its own view, sized to the page area: a docked Web Inspector
/// splits its page's parent view, which would otherwise be the whole window.
fn contain(webview: &WebView) -> Option<Retained<objc2_app_kit::NSView>> {
    use objc2::MainThreadOnly;
    use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
    use wry::WebViewExtMacOS;
    let view = webview.webview();
    // SAFETY: the page's live view, on the main thread.
    let parent = unsafe { view.superview() }?;
    let container =
        NSView::initWithFrame(NSView::alloc(objc2::MainThreadMarker::new()?), view.frame());
    container.setHidden(true);
    parent.addSubview(&container);
    view.removeFromSuperview();
    container.addSubview(&view);
    view.setFrame(container.bounds());
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    view.setHidden(false);
    Some(container)
}

/// A top-left `y` in the container's parent view, which is bottom-left unless flipped.
fn container_y(y: f64, height: f64, parent_height: f64, flipped: bool) -> f64 {
    if flipped {
        y
    } else {
        parent_height - y - height
    }
}

/// Whether `object` implements `selector`, so private WebKit API can be skipped if it is gone.
fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: `respondsToSelector:` is NSObject protocol API and has no side effects.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// A page's private `_WKInspector`, as Wry's `open_devtools` uses it. `None` if WebKit no
/// longer has the selector, or returns nil (as it does once the page has closed).
///
/// SAFETY: `view` is a live WKWebView (or any object), used on the main thread.
unsafe fn inspector_of(view: &AnyObject) -> Option<Retained<AnyObject>> {
    if !responds(view, sel!(_inspector)) {
        return None;
    }
    // SAFETY: the caller's guarantee; the selector exists and returns an object or nil.
    unsafe { msg_send![view, _inspector] }
}

/// A tab page's container, for the opt-in native test.
#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native Web Inspector test")]
#[derive(Debug)]
pub struct ContainerForTest {
    pub hidden: bool,
    /// x, y, width, height in the parent view's own coordinates.
    pub frame: (f64, f64, f64, f64),
    pub parent_height: f64,
    pub parent_flipped: bool,
    /// The WKWebView is the container's subview...
    pub holds_page: bool,
    /// ...is not hidden itself (the container hides it)...
    pub page_hidden: bool,
    /// ...and fills the container.
    pub page_fills: bool,
}

/// A page's Web Inspector, for the opt-in native test.
#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native Web Inspector test")]
#[derive(Clone, Debug)]
pub struct InspectorForTest {
    pub visible: bool,
    pub detached: bool,
    pub front: bool,
    /// The inspector's own page is docked inside the tab's container, not the whole window.
    pub in_container: bool,
    /// The class of the window the inspector's own page is in, if any.
    pub window_class: Option<String>,
}

/// What ⌥⌘I does to a page's inspector.
#[derive(Debug, PartialEq)]
enum InspectorToggle {
    /// Open it, or bring a detached one in front.
    Show,
    Close,
}

/// A docked inspector, or a detached one already in front, closes; a detached inspector behind
/// other windows comes to the front instead; a hidden one opens.
fn inspector_toggle(visible: bool, detached: bool, front: bool) -> InspectorToggle {
    if visible && (!detached || front) {
        InspectorToggle::Close
    } else {
        InspectorToggle::Show
    }
}

/// Shows a hidden inspector (or brings a detached one behind other windows to the front) and
/// closes a visible one. Selectors missing from a future WebKit are skipped instead of raising
/// an Objective-C exception.
///
/// SAFETY: `inspector` is a `_WKInspector` (or any object), used on the main thread.
unsafe fn toggle_inspector(inspector: &AnyObject) {
    if !responds(inspector, sel!(isVisible)) {
        return;
    }
    // SAFETY: the caller's guarantee; each selector is checked before it is sent.
    unsafe {
        let visible: bool = msg_send![inspector, isVisible];
        let detached = visible && inspector_is_detached(inspector);
        // `isFront` is only meaningful for a detached inspector; without it, close as before.
        let front =
            detached && (!responds(inspector, sel!(isFront)) || msg_send![inspector, isFront]);
        match inspector_toggle(visible, detached, front) {
            InspectorToggle::Close if responds(inspector, sel!(close)) => {
                let () = msg_send![inspector, close];
            }
            // WebKit's `show` brings an inspector that is already open to the front.
            InspectorToggle::Show if responds(inspector, sel!(show)) => {
                let () = msg_send![inspector, show];
            }
            _ => {}
        }
    }
}

/// Whether an inspector's own page sits in WebKit's detached inspector window rather than
/// docked in the inspected page's window.
///
/// SAFETY: `inspector` is a `_WKInspector` (or any object), used on the main thread.
unsafe fn inspector_is_detached(inspector: &AnyObject) -> bool {
    if !responds(inspector, sel!(inspectorWebView)) {
        return false;
    }
    // SAFETY: the caller's guarantee; both selectors are checked and may return nil.
    unsafe {
        let page: Option<Retained<AnyObject>> = msg_send![inspector, inspectorWebView];
        let Some(page) = page.filter(|page| responds(page, sel!(window))) else {
            return false;
        };
        let window: Option<Retained<AnyObject>> = msg_send![&*page, window];
        window.is_some_and(|window| is_inspector_window(reported_class_name(&window)))
    }
}

/// Closes the app's key window if it is a detached Web Inspector.
pub fn close_key_window() {
    if let Some(mtm) = objc2::MainThreadMarker::new()
        && let Some(window) = objc2_app_kit::NSApplication::sharedApplication(mtm).keyWindow()
        && is_inspector_window(reported_class_name(&window))
    {
        window.performClose(None);
    }
}

/// An object's class as it reports it (`-class`). That hides the subclass Key-Value Observing
/// swaps in (`NSKVONotifying_…`), which `object_getClass` returns: AppKit observes its
/// windows, so a detached inspector's window is really an `NSKVONotifying__WKInspectorWindow`.
fn reported_class_name(object: &AnyObject) -> &'static [u8] {
    // SAFETY: `-class` is NSObject protocol API, returns a class and has no side effects.
    let class: &'static objc2::runtime::AnyClass = unsafe { msg_send![object, class] };
    class.name().to_bytes()
}

/// Whether a window class is WebKit's detached Web Inspector window. A positive match: GPUI's
/// browser and Settings windows (`GPUIWindow`), its focusing popups (`GPUIPanel`) and WebKit's
/// other windows must not be closed behind their owners' backs. Should WebKit rename the class,
/// ⌘W and ⌥⌘I just stop closing a detached inspector; its own close button still works.
fn is_inspector_window(class: &[u8]) -> bool {
    class == b"_WKInspectorWindow"
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
        // The find shortcuts read the key code.
        "f" => (3, 'f'),
        "g" => (5, 'g'),
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

/// Empties the selection of a window and of its frames; cross-origin frames throw and are skipped.
const DESELECT: &str = "(function deselect(w) { \
    try { w.getSelection().removeAllRanges(); } catch (_) {} \
    try { for (let i = 0; i < w.frames.length; i++) deselect(w.frames[i]); } catch (_) {} \
})(window)";

#[derive(Debug, PartialEq, Eq)]
enum Navigation {
    Allow,
    Refuse,
    /// Refuse, and show this error page in the tab.
    Report(LoadFailure),
}

/// The navigation policy. Forgotten sites are refused before any request, because WebKit
/// would reuse the connection it trusted. Only a visible page's main frame (`reports`) shows the
/// error page: a frame on a forgotten site stays empty rather than taking over the tab.
fn navigation(exceptions: &Exceptions, url: &str, reports: bool) -> Navigation {
    if !allowed_url(url) {
        Navigation::Refuse
    } else if !exceptions.blocked(url) {
        Navigation::Allow
    } else if reports {
        Navigation::Report(crate::load_error::forgotten(url))
    } else {
        Navigation::Refuse
    }
}

/// Whether a navigation reports a refused site to its tab. `target_frame` is `isMainFrame` of
/// the target frame. A navigation with no target frame is a new window (`target=_blank`,
/// `window.open`): it must not replace the page of the tab that opened it with an error page.
fn targets_main_frame(target_frame: Option<bool>) -> bool {
    target_frame.unwrap_or(false)
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
    fn a_new_window_navigation_does_not_report_to_the_tab_that_opened_it() {
        assert!(super::targets_main_frame(Some(true)));
        assert!(!super::targets_main_frame(Some(false)), "a frame");
        assert!(!super::targets_main_frame(None), "a new window");
    }

    #[test]
    fn only_a_detached_inspector_window_closes_on_the_fallback() {
        // ⌘W or ⌥⌘I in a detached inspector closes just that window.
        assert!(super::is_inspector_window(b"_WKInspectorWindow"));
        // Browser and Settings windows, and GPUI's focusing popups (downloads list, volume
        // popover, dropdowns), keep their own close handling.
        assert!(!super::is_inspector_window(b"GPUIWindow"));
        assert!(!super::is_inspector_window(b"GPUIPanel"));
        // Nor any other AppKit window, such as WebKit's element full screen window.
        assert!(!super::is_inspector_window(b"NSWindow"));
        assert!(!super::is_inspector_window(b"WKFullScreenWindow"));
        assert!(!super::is_inspector_window(b""));
    }

    objc2::define_class!(
        /// A page whose inspector is gone: `_inspector` returns nil, as for a closed page.
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "FelinautTestClosedPage"]
        struct ClosedPage;

        impl ClosedPage {
            #[unsafe(method(_inspector))]
            fn inspector(&self) -> *mut objc2::runtime::AnyObject {
                std::ptr::null_mut()
            }
        }
    );

    objc2::define_class!(
        /// An inspector that reports itself visible but has lost its `close`.
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "FelinautTestPartialInspector"]
        struct PartialInspector;

        impl PartialInspector {
            #[unsafe(method(isVisible))]
            fn is_visible(&self) -> objc2::runtime::Bool {
                objc2::runtime::Bool::YES
            }
        }
    );

    objc2::define_class!(
        /// Something with a `level` property for Key-Value Observing.
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "FelinautTestObserved"]
        struct Observed;

        impl Observed {
            #[unsafe(method(level))]
            fn level(&self) -> isize {
                0
            }

            #[unsafe(method(setLevel:))]
            fn set_level(&self, _: isize) {}
        }
    );

    objc2::define_class!(
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "FelinautTestObserver"]
        struct Observer;

        impl Observer {
            #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
            fn observe(
                &self,
                _: *mut objc2::runtime::AnyObject,
                _: *mut objc2::runtime::AnyObject,
                _: *mut objc2::runtime::AnyObject,
                _: *mut std::ffi::c_void,
            ) {
            }
        }
    );

    #[test]
    fn a_key_value_observed_window_is_still_recognised_by_its_class() {
        use objc2::msg_send;
        let observed = new_object::<Observed>();
        let observer = new_object::<Observer>();
        let key = objc2_foundation::NSString::from_str("level");
        // SAFETY: plain NSObject subclasses; the observer is removed before either is dropped.
        unsafe {
            let () = msg_send![&*observed, addObserver: &*observer, forKeyPath: &*key, options: 0usize, context: std::ptr::null_mut::<std::ffi::c_void>()];
            // AppKit observes its windows the same way, which swaps in a KVO subclass.
            assert!(
                observed
                    .class()
                    .name()
                    .to_bytes()
                    .starts_with(b"NSKVONotifying_")
            );
            assert_eq!(
                super::reported_class_name(&observed),
                b"FelinautTestObserved"
            );
            let () = msg_send![&*observed, removeObserver: &*observer, forKeyPath: &*key];
        }
    }

    fn new_object<T: objc2::ClassType>() -> objc2::rc::Retained<objc2::runtime::AnyObject> {
        // SAFETY: `new` on an NSObject subclass with no extra state.
        unsafe { objc2::msg_send![T::class(), new] }
    }

    #[test]
    fn a_nil_or_missing_inspector_is_none_instead_of_a_panic() {
        // SAFETY: plain NSObject subclasses; none needs the main thread.
        unsafe {
            assert!(super::inspector_of(&new_object::<ClosedPage>()).is_none());
            assert!(super::inspector_of(&new_object::<objc2::runtime::NSObject>()).is_none());
        }
    }

    #[test]
    fn toggling_skips_inspector_selectors_webkit_no_longer_has() {
        // Without the guards these raise "unrecognized selector" and abort.
        // SAFETY: plain NSObject subclasses; none needs the main thread.
        unsafe {
            super::toggle_inspector(&new_object::<objc2::runtime::NSObject>());
            super::toggle_inspector(&new_object::<PartialInspector>());
        }
    }

    #[test]
    fn the_inspector_toggle_brings_a_detached_inspector_behind_others_to_the_front() {
        use super::{InspectorToggle::*, inspector_toggle};
        // (visible, detached, front)
        assert_eq!(
            inspector_toggle(false, false, false),
            Show,
            "a closed inspector opens"
        );
        assert_eq!(
            inspector_toggle(true, false, false),
            Close,
            "a docked inspector closes"
        );
        assert_eq!(
            inspector_toggle(true, true, true),
            Close,
            "a detached one in front closes"
        );
        assert_eq!(
            inspector_toggle(true, true, false),
            Show,
            "a detached one behind other windows comes to the front"
        );
    }

    #[test]
    fn container_y_flips_only_for_an_unflipped_parent() {
        assert_eq!(super::container_y(40., 300., 800., true), 40.);
        assert_eq!(super::container_y(40., 300., 800., false), 460.);
    }

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
    fn a_find_key_message_is_one_of_three_steps() {
        use super::{Event, parse_find_key};
        for (message, step) in [
            ("felifox-find:0", 0),
            ("felifox-find:1", 1),
            ("felifox-find:-1", -1),
        ] {
            assert!(matches!(parse_find_key(message), Some(Event::FindKey(s)) if s == step));
        }
        for message in [
            "felifox-find:",
            "felifox-find:2",
            "felifox-find:-2",
            "felifox-find:1 ",
            "felifox-find:01",
            "felifox-link:1",
            "",
        ] {
            assert!(parse_find_key(message).is_none(), "{message:?}");
        }
    }

    #[test]
    fn the_page_gets_command_arrows_and_the_find_keys_first() {
        use super::page_gets_key_first as first;
        for arrow in 123..=126 {
            assert!(first(arrow, true, false, false), "{arrow}");
            assert!(
                !first(arrow, false, false, false),
                "{arrow} without Command"
            );
        }
        // ⌘F, ⌘G (and ⇧⌘G: Shift is not a parameter).
        for code in [3, 5] {
            assert!(first(code, true, false, false), "{code}");
            assert!(!first(code, false, false, false), "a plain letter");
            // ⌃⌘F is full screen and ⌥⌘F is not a find key: they stay with the browser.
            assert!(!first(code, true, true, false), "{code} with Control");
            assert!(!first(code, true, false, true), "{code} with Option");
        }
        // Other shortcuts of the browser, such as ⌘T (17), ⌘W (13) and ⌘L (37).
        for code in [17, 13, 37, 0] {
            assert!(!first(code, true, false, false), "{code}");
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
    fn refuses_forgotten_sites_before_any_request() {
        use super::{Navigation, navigation};
        use crate::load_error::{Exceptions, Kind};
        let mut exceptions = Exceptions::default();
        assert_eq!(
            navigation(&exceptions, "https://wiki.corp.example/", true),
            Navigation::Allow
        );
        exceptions.allow("https://wiki.corp.example/", vec![1]);
        exceptions.forget();
        match navigation(&exceptions, "https://wiki.corp.example/a", true) {
            Navigation::Report(failure) => {
                assert_eq!(failure.kind, Kind::Forgotten);
                assert_eq!(failure.url, "https://wiki.corp.example/a");
            }
            other => panic!("{other:?}"),
        }
        // A frame and the hidden live-folder page refuse too, without an error page.
        assert_eq!(
            navigation(&exceptions, "https://wiki.corp.example/a", false),
            Navigation::Refuse
        );
        for reports in [true, false] {
            for url in [
                "https://other.example/",
                "http://wiki.corp.example/",
                "about:blank",
            ] {
                assert_eq!(
                    navigation(&exceptions, url, reports),
                    Navigation::Allow,
                    "{url}"
                );
            }
            assert_eq!(
                navigation(&exceptions, "file:///etc/passwd", reports),
                Navigation::Refuse
            );
        }
        // Try Again lifts the block.
        exceptions.retry(&crate::load_error::forgotten("https://wiki.corp.example/"));
        assert_eq!(
            navigation(&exceptions, "https://wiki.corp.example/a", false),
            Navigation::Allow
        );
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
