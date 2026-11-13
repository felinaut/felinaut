use super::*;
use page::selection::{Engine, validate_switch};
use page::{BackendInfo, Capabilities, PageBackend, RequestScope, ScriptCallback, ScriptRequests};
use std::cell::Cell;
// The root GPUI glob also exports a `test` macro; these are ordinary Rust unit tests.
use std::prelude::v1::test;
use std::sync::mpsc;

const URL: &str = "https://example.test/document";

struct FakePage {
    engine: Engine,
    ready: RefCell<Result<bool, String>>,
    readiness_polls: Cell<usize>,
    visibility: RefCell<Vec<bool>>,
    requests: ScriptRequests,
    drops: Rc<Cell<usize>>,
    document: RefCell<Option<(String, String)>>,
}

impl FakePage {
    fn new(engine: Engine, ready: Result<bool, String>) -> Rc<Self> {
        Rc::new(Self {
            engine,
            ready: RefCell::new(ready),
            readiness_polls: Cell::new(0),
            visibility: RefCell::new(Vec::new()),
            requests: ScriptRequests::default(),
            drops: Rc::new(Cell::new(0)),
            document: RefCell::new(None),
        })
    }

    fn become_ready(&self) {
        *self.ready.borrow_mut() = Ok(true);
    }
}

impl Drop for FakePage {
    fn drop(&mut self) {
        self.requests.close();
        self.drops.set(self.drops.get() + 1);
    }
}

impl PageBackend for FakePage {
    fn creation_ready(&self) -> Result<bool, String> {
        self.readiness_polls.set(self.readiness_polls.get() + 1);
        self.ready.borrow().clone()
    }

    fn backend_info(&self) -> BackendInfo {
        BackendInfo {
            id: self.engine.id(),
            engine_family: match self.engine {
                Engine::WebKit => "webkit",
                Engine::Cef => "chromium",
            },
            runtime_version: None,
            runtime_identity: page::RuntimeIdentity::default(),
            capabilities: Capabilities::default(),
        }
    }

    fn navigate(&self, _url: &str) -> Result<(), String> {
        Err(self.unsupported("navigation"))
    }

    fn back(&self) -> Result<(), String> {
        Err(self.unsupported("back"))
    }

    fn forward(&self) -> Result<(), String> {
        Err(self.unsupported("forward"))
    }

    fn reload(&self) -> Result<(), String> {
        Err(self.unsupported("reload"))
    }

    fn can_go_back(&self) -> bool {
        false
    }

    fn can_go_forward(&self) -> bool {
        false
    }

    fn is_loading(&self) -> bool {
        false
    }

    fn document(&self) -> Option<(String, String)> {
        self.document.borrow().clone()
    }

    fn set_visible(&self, visible: bool) -> Result<(), String> {
        self.visibility.borrow_mut().push(visible);
        Ok(())
    }

    fn focus(&self) -> Result<(), String> {
        Err(self.unsupported("focus"))
    }

    fn focus_parent(&self) -> Result<(), String> {
        Err(self.unsupported("parent focus"))
    }

    fn set_bounds(&self, _bounds: Bounds<Pixels>) -> Result<(), String> {
        Err(self.unsupported("bounds"))
    }

    fn drain_events(&self) -> Vec<Event> {
        Vec::new()
    }

    fn evaluate(&self, _script: &str, _callback: ScriptCallback) -> Result<(), String> {
        Err(self.unsupported("script evaluation"))
    }

    fn request_scope(&self) -> Option<RequestScope> {
        Some(self.requests.scope())
    }
}

fn loaded_tab(id: u64, engine: Engine) -> (Tab, Rc<FakePage>) {
    let original = FakePage::new(engine, Ok(true));
    let mut tab = Tab::new(id);
    tab.backend = engine.id().into();
    tab.start_load(URL.into());
    tab.loading = false;
    tab.title = "Original document".into();
    tab.page = Some(original.clone());
    (tab, original)
}

/// Supply a fake factory result; exercise the real Tab polling/commit implementation.
/// Both model availability flags are true because no native backend is created here.
fn stage_replacement(tab: &mut Tab, candidate: &Rc<FakePage>) {
    let plan = validate_switch(&tab.backend, candidate.engine, &tab.url, true, true, true)
        .expect("valid explicit switch")
        .expect("different backend");
    candidate.set_visible(false).unwrap();
    tab.replacement = Some(PageReplacement {
        plan,
        page: candidate.clone(),
        original: tab.page.clone(),
        scope: tab.page.as_ref().and_then(|page| page.request_scope()),
    });
}

fn assert_page(tab: &Tab, expected: &Rc<FakePage>) {
    let expected: Page = expected.clone();
    assert!(Rc::ptr_eq(
        tab.page.as_ref().expect("retained page"),
        &expected
    ));
}

fn assert_rejected(tab: &mut Tab, original: &Rc<FakePage>) {
    assert!(tab.poll_replacement());
    assert_eq!(tab.backend, "webkit");
    assert_page(tab, original);
    assert!(tab.replacement.is_none());
    assert!(tab.switch_error.as_deref().unwrap().contains("Tab changed"));
    assert!(original.visibility.borrow().is_empty());
    assert!(!tab.poll_replacement());
}

fn saved_tab(url: &str, backend: Option<&str>, pinned: bool, active: bool) -> session::SavedTab {
    session::SavedTab {
        url: url.into(),
        title: "Saved document".into(),
        backend: backend.map(str::to_owned),
        pinned,
        active,
        ..Default::default()
    }
}

#[test]
fn pending_readiness_preserves_original_page_and_backend() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(false));
    stage_replacement(&mut tab, &candidate);

    for _ in 0..3 {
        assert!(!tab.poll_replacement());
        assert_eq!(tab.backend, "webkit");
        assert_page(&tab, &original);
        assert_eq!(tab.title, "Original document");
        assert_eq!(tab.url, URL);
        assert!(!tab.loading);
        assert!(tab.switch_error.is_none());
        assert!(tab.replacement.is_some());
    }
    assert_eq!(candidate.readiness_polls.get(), 3);
    assert!(original.visibility.borrow().is_empty());
    assert_eq!(*candidate.visibility.borrow(), [false]);
    assert_eq!(original.drops.get(), 0);
}

#[test]
fn ready_replacement_swaps_once_and_hides_only_original() {
    for (from, to) in [(Engine::WebKit, Engine::Cef), (Engine::Cef, Engine::WebKit)] {
        let (mut tab, original) = loaded_tab(1, from);
        let candidate = FakePage::new(to, Ok(false));
        stage_replacement(&mut tab, &candidate);
        assert!(!tab.poll_replacement());
        tab.error = Some("Previous navigation failure".into());
        tab.switch_error = Some("Previous switch failure".into());
        tab.load_failure = load_error::failure("NSURLErrorDomain", -1003, URL, "Not found");
        assert!(tab.load_failure.is_some());

        candidate.become_ready();
        assert!(tab.poll_replacement());
        assert_eq!(tab.backend, to.id());
        assert_page(&tab, &candidate);
        assert!(tab.replacement.is_none());
        assert_eq!(tab.url, URL);
        assert_eq!(tab.title, "example.test");
        assert!(tab.loading);
        assert!(!tab.home);
        assert!(tab.error.is_none());
        assert!(tab.load_failure.is_none());
        assert!(tab.switch_error.is_none());
        assert_eq!(*original.visibility.borrow(), [false]);
        // Showing the new page belongs to the browser's visibility pass, not polling.
        assert_eq!(*candidate.visibility.borrow(), [false]);

        for _ in 0..3 {
            assert!(!tab.poll_replacement());
            assert_page(&tab, &candidate);
            assert_eq!(tab.backend, to.id());
        }
        assert_eq!(candidate.readiness_polls.get(), 2);
        assert_eq!(*original.visibility.borrow(), [false]);
    }
}

#[test]
fn successful_swap_releases_original_scope_and_cancels_its_pending_requests() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(false));
    let scope = original.requests.scope();
    let original_weak = Rc::downgrade(&original);
    let drops = original.drops.clone();
    let (sender, receiver) = mpsc::channel();
    let dispatch = original
        .requests
        .prepare(
            Instant::now(),
            Box::new(move |result| {
                sender.send(result).unwrap();
            }),
        )
        .unwrap();
    let completion = dispatch.completion();
    dispatch.accept();
    stage_replacement(&mut tab, &candidate);
    drop(original);

    assert!(!tab.poll_replacement());
    assert!(scope.is_current());
    assert!(original_weak.upgrade().is_some());
    assert_eq!(drops.get(), 0);
    assert!(receiver.try_recv().is_err());

    candidate.become_ready();
    assert!(tab.poll_replacement());
    assert_page(&tab, &candidate);
    assert_eq!(tab.backend, "cef");
    assert!(original_weak.upgrade().is_none());
    assert_eq!(drops.get(), 1);
    assert!(!scope.is_current());
    assert_eq!(
        receiver.try_recv().unwrap(),
        Err(page::ScriptError::PageClosed)
    );
    completion.complete(Ok("null".into()));
    assert!(receiver.try_recv().is_err());
    assert!(candidate.requests.scope().is_current());
}

#[test]
fn failed_readiness_keeps_original_page_backend_and_load_state() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(false));
    stage_replacement(&mut tab, &candidate);
    assert!(!tab.poll_replacement());
    tab.error = Some("Existing error".into());
    *candidate.ready.borrow_mut() = Err("Fake runtime initialization failed".into());

    assert!(tab.poll_replacement());
    assert_eq!(tab.backend, "webkit");
    assert_page(&tab, &original);
    assert_eq!(tab.title, "Original document");
    assert_eq!(tab.url, URL);
    assert!(!tab.loading);
    assert_eq!(tab.error.as_deref(), Some("Existing error"));
    assert_eq!(
        tab.switch_error.as_deref(),
        Some("Fake runtime initialization failed")
    );
    assert!(tab.replacement.is_none());
    assert!(original.visibility.borrow().is_empty());
    assert_eq!(*candidate.visibility.borrow(), [false]);
    assert!(!tab.poll_replacement());
    assert_eq!(candidate.readiness_polls.get(), 2);
}

#[test]
fn invalidated_native_scope_rejects_same_url_and_ignores_late_completion() {
    for close in [false, true] {
        let (mut tab, original) = loaded_tab(1, Engine::WebKit);
        let candidate = FakePage::new(Engine::Cef, Ok(false));
        let scope = original.requests.scope();
        let (sender, receiver) = mpsc::channel();
        let dispatch = original
            .requests
            .prepare(
                Instant::now(),
                Box::new(move |result| {
                    sender.send(result).unwrap();
                }),
            )
            .unwrap();
        let completion = dispatch.completion();
        dispatch.accept();
        stage_replacement(&mut tab, &candidate);
        assert!(!tab.poll_replacement());

        if close {
            original.requests.close();
        } else {
            original.requests.advance_document();
        }
        assert!(!scope.is_current());
        assert_eq!(
            receiver.try_recv().unwrap(),
            Err(if close {
                page::ScriptError::PageClosed
            } else {
                page::ScriptError::DocumentChanged
            })
        );
        completion.complete(Ok("\"late old-document result\"".into()));
        assert!(receiver.try_recv().is_err());
        // Neither URL nor Rc identity changed: only the native ledger scope expired.
        assert_eq!(tab.url, URL);
        assert_page(&tab, &original);
        candidate.become_ready();
        assert_rejected(&mut tab, &original);
    }
}

#[test]
fn changed_url_rejects_ready_replacement_with_current_native_scope() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(true));
    stage_replacement(&mut tab, &candidate);
    tab.url = "https://example.test/another-document".into();
    assert!(
        tab.replacement
            .as_ref()
            .unwrap()
            .scope
            .as_ref()
            .unwrap()
            .is_current()
    );

    assert_rejected(&mut tab, &original);
    assert_eq!(tab.url, "https://example.test/another-document");
    assert_eq!(tab.title, "Original document");
    assert!(!tab.loading);
}

#[test]
fn different_page_pointer_rejects_replacement_even_at_same_url() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(true));
    stage_replacement(&mut tab, &candidate);
    let other = FakePage::new(Engine::WebKit, Ok(true));
    tab.page = Some(other.clone());
    assert!(original.requests.scope().is_current());
    assert!(
        tab.replacement
            .as_ref()
            .unwrap()
            .scope
            .as_ref()
            .unwrap()
            .is_current()
    );
    assert_eq!(tab.url, URL);

    assert_rejected(&mut tab, &other);
    assert!(original.visibility.borrow().is_empty());
}

#[test]
fn removed_original_page_rejects_pending_replacement() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(true));
    stage_replacement(&mut tab, &candidate);
    tab.page = None;

    assert!(tab.poll_replacement());
    assert!(tab.page.is_none());
    assert_eq!(tab.backend, "webkit");
    assert!(tab.switch_error.as_deref().unwrap().contains("Tab changed"));
    assert!(tab.replacement.is_none());
    assert!(original.visibility.borrow().is_empty());
}

#[test]
fn home_state_rejects_pending_replacement_even_if_url_matches() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(true));
    stage_replacement(&mut tab, &candidate);
    tab.home = true;
    assert_eq!(tab.url, URL);

    assert_rejected(&mut tab, &original);
    assert!(tab.home);
    assert_eq!(tab.url(), HOME);
}

#[test]
fn changed_backend_cannot_be_overwritten_by_ready_plan() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(true));
    stage_replacement(&mut tab, &candidate);
    tab.backend = "another-explicit-choice".into();

    assert!(tab.poll_replacement());
    assert_eq!(tab.backend, "another-explicit-choice");
    assert_page(&tab, &original);
    assert!(
        tab.switch_error
            .as_deref()
            .unwrap()
            .contains("backend changed")
    );
    assert!(tab.replacement.is_none());
    assert!(original.visibility.borrow().is_empty());
}

#[test]
fn navigation_cancels_pending_page_and_cannot_later_commit_it() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(false));
    stage_replacement(&mut tab, &candidate);
    let candidate_scope = candidate.requests.scope();
    let candidate_weak = Rc::downgrade(&candidate);
    let drops = candidate.drops.clone();
    drop(candidate);

    tab.start_load("https://example.test/new-navigation".into());
    assert!(tab.replacement.is_none());
    assert!(candidate_weak.upgrade().is_none());
    assert_eq!(drops.get(), 1);
    assert!(!candidate_scope.is_current());
    assert!(!tab.poll_replacement());
    assert_eq!(tab.backend, "webkit");
    assert_page(&tab, &original);
    assert!(tab.loading);
    assert_eq!(tab.url, "https://example.test/new-navigation");
    assert!(original.visibility.borrow().is_empty());
}

#[test]
fn unknown_saved_backend_recovers_only_after_explicit_successful_replacement() {
    let (mut tabs, active) = restore_tabs(vec![saved_tab(URL, Some("future-engine"), false, true)]);
    let tab = &mut tabs[active];
    assert_eq!(tab.backend, "future-engine");
    assert!(tab.page.is_none());
    assert!(Engine::parse(&tab.backend).is_err());
    assert!(!tab.poll_replacement());
    assert_eq!(tab.backend, "future-engine");
    assert!(validate_switch(&tab.backend, Engine::WebKit, URL, false, true, true).is_err());
    assert_eq!(tab.backend, "future-engine");

    let failed = FakePage::new(Engine::WebKit, Err("Unavailable runtime".into()));
    stage_replacement(tab, &failed);
    assert!(tab.poll_replacement());
    assert_eq!(tab.backend, "future-engine");
    assert!(tab.page.is_none());
    assert_eq!(tab.switch_error.as_deref(), Some("Unavailable runtime"));

    let candidate = FakePage::new(Engine::WebKit, Ok(false));
    stage_replacement(tab, &candidate);
    assert!(!tab.poll_replacement());
    assert_eq!(tab.backend, "future-engine");
    assert!(tab.page.is_none());
    candidate.become_ready();
    assert!(tab.poll_replacement());
    assert_eq!(tab.backend, "webkit");
    assert_page(tab, &candidate);
    assert!(tab.switch_error.is_none());
    assert!(tab.loading);
}

#[test]
fn newly_attached_page_rejects_recovery_plan_that_started_without_a_page() {
    let (mut tabs, active) = restore_tabs(vec![saved_tab(URL, Some("future-engine"), false, true)]);
    let tab = &mut tabs[active];
    let candidate = FakePage::new(Engine::WebKit, Ok(true));
    stage_replacement(tab, &candidate);
    assert!(tab.replacement.as_ref().unwrap().original.is_none());
    let other = FakePage::new(Engine::Cef, Ok(true));
    tab.page = Some(other.clone());

    assert!(tab.poll_replacement());
    assert_eq!(tab.backend, "future-engine");
    assert_page(tab, &other);
    assert!(tab.switch_error.as_deref().unwrap().contains("Tab changed"));
    assert!(other.visibility.borrow().is_empty());
}

#[test]
fn restore_tabs_retains_raw_cef_unknown_and_legacy_webkit_without_native_pages() {
    let saved = vec![
        saved_tab(URL, Some("cef"), false, true),
        saved_tab(
            "https://example.test/pinned",
            Some(" Future-Engine\tID "),
            true,
            false,
        ),
        saved_tab("https://example.test/legacy", None, false, false),
        saved_tab("https://example.test/webkit", Some("webkit"), false, false),
        saved_tab(
            "https://example.test/malformed",
            Some("invalid-json:null"),
            false,
            false,
        ),
    ];
    let (tabs, active) = restore_tabs(session::parse(&session::serialize(&saved)));
    assert_eq!(tabs.len(), saved.len());
    assert_eq!(active, 1);
    assert_eq!(tabs[active].url, URL);
    assert_eq!(tabs[active].backend, "cef");
    assert_eq!(tabs[0].backend, " Future-Engine\tID ");
    assert!(tabs[0].pinned.is_some());
    assert!(Engine::parse(&tabs[0].backend).is_err());
    assert_eq!(tabs[2].backend, "webkit");
    assert_eq!(tabs[3].backend, "webkit");
    assert_eq!(tabs[4].backend, "invalid-json:null");
    for tab in tabs {
        assert!(tab.page.is_none());
        assert!(tab.replacement.is_none());
        assert!(!tab.home);
    }
}

#[test]
fn concurrent_same_url_tabs_have_isolated_native_scopes_and_replacements() {
    let (mut first, original_first) = loaded_tab(1, Engine::WebKit);
    let (mut second, original_second) = loaded_tab(2, Engine::Cef);
    let first_scope = original_first.requests.scope();
    let second_scope = original_second.requests.scope();
    assert!(!first_scope.matches(&second_scope));
    let first_candidate = FakePage::new(Engine::Cef, Ok(false));
    let second_candidate = FakePage::new(Engine::WebKit, Ok(false));
    stage_replacement(&mut first, &first_candidate);
    stage_replacement(&mut second, &second_candidate);
    assert!(!first.poll_replacement());
    assert!(!second.poll_replacement());

    original_first.requests.advance_document();
    assert!(!first_scope.is_current());
    assert!(second_scope.is_current());
    second_candidate.become_ready();
    assert!(second.poll_replacement());
    assert_eq!(second.backend, "webkit");
    assert_page(&second, &second_candidate);
    assert!(second.switch_error.is_none());
    assert_eq!(*original_second.visibility.borrow(), [false]);
    assert_eq!(first.backend, "webkit");
    assert_page(&first, &original_first);
    assert!(first.replacement.is_some());

    first_candidate.become_ready();
    assert_rejected(&mut first, &original_first);
    assert_page(&second, &second_candidate);
    assert_eq!(second.backend, "webkit");
    assert!(second.switch_error.is_none());
    assert!(!second.poll_replacement());
}

#[test]
fn native_same_document_url_change_rejects_before_cached_tab_url_is_updated() {
    let (mut tab, original) = loaded_tab(1, Engine::WebKit);
    let candidate = FakePage::new(Engine::Cef, Ok(true));
    stage_replacement(&mut tab, &candidate);
    *original.document.borrow_mut() = Some((
        format!("{URL}#intervening-navigation"),
        "Updated document".into(),
    ));
    assert_eq!(tab.url, URL);
    assert!(original.requests.scope().is_current());
    assert_rejected(&mut tab, &original);
}

#[test]
fn pin_unload_and_session_restore_preserve_explicit_engine_and_unknown_ids() {
    for backend in ["cef", "future-engine", "webkit"] {
        let (mut tab, original) = loaded_tab(1, Engine::Cef);
        tab.backend = backend.into();
        tab.pinned = Some(URL.into());
        tab.reset_pin(URL.into());
        assert_eq!(tab.backend, backend);
        assert_eq!(tab.pinned.as_deref(), Some(URL));
        assert!(tab.page.is_none());
        assert!(tab.replacement.is_none());
        assert_eq!(*original.visibility.borrow(), [false]);
        let saved = saved_tab(&tab.url, Some(&tab.backend), true, true);
        let (tabs, active) = restore_tabs(session::parse(&session::serialize(&[saved])));
        assert_eq!(tabs[active].backend, backend);
    }
}

#[test]
fn unsupported_audio_does_not_claim_to_mute_and_switch_clears_unapplied_settings() {
    let (mut tab, original) = loaded_tab(1, Engine::Cef);
    let muted = TabAudio {
        muted: true,
        ..TabAudio::default()
    };
    assert!(tab.change_audio(muted).is_err());
    assert!(tab.audio.is_default());
    assert_page(&tab, &original);

    tab.backend = "webkit".into();
    tab.audio = muted;
    let candidate = FakePage::new(Engine::Cef, Ok(true));
    stage_replacement(&mut tab, &candidate);
    assert!(tab.poll_replacement());
    assert_eq!(tab.backend, "cef");
    assert!(tab.audio.is_default());
}

#[test]
fn logical_close_hides_live_and_pending_views_without_freeing_native_owners() {
    let (mut tab, original) = loaded_tab(1, Engine::Cef);
    let candidate = FakePage::new(Engine::WebKit, Ok(false));
    stage_replacement(&mut tab, &candidate);
    tab.hide_for_close();
    assert_eq!(*original.visibility.borrow(), [false]);
    assert_eq!(*candidate.visibility.borrow(), [false, false]);
    assert_page(&tab, &original);
    assert!(original.requests.scope().is_current());
    assert!(tab.replacement.is_some());
}

#[test]
fn dropping_tab_releases_only_its_original_and_pending_page() {
    let (mut first, original_first) = loaded_tab(1, Engine::WebKit);
    let (mut second, original_second) = loaded_tab(2, Engine::Cef);
    let first_candidate = FakePage::new(Engine::Cef, Ok(false));
    let second_candidate = FakePage::new(Engine::WebKit, Ok(false));
    stage_replacement(&mut first, &first_candidate);
    stage_replacement(&mut second, &second_candidate);
    let old_scope = original_first.requests.scope();
    let pending_scope = first_candidate.requests.scope();
    let other_scope = original_second.requests.scope();
    let old_weak = Rc::downgrade(&original_first);
    let pending_weak = Rc::downgrade(&first_candidate);
    let old_drops = original_first.drops.clone();
    let pending_drops = first_candidate.drops.clone();
    drop(original_first);
    drop(first_candidate);
    assert!(old_scope.is_current());
    assert!(pending_scope.is_current());

    drop(first);
    assert!(old_weak.upgrade().is_none());
    assert!(pending_weak.upgrade().is_none());
    assert_eq!(old_drops.get(), 1);
    assert_eq!(pending_drops.get(), 1);
    assert!(!old_scope.is_current());
    assert!(!pending_scope.is_current());
    assert!(other_scope.is_current());
    assert_page(&second, &original_second);
    assert!(!second.poll_replacement());
    assert!(second.replacement.is_some());
    assert!(second.switch_error.is_none());
    second_candidate.become_ready();
    assert!(second.poll_replacement());
    assert_page(&second, &second_candidate);
    assert_eq!(second.backend, "webkit");
    assert_eq!(*original_second.visibility.borrow(), [false]);
}
