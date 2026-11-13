//! Native callbacks: bounded notifications and fixed-size lifecycle mailboxes only.
//! No GPUI calls, host objects, filesystem API or arbitrary page-message endpoint.
use super::{CONTEXT_READY, Evaluation, Mailbox, Notifications, policy};
use crate::{
    load_error::{Kind, LoadFailure},
    page::Event,
};
use cef::*;
use std::{
    rc::Rc as StdRc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn text(value: &CefString, limit: usize) -> Option<String> {
    let units = value.as_slice().unwrap_or_default();
    if units.len() > limit {
        return None;
    }
    let text = String::from_utf16(units).ok()?;
    (text.len() <= limit).then_some(text)
}
fn owned_text(value: CefStringUserfree, limit: usize) -> Option<String> {
    let raw: Option<&cef::sys::_cef_string_utf16_t> = (&value).into();
    if raw.is_some_and(|raw| raw.length > limit || (raw.length != 0 && raw.str_.is_null())) {
        return None;
    }
    text(&CefString::from(&value), limit)
}
fn failed(notifications: &Notifications, message: &'static str) {
    notifications.push(Event::Failed(LoadFailure {
        url: String::new(),
        kind: Kind::Other,
        detail: message.into(),
        certificate: None,
    }));
}
wrap_app! {
    pub(super) struct RuntimeApp { mock_keychain: bool }
    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> { Some(RuntimeProcess::new()) }
        fn on_before_command_line_processing(&self, _process: Option<&CefString>, command: Option<&mut CommandLine>) {
            if let Some(command) = command {
                let switch = CefString::from("disable-features");
                let existing = owned_text(command.switch_value(Some(&switch)), 16384).unwrap_or_default();
                command.append_switch_with_value(Some(&switch), Some(&CefString::from(policy::disabled_features(&existing).as_str())));
                for name in ["disable-notifications", "disable-background-networking", "disable-component-update", "no-first-run", "no-default-browser-check"] {
                    command.append_switch(Some(&CefString::from(name)));
                }
                // The only path to this flag validates and exclusively claims a
                // private, fresh, marked test HOME before loading any CEF code.
                if self.mock_keychain { command.append_switch(Some(&CefString::from("use-mock-keychain"))); }
            }
        }
    }
}
wrap_browser_process_handler! {
    struct RuntimeProcess;
    impl BrowserProcessHandler {
        fn on_context_initialized(&self) { CONTEXT_READY.store(true, Ordering::Release); }
        fn on_schedule_message_pump_work(&self, delay_ms: i64) { cef_host::pump::schedule(delay_ms); }
    }
}
wrap_client! {
    pub(super) struct RuntimeClient { mailbox: StdRc<Mailbox>, notifications: Notifications, evaluation: Evaluation }
    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> { Some(RuntimeLife::new(self.mailbox.clone(), self.notifications.clone(), self.evaluation.clone())) }
        fn load_handler(&self) -> Option<LoadHandler> { Some(RuntimeLoad::new(self.mailbox.clone(), self.notifications.clone(), self.evaluation.clone())) }
        fn display_handler(&self) -> Option<DisplayHandler> { Some(RuntimeDisplay::new(self.notifications.clone())) }
        fn request_handler(&self) -> Option<RequestHandler> { Some(RuntimeRequest::new(self.notifications.clone(), self.mailbox.renderer_failed.clone())) }
        fn download_handler(&self) -> Option<DownloadHandler> { Some(DenyDownloads::new()) }
        fn permission_handler(&self) -> Option<PermissionHandler> { Some(DenyPermissions::new()) }
        fn jsdialog_handler(&self) -> Option<JsdialogHandler> { Some(DenyDialogs::new()) }
        fn frame_handler(&self) -> Option<FrameHandler> { Some(RuntimeFrames::new(self.notifications.clone(), self.evaluation.clone())) }
        fn on_process_message_received(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, source: ProcessId, message: Option<&mut ProcessMessage>) -> i32 {
            self.evaluation.receive(browser, frame, source, message)
        }
    }
}
wrap_life_span_handler! {
    struct RuntimeLife { mailbox: StdRc<Mailbox>, notifications: Notifications, evaluation: Evaluation }
    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                let mut slot = self.mailbox.browser.borrow_mut();
                if slot.is_some() || self.mailbox.created.get() || self.mailbox.closed.get() {
                    self.mailbox.failed.set(Some("Duplicate or late CEF creation callback"));
                } else {
                    *slot = Some(browser.clone());
                    self.mailbox.created.set(true);
                }
            } else { self.mailbox.failed.set(Some("CEF creation callback had no browser")); }
        }
        fn do_close(&self, _browser: Option<&mut Browser>) -> i32 {
            self.mailbox.close_ready.set(true);
            // Suppress CEF closing GPUI's window. poll detaches only this page's
            // child views AFTER returning from this callback, then awaits close.
            1
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.notifications.closed();
            self.evaluation.close();
            self.mailbox.closed.set(true);
        }
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: i32, url: Option<&CefString>, _name: Option<&CefString>, _disposition: WindowOpenDisposition, _gesture: i32, _features: Option<&PopupFeatures>, _window: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra: Option<&mut Option<DictionaryValue>>, _no_access: Option<&mut i32>) -> i32 {
            if let Some(url) = url.and_then(|url| text(url, 16384)) && policy::navigation(&url) {
                self.notifications.push(Event::Open(url));
            }
            1
        }
    }
}
wrap_load_handler! {
    struct RuntimeLoad { mailbox: StdRc<Mailbox>, notifications: Notifications, evaluation: Evaluation }
    impl LoadHandler {
        fn on_load_start(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition: TransitionType) {
            if let Some(frame) = frame && frame.is_main() != 0 {
                self.mailbox.renderer_failed.store(false, Ordering::Release);
                let mut cycle = self.mailbox.load.get(); cycle.started(); self.mailbox.load.set(cycle);
                let url = owned_text(frame.url(), 16384).unwrap_or_default();
                self.notifications.push(Event::Started(url));
                self.evaluation.invalidate();
            }
        }
        fn on_load_end(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, _status: i32) {
            if let Some(frame) = frame && frame.is_main() != 0 && self.mailbox.load.get().successful_end() && !self.mailbox.renderer_failed.load(Ordering::Acquire) {
                self.notifications.push(Event::Finished(owned_text(frame.url(), 16384).unwrap_or_default()));
                if let Some(browser) = browser { self.evaluation.reconcile(browser); }
            }
        }
        fn on_load_error(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, error: Errorcode, _text: Option<&CefString>, url: Option<&CefString>) {
            if frame.is_some_and(|f| f.is_main() != 0) {
                let mut cycle = self.mailbox.load.get(); cycle.failed(); self.mailbox.load.set(cycle);
                let url = url.and_then(|url| text(url, 16384)).unwrap_or_default();
                if let Some(failure) = policy::failure(error.get_raw(), &url) {
                    self.notifications.document_changed(); self.evaluation.invalidate();
                    self.notifications.push(Event::Failed(failure));
                } else { self.notifications.push(Event::Stopped); }
            }
        }
    }
}
wrap_frame_handler! {
    struct RuntimeFrames { notifications: Notifications, evaluation: Evaluation }
    impl FrameHandler {
        fn on_frame_detached(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>) {
            if frame.is_some_and(|f| f.is_main() != 0) {
                self.notifications.document_changed(); self.evaluation.invalidate();
            }
        }
        fn on_main_frame_changed(&self, _browser: Option<&mut Browser>, _old: Option<&mut Frame>, _new: Option<&mut Frame>) {
            self.notifications.document_changed(); self.evaluation.invalidate();
        }
    }
}
wrap_display_handler! {
    struct RuntimeDisplay { notifications: Notifications }
    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.is_some_and(|f| f.is_main() != 0) && let Some(url) = url.and_then(|v| text(v, 16384)) && policy::navigation(&url) {
                self.notifications.push(Event::Url(url));
            }
        }
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(title) = title.and_then(|v| text(v, 16384)) { self.notifications.push(Event::Title(title)); }
            else { failed(&self.notifications, "CEF title exceeded its native bound"); }
        }
    }
}
wrap_request_handler! {
    struct RuntimeRequest { notifications: Notifications, renderer_failed: Arc<AtomicBool> }
    impl RequestHandler {
        fn on_before_browse(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>, _gesture: i32, _redirect: i32) -> i32 {
            let allowed = request.and_then(|r| owned_text(r.url(), 16384)).is_some_and(|url| policy::navigation(&url));
            if !allowed && frame.is_some_and(|f| f.is_main() != 0) {
                failed(&self.notifications, "CEF main-frame navigation was blocked by native URL policy");
            }
            i32::from(!allowed)
        }
        fn resource_request_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, navigation: i32, download: i32, _initiator: Option<&CefString>, disable: Option<&mut i32>) -> Option<ResourceRequestHandler> {
            resource_handler(request, navigation, download, disable)
        }
        fn on_certificate_error(&self, _browser: Option<&mut Browser>, _error: Errorcode, _url: Option<&CefString>, _ssl: Option<&mut Sslinfo>, _callback: Option<&mut Callback>) -> i32 { 0 }
        fn on_select_client_certificate(&self, _browser: Option<&mut Browser>, _proxy: i32, _host: Option<&CefString>, _port: i32, _certificates: Option<&[Option<X509Certificate>]>, callback: Option<&mut SelectClientCertificateCallback>) -> i32 {
            if let Some(callback) = callback { callback.select(None); }
            1
        }
        fn on_render_process_terminated(&self, _browser: Option<&mut Browser>, _status: TerminationStatus, _code: i32, _text: Option<&CefString>) {
            // RequestHandler also has IO-thread callbacks/destruction: it must
            // not retain AppKit objects or UI-only Rc/Evaluation state. Retire
            // request scopes immediately, then invalidate transport in UI poll.
            self.notifications.document_changed();
            self.renderer_failed.store(true, Ordering::Release);
            failed(&self.notifications, "CEF renderer terminated; reload the page to recover");
        }
    }
}
fn resource_handler(
    request: Option<&mut Request>,
    navigation: i32,
    download: i32,
    disable: Option<&mut i32>,
) -> Option<ResourceRequestHandler> {
    let policy = policy::RequestPolicy {
        navigation: navigation != 0,
        download: download != 0,
    };
    let allowed = request
        .and_then(|r| owned_text(r.url(), 2 * 1024 * 1024))
        .is_some_and(|url| policy.allows(&url));
    // Preserve CEF's network handler for allowed web resources; never fall through
    // to default handling for privileged/unknown schemes or unsupported downloads.
    if let Some(disable) = disable {
        *disable = i32::from(!allowed);
    }
    Some(RuntimeResources::new(policy))
}
wrap_request_context_handler! {
    pub(super) struct RuntimeContext;
    impl RequestContextHandler {
        fn resource_request_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, navigation: i32, download: i32, _initiator: Option<&CefString>, disable: Option<&mut i32>) -> Option<ResourceRequestHandler> {
            // Also covers service-worker requests without an associated Browser.
            resource_handler(request, navigation, download, disable)
        }
    }
}
wrap_resource_request_handler! {
    struct RuntimeResources { policy: policy::RequestPolicy }
    impl ResourceRequestHandler {
        fn on_before_resource_load(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _callback: Option<&mut Callback>) -> ReturnValue {
            if request.and_then(|r| owned_text(r.url(), 2 * 1024 * 1024)).is_some_and(|url| self.policy.allows(&url)) { ReturnValue::CONTINUE } else { ReturnValue::CANCEL }
        }
        fn on_resource_redirect(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, _response: Option<&mut Response>, new_url: Option<&mut CefString>) {
            if let Some(url) = new_url && !text(url, 2 * 1024 * 1024).is_some_and(|url| self.policy.allows(&url)) {
                *url = CefString::from("about:blank");
            }
        }
        fn on_protocol_execution(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, allow: Option<&mut i32>) {
            if let Some(allow) = allow { *allow = 0; }
        }
    }
}
wrap_download_handler! {
    struct DenyDownloads;
    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, _url: Option<&CefString>, _method: Option<&CefString>) -> i32 { 0 }
        fn on_before_download(&self, _browser: Option<&mut Browser>, _item: Option<&mut DownloadItem>, _name: Option<&CefString>, _callback: Option<&mut BeforeDownloadCallback>) -> i32 { 1 }
        fn on_download_updated(&self, _browser: Option<&mut Browser>, _item: Option<&mut DownloadItem>, callback: Option<&mut DownloadItemCallback>) {
            if let Some(callback) = callback { callback.cancel(); }
        }
    }
}
wrap_permission_handler! {
    struct DenyPermissions;
    impl PermissionHandler {
        fn on_request_media_access_permission(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _origin: Option<&CefString>, _permissions: u32, callback: Option<&mut MediaAccessCallback>) -> i32 {
            if let Some(callback) = callback { callback.cancel(); }
            1
        }
        fn on_show_permission_prompt(&self, _browser: Option<&mut Browser>, _id: u64, _origin: Option<&CefString>, _permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> i32 {
            if let Some(callback) = callback { callback.cont(PermissionRequestResult::DENY); }
            1
        }
    }
}
wrap_jsdialog_handler! {
    struct DenyDialogs;
    impl JsdialogHandler {
        fn on_jsdialog(&self, _browser: Option<&mut Browser>, _origin: Option<&CefString>, _kind: JsdialogType, _message: Option<&CefString>, _default: Option<&CefString>, _callback: Option<&mut JsdialogCallback>, suppress: Option<&mut i32>) -> i32 {
            if let Some(suppress) = suppress { *suppress = 1; }
            0
        }
        fn on_before_unload_dialog(&self, _browser: Option<&mut Browser>, _message: Option<&CefString>, _reload: i32, callback: Option<&mut JsdialogCallback>) -> i32 {
            // Close is forceful until a shared unsaved-work prompt is supported.
            // No native dialog may hold up application-wide shutdown indefinitely.
            if let Some(callback) = callback { callback.cont(1, None); }
            1
        }
    }
}
