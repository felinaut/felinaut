//! CEF page operations, compiled by the opt-in native harness until host integration
//! is ready. CEF initialization, child-view ownership and asynchronous close remain
//! the runtime owner's responsibility, not one initialization/shutdown per page.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use cef::{Browser, CefString, ImplBrowser, ImplBrowserHost, ImplFrame};
use gpui::{Bounds, Pixels};

use crate::{
    load_error::{Kind, LoadFailure},
    page::{BackendInfo, Event, PageBackend, RequestScope, ScriptCallback, ScriptRequests},
};

#[path = "cef/evaluation.rs"]
pub mod evaluation;
#[cfg(feature = "engine-cef")]
pub use cef_protocol as protocol;
#[cfg(not(feature = "engine-cef"))]
#[path = "cef/protocol.rs"]
pub mod protocol;
use evaluation::Evaluation;

#[cfg(feature = "engine-cef")]
#[path = "cef/runtime.rs"]
pub mod runtime;

const EVENT_LIMIT: usize = 256;
const TEXT_LIMIT: usize = 16 * 1024;

/// Adapter-local native presentation, never exposed to page scripts or browser models.
/// The runtime must retain its native parent until asynchronous CEF close completes.
pub trait Presentation {
    fn set_visible(&self, visible: bool) -> Result<(), String>;
    fn focus_parent(&self) -> Result<(), String>;
    fn set_bounds(&self, bounds: Bounds<Pixels>) -> Result<(), String>;
}

/// Callbacks only enqueue bounded notifications and invalidate script scopes. They
/// do not retain the page or re-enter GPUI. Poll `drain_events` on the owning thread.
#[derive(Clone, Default)]
pub struct Notifications {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    events: Mutex<VecDeque<Event>>,
    failed: AtomicBool,
    closed: AtomicBool,
    requests: ScriptRequests,
}

impl Notifications {
    pub fn script_requests(&self) -> ScriptRequests {
        self.shared.requests.clone()
    }

    pub fn push(&self, event: Event) {
        if self.shared.closed.load(Ordering::Acquire) || !self.shared.requests.scope().is_current()
        {
            return;
        }
        if matches!(event, Event::Started(_)) {
            self.shared.requests.advance_document();
        }
        let bounded = match &event {
            Event::Started(value)
            | Event::Finished(value)
            | Event::Title(value)
            | Event::Url(value)
            | Event::Open(value)
            | Event::OpenInBackground(value) => value.len() <= TEXT_LIMIT,
            Event::Hover(value) => value.as_ref().is_none_or(|value| value.len() <= TEXT_LIMIT),
            Event::Failed(failure) => {
                failure.url.len() <= TEXT_LIMIT
                    && failure.detail.len() <= TEXT_LIMIT
                    && failure.certificate.is_none()
            }
            Event::Icon(_) => false,
            _ => true,
        };
        let overflow = {
            let mut events = self
                .shared
                .events
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if bounded && events.len() < EVENT_LIMIT {
                events.push_back(event);
                false
            } else {
                events.clear();
                true
            }
        };
        if overflow {
            self.shared.failed.store(true, Ordering::Release);
            self.shared.requests.close();
        }
    }

    pub fn document_changed(&self) {
        self.shared.requests.advance_document();
    }

    pub fn closed(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.requests.close();
        self.shared
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    fn drain(&self) -> Vec<Event> {
        if self.shared.failed.swap(false, Ordering::AcqRel) {
            self.shared
                .events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
            return vec![Event::Failed(LoadFailure {
                url: String::new(),
                kind: Kind::Other,
                detail: "CEF notification queue exceeded its bound".into(),
                certificate: None,
            })];
        }
        self.shared
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .collect()
    }
}

pub struct CefPage {
    browser: Browser,
    info: BackendInfo,
    notifications: Notifications,
    presentation: Rc<dyn Presentation>,
    navigation_allowed: Box<dyn Fn(&str) -> bool>,
    document: RefCell<Option<(String, String)>>,
    evaluation: Evaluation,
}

impl CefPage {
    /// Attach only after `OnAfterCreated`, on CEF's UI/AppKit thread. The runtime
    /// keeps its own browser/native-parent references until `OnBeforeClose` and
    /// must drop this adapter before CEF shutdown. No native ownership is transferred.
    pub fn attach(
        browser: Browser,
        info: BackendInfo,
        notifications: Notifications,
        evaluation: Evaluation,
        presentation: Rc<dyn Presentation>,
        navigation_allowed: impl Fn(&str) -> bool + 'static,
    ) -> Result<Self, String> {
        if cef::currently_on(cef::ThreadId::UI) == 0 || browser.is_valid() == 0 {
            return Err("CEF page attachment requires a live browser on its UI thread".into());
        }
        evaluation.attach(&browser)?;
        Ok(Self {
            browser,
            info,
            notifications,
            presentation,
            navigation_allowed: Box::new(navigation_allowed),
            document: RefCell::new(None),
            evaluation,
        })
    }

    fn ready(&self) -> Result<(), String> {
        if self.notifications.shared.closed.load(Ordering::Acquire)
            || !self.notifications.shared.requests.scope().is_current()
            || self.browser.is_valid() == 0
        {
            Err("CEF page is closed or its callback channel failed".into())
        } else {
            Ok(())
        }
    }

    fn changing_document(&self) -> Result<(), String> {
        self.ready()?;
        self.notifications.document_changed();
        self.evaluation.invalidate();
        Ok(())
    }
}

impl PageBackend for CefPage {
    fn backend_info(&self) -> BackendInfo {
        self.info.clone()
    }

    fn navigate(&self, url: &str) -> Result<(), String> {
        self.ready()?;
        if !valid_navigation(url) || !(self.navigation_allowed)(url) {
            return Err("CEF navigation URL was rejected by native policy".into());
        }
        let frame = self
            .browser
            .main_frame()
            .filter(|frame| frame.is_valid() != 0)
            .ok_or("CEF main frame is unavailable")?;
        self.changing_document()?;
        frame.load_url(Some(&CefString::from(url)));
        Ok(())
    }

    fn back(&self) -> Result<(), String> {
        self.ready()?;
        if !self.can_go_back() {
            return Err("CEF has no previous page".into());
        }
        self.changing_document()?;
        self.browser.go_back();
        Ok(())
    }

    fn forward(&self) -> Result<(), String> {
        self.ready()?;
        if !self.can_go_forward() {
            return Err("CEF has no next page".into());
        }
        self.changing_document()?;
        self.browser.go_forward();
        Ok(())
    }

    fn reload(&self) -> Result<(), String> {
        self.changing_document()?;
        self.browser.reload();
        Ok(())
    }

    fn can_go_back(&self) -> bool {
        self.ready().is_ok() && self.browser.can_go_back() != 0
    }
    fn can_go_forward(&self) -> bool {
        self.ready().is_ok() && self.browser.can_go_forward() != 0
    }
    fn is_loading(&self) -> bool {
        self.ready().is_ok() && self.browser.is_loading() != 0
    }
    fn document(&self) -> Option<(String, String)> {
        self.ready().ok()?;
        self.document.borrow().clone()
    }
    fn set_visible(&self, visible: bool) -> Result<(), String> {
        self.ready()?;
        self.presentation.set_visible(visible)
    }
    fn focus(&self) -> Result<(), String> {
        self.ready()?;
        self.browser
            .host()
            .ok_or("CEF host is unavailable")?
            .set_focus(1);
        Ok(())
    }
    fn focus_parent(&self) -> Result<(), String> {
        self.ready()?;
        self.browser
            .host()
            .ok_or("CEF host is unavailable")?
            .set_focus(0);
        self.presentation.focus_parent()
    }
    fn set_bounds(&self, bounds: Bounds<Pixels>) -> Result<(), String> {
        self.ready()?;
        validate_bounds(bounds)?;
        self.presentation.set_bounds(bounds)
    }
    fn drain_events(&self) -> Vec<Event> {
        self.evaluation.expire(Instant::now());
        if self.ready().is_ok() {
            self.evaluation.reconcile(&self.browser);
        }
        let events = self.notifications.drain();
        for event in &events {
            match event {
                Event::Url(url) => {
                    let mut document = self.document.borrow_mut();
                    if document.as_ref().is_none_or(|(current, _)| current != url) {
                        *document = Some((url.clone(), String::new()));
                    }
                }
                Event::Title(title) => {
                    if let Some((_, current)) = self.document.borrow_mut().as_mut() {
                        *current = title.clone();
                    }
                }
                _ => {}
            }
        }
        events
    }
    fn evaluate(&self, script: &str, callback: ScriptCallback) -> Result<(), String> {
        self.ready()?;
        self.evaluation.evaluate(&self.browser, script, callback)
    }
    fn request_scope(&self) -> Option<RequestScope> {
        self.ready().ok()?;
        Some(self.notifications.shared.requests.scope())
    }
}

impl Drop for CefPage {
    fn drop(&mut self) {
        // Do not free the child/parent here: CEF close is asynchronous. Its runtime
        // owner retains those resources and dispatches close outside this drop.
        self.notifications.closed();
        self.evaluation.close();
    }
}

fn valid_navigation(value: &str) -> bool {
    if value.len() > TEXT_LIMIT || value.chars().any(char::is_control) {
        return false;
    }
    value == "about:blank"
        || url::Url::parse(value).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.has_host()
                && url.username().is_empty()
                && url.password().is_none()
        })
}

fn validate_bounds(bounds: Bounds<Pixels>) -> Result<(), String> {
    let x: f32 = bounds.origin.x.into();
    let y: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    if [x, y, width, height].iter().any(|value| !value.is_finite())
        || width < 0.
        || height < 0.
        || width > 16384.
        || height > 16384.
    {
        return Err("Invalid CEF page bounds".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};
    use std::sync::Mutex;

    #[test]
    fn navigation_rejects_privileged_credentialed_malformed_and_oversized_urls() {
        for value in [
            "https://example.test/",
            "http://127.0.0.1:1234/",
            "about:blank",
        ] {
            assert!(valid_navigation(value));
        }
        for value in [
            "",
            "relative",
            "javascript:alert(1)",
            "file:///tmp/test",
            "data:text/html,x",
            "about:settings",
            "https://user:secret@example.test",
            "https://example.test/\n",
        ] {
            assert!(!valid_navigation(value), "{value}");
        }
        assert!(!valid_navigation(&format!(
            "https://example.test/{}",
            "a".repeat(TEXT_LIMIT)
        )));
    }

    #[test]
    fn native_events_invalidate_document_before_the_ui_drain() {
        let notifications = Notifications::default();
        let scope = notifications.shared.requests.scope();
        let result = Arc::new(Mutex::new(None));
        let delivered = result.clone();
        let dispatch = notifications
            .shared
            .requests
            .prepare(
                Instant::now(),
                Box::new(move |value| *delivered.lock().unwrap() = Some(value)),
            )
            .unwrap();
        let completion = dispatch.completion();
        dispatch.accept();
        notifications.push(Event::Started("https://fixture.test".into()));
        assert!(!scope.is_current());
        assert_eq!(
            *result.lock().unwrap(),
            Some(Err(crate::page::ScriptError::DocumentChanged))
        );
        completion.complete(Ok("null".into()));
        assert_eq!(notifications.drain().len(), 1);
        assert!(notifications.drain().is_empty());
    }

    #[test]
    fn queue_and_payload_overflow_fail_closed_without_unbounded_retention() {
        for oversized in [false, true] {
            let notifications = Notifications::default();
            let scope = notifications.shared.requests.scope();
            if oversized {
                notifications.push(Event::Title("x".repeat(TEXT_LIMIT + 1)));
            } else {
                for _ in 0..=EVENT_LIMIT {
                    notifications.push(Event::Focused);
                }
            }
            assert!(!scope.is_current());
            assert!(matches!(
                notifications.drain().as_slice(),
                [Event::Failed(_)]
            ));
            assert!(notifications.drain().is_empty());
        }
    }

    #[test]
    fn close_discards_notifications_and_is_idempotent() {
        let notifications = Notifications::default();
        notifications.push(Event::Focused);
        let scope = notifications.shared.requests.scope();
        notifications.closed();
        notifications.closed();
        notifications.push(Event::Title("late".into()));
        assert!(!scope.is_current());
        assert!(notifications.drain().is_empty());
    }

    #[test]
    fn bounds_accept_hidden_and_fractional_sizes_and_reject_nonfinite_or_huge() {
        for width in [0., 0.5, 16384.] {
            assert!(
                validate_bounds(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(width), px(100.))
                ))
                .is_ok()
            );
        }
        for width in [-1., f32::NAN, f32::INFINITY, 16385.] {
            assert!(
                validate_bounds(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(width), px(100.))
                ))
                .is_err()
            );
        }
        assert!(
            validate_bounds(Bounds::new(
                point(px(f32::NAN), px(0.)),
                size(px(100.), px(100.))
            ))
            .is_err()
        );
    }
}
