//! Engine-neutral page operations used by the browser. Native resources stay in adapters.
//!
//! Pages are UI-thread-owned and must be dropped before their parent window. Notifications
//! are drained by the browser; native callbacks must not re-enter GPUI rendering.

use std::rc::Rc;

use gpui::{Bounds, Pixels};
use serde_json::{Value, json};

use crate::audio::{AudioReport, TabAudio};
use crate::load_error::LoadFailure;

#[path = "requests.rs"]
mod requests;
pub use requests::{RequestScope, ScriptError, ScriptRequests};

pub type Page = Rc<dyn PageBackend>;
pub type ScriptResult = Result<String, ScriptError>;
pub type ScriptCallback = Box<dyn Fn(ScriptResult) + Send + 'static>;
pub type SnapshotCallback = Box<dyn Fn(Result<Vec<u8>, String>) + 'static>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started(String),
    Finished(String),
    Title(String),
    Url(String),
    Open(String),
    OpenInBackground(String),
    Hover(Option<String>),
    Focused,
    Audio(AudioReport),
    Icon(Vec<u8>),
    /// A top-level load failed before any document was shown.
    Failed(LoadFailure),
    Stopped,
}

/// Optional operations, not a claim of general website or media compatibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub zoom: bool,
    pub audio_control: bool,
    pub trusted_input: bool,
    pub snapshot: bool,
    pub certificate_exceptions: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeSource {
    SystemSharedCache,
    ExternalFramework,
    #[default]
    Unverified,
}

impl RuntimeSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemSharedCache => "system-shared-cache",
            Self::ExternalFramework => "external-framework",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub source: RuntimeSource,
    pub image_uuid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendInfo {
    pub id: &'static str,
    pub engine_family: &'static str,
    /// The runtime's own version, not Felinaut's or a user-agent-derived version.
    pub runtime_version: Option<String>,
    pub runtime_identity: RuntimeIdentity,
    pub capabilities: Capabilities,
}

impl BackendInfo {
    /// Diagnostics deliberately exclude profile paths, URLs and account/session data.
    pub fn json(&self) -> Value {
        json!({
            "backend": self.id,
            "engine_family": self.engine_family,
            "runtime_version": self.runtime_version,
            "runtime_identity": {
                "source": self.runtime_identity.source.label(),
                "image_uuid": self.runtime_identity.image_uuid,
            },
            "capabilities": {
                "zoom": self.capabilities.zoom,
                "audio_control": self.capabilities.audio_control,
                "trusted_input": self.capabilities.trusted_input,
                "snapshot": self.capabilities.snapshot,
                "certificate_exceptions": self.capabilities.certificate_exceptions,
            },
        })
    }
}

/// Required operations have no success-shaped defaults. Optional ones fail explicitly.
/// Script requests use explicit completion errors and document scopes. Native notification
/// attribution, snapshots and profile selection still need their own lifetime contracts.
pub trait PageBackend {
    fn backend_info(&self) -> BackendInfo;
    fn navigate(&self, url: &str) -> Result<(), String>;
    fn back(&self) -> Result<(), String>;
    fn forward(&self) -> Result<(), String>;
    fn reload(&self) -> Result<(), String>;
    fn can_go_back(&self) -> bool;
    fn can_go_forward(&self) -> bool;
    fn is_loading(&self) -> bool;
    fn document(&self) -> Option<(String, String)>;
    fn set_visible(&self, visible: bool) -> Result<(), String>;
    fn focus(&self) -> Result<(), String>;
    fn focus_parent(&self) -> Result<(), String>;
    fn set_bounds(&self, bounds: Bounds<Pixels>) -> Result<(), String>;
    fn drain_events(&self) -> Vec<Event>;
    /// On Ok, exactly one callback resolves the accepted request, including cancellation.
    /// On dispatch Err, the callback is not invoked. Results retain the adapter's JSON text;
    /// reported backend failures and cancellation are not successful JSON values.
    /// Canceling result delivery does not forcibly abort or undo engine JavaScript.
    fn evaluate(&self, script: &str, callback: ScriptCallback) -> Result<(), String>;

    /// Opaque identity for trusted multi-step operations; unsupported adapters cannot
    /// safely enqueue native input after asynchronous JavaScript preflight.
    fn request_scope(&self) -> Option<RequestScope> {
        None
    }

    fn zoom(&self) -> Result<f64, String> {
        Err(self.unsupported("zoom"))
    }

    fn set_zoom(&self, _zoom: f64) -> Result<(), String> {
        Err(self.unsupported("zoom"))
    }

    fn apply_audio(&self, _audio: TabAudio) -> Result<(), String> {
        Err(self.unsupported("audio control"))
    }

    fn native_click(&self, _x: f64, _y: f64) -> Result<(), String> {
        Err(self.unsupported("trusted pointer input"))
    }

    fn native_text(&self, _text: &str) -> Result<(), String> {
        Err(self.unsupported("trusted text input"))
    }

    fn native_key(&self, _key: &str) -> Result<(), String> {
        Err(self.unsupported("trusted key input"))
    }

    fn screenshot(&self, callback: SnapshotCallback) {
        callback(Err(self.unsupported("snapshots")));
    }

    fn allow_certificate(&self, _url: &str, _certificate: Vec<u8>) -> Result<(), String> {
        Err(self.unsupported("certificate exceptions"))
    }

    fn has_certificate_exception(&self, _url: &str) -> bool {
        false
    }

    fn unsupported(&self, operation: &str) -> String {
        format!(
            "Backend {} does not support {operation}",
            self.backend_info().id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakePage {
        history: RefCell<Vec<String>>,
        position: Cell<usize>,
        events: RefCell<Vec<Event>>,
        closed: Cell<bool>,
    }

    impl PageBackend for FakePage {
        fn backend_info(&self) -> BackendInfo {
            BackendInfo {
                id: "test-backend",
                engine_family: "test-family",
                runtime_version: None,
                runtime_identity: RuntimeIdentity::default(),
                capabilities: Capabilities::default(),
            }
        }
        fn navigate(&self, url: &str) -> Result<(), String> {
            if self.closed.get() {
                return Err("page closed".into());
            }
            let mut history = self.history.borrow_mut();
            if !history.is_empty() {
                history.truncate(self.position.get() + 1);
            }
            history.push(url.into());
            self.position.set(history.len() - 1);
            self.events
                .borrow_mut()
                .extend([Event::Started(url.into()), Event::Finished(url.into())]);
            Ok(())
        }
        fn back(&self) -> Result<(), String> {
            if !self.can_go_back() {
                return Err("no previous page".into());
            }
            self.position.set(self.position.get() - 1);
            Ok(())
        }
        fn forward(&self) -> Result<(), String> {
            if !self.can_go_forward() {
                return Err("no next page".into());
            }
            self.position.set(self.position.get() + 1);
            Ok(())
        }
        fn reload(&self) -> Result<(), String> {
            let (url, _) = self.document().ok_or("no document")?;
            self.events.borrow_mut().push(Event::Finished(url));
            Ok(())
        }
        fn can_go_back(&self) -> bool {
            !self.closed.get() && self.position.get() > 0
        }
        fn can_go_forward(&self) -> bool {
            !self.closed.get() && self.position.get() + 1 < self.history.borrow().len()
        }
        fn is_loading(&self) -> bool {
            false
        }
        fn document(&self) -> Option<(String, String)> {
            if self.closed.get() {
                return None;
            }
            self.history
                .borrow()
                .get(self.position.get())
                .map(|url| (url.clone(), "Fixture".into()))
        }
        fn set_visible(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn focus(&self) -> Result<(), String> {
            Ok(())
        }
        fn focus_parent(&self) -> Result<(), String> {
            Ok(())
        }
        fn set_bounds(&self, _: Bounds<Pixels>) -> Result<(), String> {
            Ok(())
        }
        fn drain_events(&self) -> Vec<Event> {
            std::mem::take(&mut *self.events.borrow_mut())
        }
        fn evaluate(&self, _: &str, callback: ScriptCallback) -> Result<(), String> {
            if self.closed.get() {
                return Err("page closed".into());
            }
            callback(Ok("null".into()));
            Ok(())
        }
    }

    #[test]
    fn optional_operations_fail_without_claiming_capabilities() {
        let page: Page = Rc::new(FakePage::default());
        assert_eq!(page.backend_info().capabilities, Capabilities::default());
        assert!(page.zoom().unwrap_err().contains("does not support zoom"));
        assert!(page.set_zoom(1.5).is_err());
        assert!(page.apply_audio(TabAudio::default()).is_err());
        assert!(page.native_click(1., 2.).is_err());
        assert!(page.native_text("hello").is_err());
        assert!(page.native_key("Enter").is_err());
        assert!(
            page.allow_certificate("https://example.test", vec![1])
                .is_err()
        );
        assert!(!page.has_certificate_exception("https://example.test"));
        let result = Rc::new(RefCell::new(Vec::new()));
        let delivered = result.clone();
        page.screenshot(Box::new(move |png| delivered.borrow_mut().push(png)));
        let result = result.borrow();
        assert_eq!(result.len(), 1);
        assert!(result[0].as_ref().unwrap_err().contains("snapshots"));
    }

    #[test]
    fn browser_can_navigate_and_drain_events_through_an_erased_backend() {
        let page: Page = Rc::new(FakePage::default());
        assert!(page.document().is_none());
        assert!(page.back().is_err());
        assert!(page.forward().is_err());
        assert!(page.reload().is_err());
        page.navigate("https://first.test").unwrap();
        page.navigate("https://second.test").unwrap();
        assert!(page.can_go_back());
        assert!(!page.can_go_forward());
        page.back().unwrap();
        assert_eq!(page.document().unwrap().0, "https://first.test");
        assert!(page.can_go_forward());
        page.navigate("https://replacement.test").unwrap();
        assert!(!page.can_go_forward());
        assert_eq!(page.document().unwrap().0, "https://replacement.test");
        assert_eq!(page.drain_events().len(), 6);
        assert!(page.drain_events().is_empty());
    }

    #[test]
    fn script_result_and_closed_page_errors_cross_the_contract() {
        let native = Rc::new(FakePage::default());
        let page: Page = native.clone();
        let result = Arc::new(Mutex::new(None));
        let delivered = result.clone();
        page.evaluate(
            "null",
            Box::new(move |value| *delivered.lock().unwrap() = Some(value)),
        )
        .unwrap();
        assert_eq!(*result.lock().unwrap(), Some(Ok("null".into())));
        native.closed.set(true);
        assert!(page.navigate("https://example.test").is_err());
        assert!(page.document().is_none());
        assert!(
            page.evaluate("null", Box::new(|_| panic!("closed page callback")))
                .is_err()
        );
    }

    #[test]
    fn metadata_keeps_engine_family_separate_and_unknown_version_explicit() {
        let info = FakePage::default().backend_info().json();
        assert_eq!(info["backend"], "test-backend");
        assert_eq!(info["engine_family"], "test-family");
        assert!(info["runtime_version"].is_null());
        assert_eq!(info["capabilities"]["trusted_input"], false);
        assert_eq!(info["runtime_identity"]["source"], "unverified");
        assert!(info["runtime_identity"]["image_uuid"].is_null());
        assert_eq!(info.as_object().unwrap().len(), 5);
    }
}
