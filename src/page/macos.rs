//! The existing macOS adapter; no engine selection or alternate runtime is implied.

#[path = "macos/runtime.rs"]
mod runtime;

use std::rc::Rc;

use gpui::{Bounds, Pixels, Window};

use crate::audio::TabAudio;
use crate::webview::{self, NativePage};

use super::{
    BackendInfo, Capabilities, Event, Page, PageBackend, ScriptCallback, SnapshotCallback,
};

pub fn backend_info() -> BackendInfo {
    BackendInfo {
        id: "native-webkit",
        engine_family: "webkit",
        runtime_version: wry::webview_version()
            .ok()
            .filter(|version| !version.is_empty()),
        runtime_identity: runtime::identity(),
        capabilities: Capabilities {
            zoom: true,
            audio_control: true,
            trusted_input: true,
            snapshot: true,
            certificate_exceptions: true,
        },
    }
}

pub fn create(window: &Window, url: &str) -> Result<Page, String> {
    NativePage::new(window, url).map(|page| Rc::new(page) as Page)
}

pub fn create_hidden(window: &Window, url: &str) -> Result<Page, String> {
    NativePage::hidden(window, url).map(|page| Rc::new(page) as Page)
}

impl PageBackend for NativePage {
    fn backend_info(&self) -> BackendInfo {
        backend_info()
    }
    fn navigate(&self, url: &str) -> Result<(), String> {
        NativePage::navigate(self, url)
    }
    fn back(&self) -> Result<(), String> {
        NativePage::back(self)
    }
    fn forward(&self) -> Result<(), String> {
        NativePage::forward(self)
    }
    fn reload(&self) -> Result<(), String> {
        NativePage::reload(self)
    }
    fn can_go_back(&self) -> bool {
        NativePage::can_go_back(self)
    }
    fn can_go_forward(&self) -> bool {
        NativePage::can_go_forward(self)
    }
    fn is_loading(&self) -> bool {
        NativePage::is_loading(self)
    }
    fn document(&self) -> Option<(String, String)> {
        NativePage::document(self)
    }
    fn set_visible(&self, visible: bool) -> Result<(), String> {
        NativePage::set_visible(self, visible)
    }
    fn focus(&self) -> Result<(), String> {
        NativePage::focus(self)
    }
    fn focus_parent(&self) -> Result<(), String> {
        NativePage::focus_parent(self)
    }
    fn set_bounds(&self, bounds: Bounds<Pixels>) -> Result<(), String> {
        NativePage::set_bounds(self, bounds)
    }
    fn drain_events(&self) -> Vec<Event> {
        NativePage::drain_events(self)
    }
    fn evaluate(&self, script: &str, callback: ScriptCallback) -> Result<(), String> {
        NativePage::evaluate(self, script, callback)
    }
    fn zoom(&self) -> Result<f64, String> {
        Ok(NativePage::zoom(self))
    }
    fn set_zoom(&self, zoom: f64) -> Result<(), String> {
        NativePage::set_zoom(self, zoom)
    }
    fn apply_audio(&self, audio: TabAudio) -> Result<(), String> {
        NativePage::apply_audio(self, audio)
    }
    fn native_click(&self, x: f64, y: f64) -> Result<(), String> {
        NativePage::native_click(self, x, y)
    }
    fn native_text(&self, text: &str) -> Result<(), String> {
        NativePage::native_text(self, text)
    }
    fn native_key(&self, key: &str) -> Result<(), String> {
        NativePage::native_key(self, key)
    }
    fn screenshot(&self, callback: SnapshotCallback) {
        NativePage::screenshot(self, callback)
    }
    fn allow_certificate(&self, url: &str, certificate: Vec<u8>) -> Result<(), String> {
        webview::allow_certificate(url, certificate);
        Ok(())
    }
    fn has_certificate_exception(&self, url: &str) -> bool {
        webview::has_certificate_exception(url)
    }
}
