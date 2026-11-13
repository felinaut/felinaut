//! Browser-process transport for the native-only renderer evaluation protocol.
//!
//! The parent advances the request ledger before `invalidate`, closes it before
//! `close`, and calls `reconcile` at load end/context hints/UI drains. Native frame
//! notifications must invalidate on detach/replacement (including BFCache), and
//! renderer failure must invalidate the ledger as well. No GPUI calls occur here.
//! CEF send has no delivery acknowledgement; acceptance means local dispatch.
//! Deadlines cancel delivery, not JavaScript execution or its side effects.

use std::{
    cell::RefCell,
    collections::{BTreeMap, btree_map::Entry},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use cef::{
    Browser, CefString, CefStringUserfree, Frame, ImplBrowser, ImplFrame, ImplListValue,
    ImplProcessMessage, ProcessId, ProcessMessage, ThreadId, ValueType,
};

use crate::{
    evaluation_protocol::{self as protocol, Field, FieldSpec, Id, Message, Status},
    page::{RequestScope, ScriptCallback, ScriptCompletion, ScriptError, ScriptRequests},
};

const PENDING_LIMIT: usize = 64;
const TIMEOUT: Duration = Duration::from_secs(10);

// Correlation IDs must not restart when an Evaluation is replaced around an
// existing native browser/context: old IPC could otherwise match the new page.
static WIRE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn next_wire_id() -> Option<Id> {
    allocate_wire_id(&WIRE_SEQUENCE)
}

fn allocate_wire_id(sequence: &AtomicU64) -> Option<Id> {
    sequence
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .ok()
        .and_then(|previous| previous.checked_add(1))
        .and_then(Id::new)
}

#[derive(Clone)]
pub struct Evaluation {
    state: Rc<RefCell<State>>,
    requests: ScriptRequests,
}

#[derive(Default)]
struct State {
    browser_id: Option<i32>,
    closed: bool,

    binding: Option<Binding>,
    pending: BTreeMap<Id, Pending>,
}

struct Binding {
    id: Id,
    frame: String,
    scope: RequestScope,
    phase: Phase,
}

#[derive(Clone, Copy)]
enum Phase {
    Waiting { deadline: Instant },
    Ready { context: Id },
}

struct Pending {
    binding: Id,
    context: Id,
    frame: String,
    scope: RequestScope,
    deadline: Instant,
    completion: ScriptCompletion,
}

type Cancellations = Vec<(Pending, ScriptError)>;

impl Evaluation {
    pub fn new(requests: ScriptRequests) -> Self {
        Self {
            state: Rc::new(RefCell::new(State::default())),
            requests,
        }
    }

    /// Records the expected native ID, not a retained browser reference. An
    /// Evaluation cannot be reattached to another browser or reopened after close.
    pub fn attach(&self, browser: &Browser) -> Result<(), String> {
        ui_thread()?;
        if browser.is_valid() == 0 || !self.requests.scope().is_current() {
            return Err("CEF evaluation requires a live page".into());
        }
        let id = browser.identifier();
        let mut state = self.state.borrow_mut();
        if state.closed || state.browser_id.is_some_and(|expected| expected != id) {
            return Err("CEF evaluation cannot attach to this browser".into());
        }
        state.browser_id = Some(id);
        Ok(())
    }

    /// At most one bind is outstanding. Lost bind replies are retired after ten
    /// seconds; later reconciliation uses a fresh, never-reused binding ID.
    pub fn reconcile(&self, browser: &Browser) {
        let _ = self.reconcile_inner(browser);
    }

    fn reconcile_inner(&self, browser: &Browser) -> Result<(), String> {
        ui_thread()?;
        let expected = self.expected_browser()?;
        let (frame, frame_id) = current_frame(browser, expected)?;
        let now = Instant::now();
        let cancelled = self.state.borrow_mut().prune(now, Some(&frame_id));
        deliver_cancelled(cancelled);
        let scope = self.requests.scope();
        let plan = self
            .state
            .borrow_mut()
            .bind(&frame_id, scope, now)
            .map_err(|error| error.to_string())?;
        let Some((binding, message)) = plan else {
            return Ok(());
        };
        let mut message = match encode(message) {
            Ok(message) => message,
            Err(error) => {
                self.state.borrow_mut().reject_bind(binding);
                return Err(error);
            }
        };
        // Native construction/queries and sends happen outside the RefCell borrow.
        if authenticate(browser, &frame, expected).as_deref() != Ok(frame_id.as_str())
            || !self.state.borrow().waiting(binding, &frame_id)
        {
            self.state.borrow_mut().reject_bind(binding);
            return Err("CEF evaluation document changed before binding".into());
        }
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        Ok(())
    }

    /// Called only after the parent has advanced the ledger. Also defensively
    /// completes any remaining tokens; completion and token destruction are never
    /// performed with the transport state borrowed.
    pub fn invalidate(&self) {
        let pending = self.state.borrow_mut().invalidate();
        cancel_all(pending, ScriptError::DocumentChanged);
    }

    /// The parent closes the ledger first. This is terminal and idempotent.
    pub fn close(&self) {
        let pending = {
            let mut state = self.state.borrow_mut();
            state.closed = true;
            state.browser_id = None;
            state.invalidate()
        };
        cancel_all(pending, ScriptError::PageClosed);
    }

    pub fn expire(&self, now: Instant) {
        let cancelled = self.state.borrow_mut().prune(now, None);
        deliver_cancelled(cancelled);
        self.requests.expire(now);
    }

    pub fn evaluate(
        &self,
        browser: &Browser,
        script: &str,
        callback: ScriptCallback,
    ) -> Result<(), String> {
        ui_thread()?;
        if script.len() > protocol::SOURCE_LIMIT {
            return Err("CEF evaluation source exceeds the byte limit".into());
        }
        let now = Instant::now();
        self.expire(now);
        let expected = self.expected_browser()?;
        let (frame, frame_id) = current_frame(browser, expected)?;
        let cancelled = self.state.borrow_mut().prune(now, Some(&frame_id));
        deliver_cancelled(cancelled);
        let scope = self.requests.scope();
        let (binding, context, request) = {
            let state = self.state.borrow();
            let (binding, context) = state
                .ready(&frame_id, &scope)
                .ok_or("CEF evaluation context is not bound")?;
            if state.pending.len() >= PENDING_LIMIT {
                return Err(ScriptError::TooManyRequests.to_string());
            }
            let request =
                next_wire_id().ok_or_else(|| ScriptError::SequenceExhausted.to_string())?;
            (binding, context, request)
        };
        let deadline = now
            .checked_add(TIMEOUT)
            .ok_or_else(|| ScriptError::Capacity.to_string())?;
        let mut message = encode(Message::Run {
            binding,
            context,
            request,
            source: script.into(),
        })?;
        let dispatch = self
            .requests
            .prepare(now, callback)
            .map_err(|error| error.to_string())?;
        let pending = Pending {
            binding,
            context,
            frame: frame_id.clone(),
            scope,
            deadline,
            completion: dispatch.completion(),
        };
        let reserved = self.state.borrow_mut().reserve(request, pending);
        if let Err(pending) = reserved {
            dispatch.reject();
            drop(pending);
            return Err("CEF evaluation document changed or capacity was exhausted".into());
        }
        if authenticate(browser, &frame, expected).as_deref() != Ok(frame_id.as_str())
            || !self
                .state
                .borrow()
                .has_pending(request, binding, context, &frame_id)
        {
            // Reject before dropping the completion token: failed dispatch must
            // not invoke a callback, even if lifecycle cancellation raced it.
            dispatch.reject();
            let pending = self.state.borrow_mut().pending.remove(&request);
            drop(pending);
            return Err("CEF evaluation document changed before dispatch".into());
        }
        frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
        dispatch.accept();
        Ok(())
    }

    /// Only authenticated renderer notifications can mutate the transport. This
    /// callback never logs source/result data or calls GPUI. Script callbacks must
    /// enqueue their own UI work, as required by ScriptRequests.
    pub fn receive(
        &self,
        browser: Option<&mut Browser>,
        frame: Option<&mut Frame>,
        source: ProcessId,
        message: Option<&mut ProcessMessage>,
    ) -> i32 {
        if ui_thread().is_err() {
            return 0;
        }
        match catch_unwind(AssertUnwindSafe(|| {
            self.receive_inner(browser, frame, source, message)
        })) {
            Ok(handled) => handled,
            Err(payload) => {
                discard_panic(payload);
                let pending = self.state.borrow_mut().invalidate();
                cancel_all(
                    pending,
                    ScriptError::Backend("CEF evaluation callback failed".into()),
                );
                1
            }
        }
    }

    fn receive_inner(
        &self,
        browser: Option<&mut Browser>,
        frame: Option<&mut Frame>,
        source: ProcessId,
        message: Option<&mut ProcessMessage>,
    ) -> i32 {
        let Some(message) = message else { return 0 };
        if message.is_valid() == 0 {
            return 0;
        }
        let Ok(name) = read_text(message.name(), protocol::NAME_LIMIT) else {
            return 0;
        };
        if protocol::schema(&name).is_none() {
            return 0;
        }
        if source != ProcessId::RENDERER
            || !matches!(
                name.as_str(),
                protocol::HINT | protocol::BOUND | protocol::DONE | protocol::RELEASED
            )
        {
            return 1;
        }
        let (Some(browser), Some(frame)) = (browser, frame) else {
            return 1;
        };
        let Ok(expected) = self.expected_browser() else {
            return 1;
        };
        let Ok(frame_id) = authenticate(browser, frame, expected) else {
            return 1;
        };
        let Ok(message) = decode(&name, message) else {
            return 1;
        };
        let now = Instant::now();
        let cancelled = self.state.borrow_mut().prune(now, Some(&frame_id));
        deliver_cancelled(cancelled);
        match message {
            Message::ContextHint => self.reconcile(browser),
            Message::Bound {
                binding,
                context,
                status,
            } => {
                self.state
                    .borrow_mut()
                    .bound(&frame_id, binding, context, status, now);
            }
            Message::Done {
                binding,
                context,
                request,
                status,
                payload,
            } => {
                let pending = self
                    .state
                    .borrow_mut()
                    .take_done(&frame_id, binding, context, request);
                if let Some(pending) = pending {
                    let cancelled = if status == Status::StaleContext {
                        self.state.borrow_mut().release(&frame_id, binding, context)
                    } else {
                        Vec::new()
                    };
                    let result = if pending.scope.is_current() {
                        result(status, payload)
                    } else {
                        Err(ScriptError::DocumentChanged)
                    };
                    pending.completion.complete(result);
                    cancel_all(cancelled, ScriptError::DocumentChanged);
                }
            }
            Message::Released { binding, context } => {
                let pending = self.state.borrow_mut().release(&frame_id, binding, context);
                cancel_all(pending, ScriptError::DocumentChanged);
            }
            _ => {}
        }
        1
    }

    fn expected_browser(&self) -> Result<i32, String> {
        let state = self.state.borrow();
        if state.closed {
            return Err("CEF evaluation is closed".into());
        }
        state
            .browser_id
            .ok_or_else(|| "CEF evaluation is not attached".into())
    }
}

impl State {
    fn invalidate(&mut self) -> Vec<Pending> {
        self.binding = None;
        std::mem::take(&mut self.pending).into_values().collect()
    }

    fn prune(&mut self, now: Instant, frame: Option<&str>) -> Cancellations {
        let stale = self.binding.as_ref().is_some_and(|binding| {
            !binding.scope.is_current() || frame.is_some_and(|frame| frame != binding.frame)
        });
        if stale {
            return self
                .invalidate()
                .into_iter()
                .map(|pending| (pending, ScriptError::DocumentChanged))
                .collect();
        }
        if self.binding.as_ref().is_some_and(
            |binding| matches!(binding.phase, Phase::Waiting { deadline } if deadline <= now),
        ) {
            self.binding = None;
        }
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(id, pending)| {
                if !pending.scope.is_current() {
                    Some((*id, ScriptError::DocumentChanged))
                } else if pending.deadline <= now {
                    Some((*id, ScriptError::TimedOut))
                } else {
                    None
                }
            })
            .collect();
        expired
            .into_iter()
            .filter_map(|(id, error)| self.pending.remove(&id).map(|pending| (pending, error)))
            .collect()
    }

    fn bind(
        &mut self,
        frame: &str,
        scope: RequestScope,
        now: Instant,
    ) -> Result<Option<(Id, Message)>, ScriptError> {
        if self.closed || self.browser_id.is_none() || !scope.is_current() {
            return Err(ScriptError::PageClosed);
        }
        if self.binding.is_some() {
            return Ok(None);
        }
        let deadline = now.checked_add(TIMEOUT).ok_or(ScriptError::Capacity)?;
        let id = next_wire_id().ok_or(ScriptError::SequenceExhausted)?;
        self.binding = Some(Binding {
            id,
            frame: frame.into(),
            scope,
            phase: Phase::Waiting { deadline },
        });
        Ok(Some((id, Message::Bind { binding: id })))
    }

    fn waiting(&self, id: Id, frame: &str) -> bool {
        !self.closed
            && self.binding.as_ref().is_some_and(|binding| {
                binding.id == id
                    && binding.frame == frame
                    && binding.scope.is_current()
                    && matches!(binding.phase, Phase::Waiting { .. })
            })
    }

    fn reject_bind(&mut self, id: Id) {
        if self.binding.as_ref().is_some_and(|binding| {
            binding.id == id && matches!(binding.phase, Phase::Waiting { .. })
        }) {
            self.binding = None;
        }
    }

    fn bound(&mut self, frame: &str, id: Id, context: Option<Id>, status: Status, now: Instant) {
        if !self.waiting(id, frame) {
            return;
        }
        let Some(binding) = self.binding.as_mut() else {
            return;
        };
        if !matches!(binding.phase, Phase::Waiting { deadline } if deadline > now) {
            return;
        }
        if let (Status::Ok, Some(context)) = (status, context) {
            binding.phase = Phase::Ready { context };
        } else {
            self.binding = None;
        }
    }

    fn ready(&self, frame: &str, scope: &RequestScope) -> Option<(Id, Id)> {
        if self.closed {
            return None;
        }
        let binding = self.binding.as_ref()?;
        if binding.frame != frame || !binding.scope.matches(scope) {
            return None;
        }
        match binding.phase {
            Phase::Ready { context } => Some((binding.id, context)),
            Phase::Waiting { .. } => None,
        }
    }

    fn reserve(&mut self, id: Id, pending: Pending) -> Result<(), Pending> {
        if self.pending.len() >= PENDING_LIMIT
            || self.ready(&pending.frame, &pending.scope)
                != Some((pending.binding, pending.context))
        {
            return Err(pending);
        }
        match self.pending.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(pending);
                Ok(())
            }
            Entry::Occupied(_) => Err(pending),
        }
    }

    fn has_pending(&self, request: Id, binding: Id, context: Id, frame: &str) -> bool {
        self.pending.get(&request).is_some_and(|pending| {
            pending.binding == binding
                && pending.context == context
                && pending.frame == frame
                && self.ready(frame, &pending.scope) == Some((binding, context))
        })
    }

    fn take_done(&mut self, frame: &str, binding: Id, context: Id, request: Id) -> Option<Pending> {
        if !self.has_pending(request, binding, context, frame) {
            return None;
        }
        self.pending.remove(&request)
    }

    fn release(&mut self, frame: &str, id: Id, context: Id) -> Vec<Pending> {
        let matches = self.binding.as_ref().is_some_and(|binding| {
            binding.frame == frame
                && binding.id == id
                && binding.scope.is_current()
                && matches!(binding.phase, Phase::Ready { context: current } if current == context)
        });
        if matches {
            self.invalidate()
        } else {
            Vec::new()
        }
    }
}

fn ui_thread() -> Result<(), String> {
    if cef::currently_on(ThreadId::UI) == 0 {
        Err("CEF evaluation requires its UI thread".into())
    } else {
        Ok(())
    }
}

fn current_frame(browser: &Browser, expected: i32) -> Result<(Frame, String), String> {
    if browser.is_valid() == 0 || browser.identifier() != expected {
        return Err("CEF evaluation browser does not match".into());
    }
    let frame = browser
        .main_frame()
        .ok_or("CEF evaluation main frame is unavailable")?;
    let identity = authenticate(browser, &frame, expected)?;
    Ok((frame, identity))
}

fn authenticate(browser: &Browser, frame: &Frame, expected: i32) -> Result<String, String> {
    if browser.is_valid() == 0
        || browser.identifier() != expected
        || frame.is_valid() == 0
        || frame.is_main() == 0
    {
        return Err("CEF evaluation native identity does not match".into());
    }
    let mut owner = frame
        .browser()
        .ok_or("CEF evaluation frame has no browser")?;
    if owner.is_valid() == 0
        || owner.identifier() != expected
        || browser.is_same(Some(&mut owner)) == 0
    {
        return Err("CEF evaluation frame belongs to another browser".into());
    }
    let main = browser
        .main_frame()
        .ok_or("CEF evaluation main frame is unavailable")?;
    if main.is_valid() == 0 || main.is_main() == 0 {
        return Err("CEF evaluation main frame is unavailable".into());
    }
    let id = read_text(frame.identifier(), protocol::FRAME_LIMIT)
        .map_err(|_| "CEF evaluation frame identity is invalid")?;
    let current = read_text(main.identifier(), protocol::FRAME_LIMIT)
        .map_err(|_| "CEF evaluation frame identity is invalid")?;
    if id.is_empty() || id != current {
        return Err("CEF evaluation frame is not current".into());
    }
    Ok(id)
}

fn result(status: Status, payload: String) -> Result<String, ScriptError> {
    match status {
        Status::Ok => {
            if payload.len() > protocol::RESULT_LIMIT {
                return Err(ScriptError::ResultTooLarge);
            }
            // Keep serde_json's recursion limit; the bounded wire string is not
            // permission for arbitrary recursion or a malformed successful value.
            serde_json::from_str::<serde_json::Value>(&payload)
                .map_err(|_| ScriptError::Backend("CEF returned invalid JSON".into()))?;
            Ok(payload)
        }
        Status::StaleContext => Err(ScriptError::DocumentChanged),
        Status::ResultTooLarge => Err(ScriptError::ResultTooLarge),
        _ => Err(ScriptError::Backend(status.diagnostic().into())),
    }
}

fn cancel_all(pending: Vec<Pending>, error: ScriptError) {
    for pending in pending {
        pending.completion.complete(Err(error.clone()));
    }
}
fn deliver_cancelled(cancelled: Cancellations) {
    for (pending, error) in cancelled {
        pending.completion.complete(Err(error));
    }
}

fn read_text(value: CefStringUserfree, limit: usize) -> Result<String, protocol::ProtocolError> {
    let raw: Option<&cef::sys::_cef_string_utf16_t> = (&value).into();
    let Some(raw) = raw else {
        return Ok(String::new());
    };
    if raw.length > limit {
        return Err(protocol::ProtocolError::TooLarge);
    }
    if raw.length == 0 {
        return Ok(String::new());
    }
    if raw.str_.is_null() {
        return Err(protocol::ProtocolError::InvalidPayload);
    }
    // CEF's userfree getter can allocate internally, but the Rust-side copy is
    // bounded before conversion and then checked against an exact UTF-8 budget.
    let copy = CefString::from(&value);
    protocol::bounded_utf16(
        copy.as_slice()
            .ok_or(protocol::ProtocolError::InvalidPayload)?,
        limit,
    )
}

fn decode(name: &str, message: &ProcessMessage) -> Result<Message, protocol::ProtocolError> {
    let specs = protocol::schema(name).ok_or(protocol::ProtocolError::UnknownMessage)?;
    let args = message
        .argument_list()
        .ok_or(protocol::ProtocolError::InvalidShape)?;
    if args.is_valid() == 0 || args.size() != specs.len() {
        return Err(protocol::ProtocolError::InvalidShape);
    }
    let mut fields = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        fields.push(match spec {
            FieldSpec::String(limit) if args.get_type(index) == ValueType::STRING => {
                Field::String(read_text(args.string(index), *limit)?)
            }
            FieldSpec::Int if args.get_type(index) == ValueType::INT => Field::Int(args.int(index)),
            _ => return Err(protocol::ProtocolError::InvalidShape),
        });
    }
    Message::decode(name, fields)
}

fn encode(message: Message) -> Result<ProcessMessage, String> {
    let (name, fields) = message
        .into_wire()
        .map_err(|_| "CEF evaluation outgoing schema is invalid")?;
    let message = cef::process_message_create(Some(&CefString::from(name)))
        .ok_or("CEF evaluation message allocation failed")?;
    let args = message
        .argument_list()
        .ok_or("CEF evaluation argument allocation failed")?;
    if args.set_size(fields.len()) == 0 {
        return Err("CEF evaluation argument allocation failed".into());
    }
    for (index, field) in fields.into_iter().enumerate() {
        let success = match field {
            Field::String(value) => args.set_string(index, Some(&CefString::from(value.as_str()))),
            Field::Int(value) => args.set_int(index, value),
        };
        if success == 0 {
            return Err("CEF evaluation argument construction failed".into());
        }
    }
    Ok(message)
}

fn discard_panic(payload: Box<dyn std::any::Any + Send>) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
        std::mem::forget(payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, channel};

    fn id(value: u64) -> Id {
        Id::new(value).unwrap()
    }
    fn callback() -> (ScriptCallback, Receiver<Result<String, ScriptError>>) {
        let (sender, receiver) = channel();
        (Box::new(move |value| sender.send(value).unwrap()), receiver)
    }
    fn bound(requests: &ScriptRequests, now: Instant) -> State {
        let mut state = State {
            browser_id: Some(17),
            ..State::default()
        };
        let (binding, _) = state.bind("main", requests.scope(), now).unwrap().unwrap();
        state.bound("main", binding, Some(id(100)), Status::Ok, now);
        state
    }
    struct Reservation(Box<dyn FnOnce(bool)>);
    impl Reservation {
        fn accept(self) {
            (self.0)(true);
        }
        fn reject(self) {
            (self.0)(false);
        }
    }
    fn pending(
        requests: &ScriptRequests,
        state: &State,
        now: Instant,
        callback: ScriptCallback,
    ) -> (Reservation, Pending) {
        let dispatch = requests.prepare(now, callback).unwrap();
        let (binding, context) = state.ready("main", &requests.scope()).unwrap();
        let pending = Pending {
            binding,
            context,
            frame: "main".into(),
            scope: requests.scope(),
            deadline: now + TIMEOUT,
            completion: dispatch.completion(),
        };
        (
            Reservation(Box::new(move |accept| {
                if accept {
                    dispatch.accept();
                } else {
                    dispatch.reject();
                }
            })),
            pending,
        )
    }

    #[test]
    fn wire_ids_survive_page_replacement_and_fail_closed_at_exhaustion() {
        let now = Instant::now();
        let first_requests = ScriptRequests::default();
        let mut first = bound(&first_requests, now);
        let first_id = first.binding.as_ref().unwrap().id;
        first.invalidate();
        let replacement_requests = ScriptRequests::default();
        let mut replacement = State {
            browser_id: Some(17),
            ..State::default()
        };
        let (replacement_id, _) = replacement
            .bind("main", replacement_requests.scope(), now)
            .unwrap()
            .unwrap();
        assert!(replacement_id > first_id);
        replacement.bound("main", first_id, Some(id(100)), Status::Ok, now);
        assert!(
            replacement
                .ready("main", &replacement_requests.scope())
                .is_none()
        );
        let sequence = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_wire_id(&sequence), Some(id(u64::MAX)));
        assert_eq!(allocate_wire_id(&sequence), None);
        assert_eq!(allocate_wire_id(&sequence), None);
    }

    #[test]
    fn bind_is_single_flight_and_only_matching_reply_installs_context() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = State {
            browser_id: Some(17),
            ..State::default()
        };
        let (binding, message) = state.bind("main", requests.scope(), now).unwrap().unwrap();
        assert_eq!(message, Message::Bind { binding });
        assert!(state.bind("main", requests.scope(), now).unwrap().is_none());
        state.bound("other", binding, Some(id(100)), Status::Ok, now);
        state.bound("main", id(u64::MAX), Some(id(100)), Status::Ok, now);
        assert!(state.ready("main", &requests.scope()).is_none());
        state.bound("main", binding, Some(id(100)), Status::Ok, now);
        assert_eq!(
            state.ready("main", &requests.scope()),
            Some((binding, id(100)))
        );
        state.bound("main", binding, None, Status::NoContext, now);
        assert_eq!(
            state.ready("main", &requests.scope()),
            Some((binding, id(100)))
        );
    }

    #[test]
    fn bind_failure_timeout_and_rejection_allow_fresh_id_not_old_ack() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = State {
            browser_id: Some(17),
            ..State::default()
        };
        let (first, _) = state.bind("main", requests.scope(), now).unwrap().unwrap();
        state.bound("main", first, None, Status::NoContext, now);
        let (second, _) = state.bind("main", requests.scope(), now).unwrap().unwrap();
        assert!(second > first);
        state.prune(now + TIMEOUT, None);
        state.bound("main", second, Some(id(100)), Status::Ok, now + TIMEOUT);
        assert!(state.binding.is_none());
        let (third, _) = state
            .bind("main", requests.scope(), now + TIMEOUT)
            .unwrap()
            .unwrap();
        state.bound("main", first, Some(id(100)), Status::Ok, now + TIMEOUT);
        assert!(state.ready("main", &requests.scope()).is_none());
        state.reject_bind(second);
        assert!(state.waiting(third, "main"));
        state.reject_bind(third);
        assert!(state.binding.is_none());
    }

    #[test]
    fn two_phase_result_is_buffered_and_duplicates_cannot_complete_twice() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = bound(&requests, now);
        let (callback, receiver) = callback();
        let (dispatch, pending) = pending(&requests, &state, now, callback);
        let binding = pending.binding;
        assert!(state.reserve(id(5), pending).is_ok());
        let pending = state.take_done("main", binding, id(100), id(5)).unwrap();
        pending.completion.complete(result(Status::Ok, "42".into()));
        assert!(receiver.try_recv().is_err());
        dispatch.accept();
        assert_eq!(receiver.recv().unwrap(), Ok("42".into()));
        assert!(state.take_done("main", binding, id(100), id(5)).is_none());
        drop(pending);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn failed_dispatch_is_silent_even_after_provisional_cancellation() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = bound(&requests, now);
        let (callback, receiver) = callback();
        let (dispatch, pending) = pending(&requests, &state, now, callback);
        assert!(state.reserve(id(5), pending).is_ok());
        cancel_all(state.invalidate(), ScriptError::DocumentChanged);
        dispatch.reject();
        assert!(receiver.recv().is_err());
    }

    #[test]
    fn wrong_frame_binding_context_and_request_do_not_consume_pending() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = bound(&requests, now);
        let (callback, receiver) = callback();
        let (dispatch, pending) = pending(&requests, &state, now, callback);
        let binding = pending.binding;
        assert!(state.reserve(id(5), pending).is_ok());
        dispatch.accept();
        for (frame, b, c, r) in [
            ("other", binding, id(100), id(5)),
            ("main", id(99), id(100), id(5)),
            ("main", binding, id(99), id(5)),
            ("main", binding, id(100), id(99)),
        ] {
            assert!(state.take_done(frame, b, c, r).is_none());
        }
        assert_eq!(state.pending.len(), 1);
        assert!(receiver.try_recv().is_err());
        cancel_all(state.invalidate(), ScriptError::DocumentChanged);
        assert_eq!(receiver.recv().unwrap(), Err(ScriptError::DocumentChanged));
    }

    #[test]
    fn only_matching_release_cancels_and_old_release_preserves_replacement() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = bound(&requests, now);
        let (callback, receiver) = callback();
        let (dispatch, pending) = pending(&requests, &state, now, callback);
        let binding = pending.binding;
        assert!(state.reserve(id(5), pending).is_ok());
        dispatch.accept();
        assert!(state.release("other", binding, id(100)).is_empty());
        assert!(state.release("main", binding, id(99)).is_empty());
        cancel_all(
            state.release("main", binding, id(100)),
            ScriptError::DocumentChanged,
        );
        assert_eq!(receiver.recv().unwrap(), Err(ScriptError::DocumentChanged));
        let (replacement, _) = state.bind("main", requests.scope(), now).unwrap().unwrap();
        state.bound("main", replacement, Some(id(101)), Status::Ok, now);
        assert!(state.release("main", binding, id(100)).is_empty());
        assert_eq!(
            state.ready("main", &requests.scope()),
            Some((replacement, id(101)))
        );
    }

    #[test]
    fn document_scope_and_native_frame_changes_retire_old_requests() {
        for advance in [false, true] {
            let requests = ScriptRequests::default();
            let now = Instant::now();
            let mut state = bound(&requests, now);
            let (callback, receiver) = callback();
            let (dispatch, pending) = pending(&requests, &state, now, callback);
            assert!(state.reserve(id(5), pending).is_ok());
            dispatch.accept();
            if advance {
                requests.advance_document();
            }
            deliver_cancelled(state.prune(now, Some(if advance { "main" } else { "replacement" })));
            assert_eq!(receiver.recv().unwrap(), Err(ScriptError::DocumentChanged));
            assert!(state.binding.is_none());
            assert!(state.pending.is_empty());
        }
    }

    #[test]
    fn expiry_is_inclusive_reclaims_wire_map_and_preserves_newer_requests() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = bound(&requests, now);
        let (callback1, receiver1) = callback();
        let (dispatch, pending1) = pending(&requests, &state, now, callback1);
        assert!(state.reserve(id(5), pending1).is_ok());
        dispatch.accept();
        let (callback2, receiver2) = callback();
        let (dispatch, pending2) =
            pending(&requests, &state, now + Duration::from_secs(1), callback2);
        assert!(state.reserve(id(6), pending2).is_ok());
        dispatch.accept();
        assert!(
            state
                .prune(now + TIMEOUT - Duration::from_nanos(1), None)
                .is_empty()
        );
        deliver_cancelled(state.prune(now + TIMEOUT, None));
        assert_eq!(receiver1.recv().unwrap(), Err(ScriptError::TimedOut));
        assert!(receiver2.try_recv().is_err());
        assert_eq!(state.pending.len(), 1);
        cancel_all(state.invalidate(), ScriptError::DocumentChanged);
    }

    #[test]
    fn capacity_is_bounded_and_rejection_does_not_replace_existing_token() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = bound(&requests, now);
        let mut receivers = Vec::new();
        for n in 0..PENDING_LIMIT {
            let (callback, receiver) = callback();
            let (dispatch, pending) = pending(&requests, &state, now, callback);
            assert!(state.reserve(id(n as u64 + 5), pending).is_ok());
            dispatch.accept();
            receivers.push(receiver);
        }
        assert_eq!(state.pending.len(), PENDING_LIMIT);
        let extra_requests = ScriptRequests::default();
        let (extra_callback, extra_receiver) = callback();
        let extra_dispatch = extra_requests.prepare(now, extra_callback).unwrap();
        let (binding, context) = state.ready("main", &requests.scope()).unwrap();
        let extra = Pending {
            binding,
            context,
            frame: "main".into(),
            scope: requests.scope(),
            deadline: now + TIMEOUT,
            completion: extra_dispatch.completion(),
        };
        let rejected = state.reserve(id(999), extra).err().unwrap();
        extra_dispatch.reject();
        drop(rejected);
        assert!(extra_receiver.recv().is_err());
        assert_eq!(state.pending.len(), PENDING_LIMIT);
        deliver_cancelled(state.prune(now + TIMEOUT, None));
        for receiver in receivers {
            assert_eq!(receiver.recv().unwrap(), Err(ScriptError::TimedOut));
        }
        assert!(state.pending.is_empty());
    }

    #[test]
    fn duplicate_wire_id_does_not_drop_or_replace_original_completion() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut state = bound(&requests, now);
        let (callback1, receiver1) = callback();
        let (dispatch1, pending1) = pending(&requests, &state, now, callback1);
        let binding = pending1.binding;
        assert!(state.reserve(id(5), pending1).is_ok());
        dispatch1.accept();
        let (callback2, receiver2) = callback();
        let (dispatch2, pending2) = pending(&requests, &state, now, callback2);
        let rejected = state.reserve(id(5), pending2).err().unwrap();
        dispatch2.reject();
        drop(rejected);
        assert!(receiver2.recv().is_err());
        assert!(receiver1.try_recv().is_err());
        let original = state.take_done("main", binding, id(100), id(5)).unwrap();
        original.completion.complete(Ok("null".into()));
        assert_eq!(receiver1.recv().unwrap(), Ok("null".into()));
    }

    #[test]
    fn cancellation_callbacks_can_reenter_transport_without_borrow_panics() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let evaluation = Evaluation::new(requests.clone());
        *evaluation.state.borrow_mut() = bound(&requests, now);
        let (sender, receiver) = channel();
        let state = Rc::downgrade(&evaluation.state);
        // ScriptCallback is Send; hand off only a native-free notification here.
        // The actual reentrant callback below uses a thread-local transport slot.
        thread_local! { static REENTER: RefCell<Option<Evaluation>> = const { RefCell::new(None) }; }
        REENTER.with(|slot| *slot.borrow_mut() = Some(evaluation.clone()));
        let callback: ScriptCallback = Box::new(move |value| {
            REENTER.with(|slot| {
                if let Some(evaluation) = slot.borrow().as_ref() {
                    evaluation.expire(Instant::now());
                }
            });
            sender.send(value).unwrap();
        });
        let (dispatch, pending) = pending(&requests, &evaluation.state.borrow(), now, callback);
        assert!(
            evaluation
                .state
                .borrow_mut()
                .reserve(id(5), pending)
                .is_ok()
        );
        dispatch.accept();
        evaluation.invalidate();
        assert_eq!(receiver.recv().unwrap(), Err(ScriptError::DocumentChanged));
        assert!(state.upgrade().unwrap().borrow().pending.is_empty());
        REENTER.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn invalidate_does_not_advance_ledger_and_close_is_terminal() {
        let requests = ScriptRequests::default();
        let evaluation = Evaluation::new(requests.clone());
        let scope = requests.scope();
        evaluation.invalidate();
        assert!(scope.matches(&requests.scope()));
        requests.close();
        evaluation.close();
        evaluation.close();
        let mut state = evaluation.state.borrow_mut();
        assert!(matches!(
            state.bind("main", requests.scope(), Instant::now()),
            Err(ScriptError::PageClosed)
        ));
        assert!(state.pending.is_empty());
    }

    #[test]
    fn json_validation_preserves_text_and_rejects_invalid_or_excessive_values() {
        for json in [
            "null",
            "true",
            "42",
            "\"hello\"",
            "[]",
            "{\"v\":\"🦊\"}",
            " \n [1, 2] ",
        ] {
            assert_eq!(result(Status::Ok, json.into()), Ok(json.into()));
        }
        for invalid in ["", "undefined", "NaN", "{} false", "{", "\"unterminated"] {
            assert_eq!(
                result(Status::Ok, invalid.into()),
                Err(ScriptError::Backend("CEF returned invalid JSON".into()))
            );
        }
        let deep = format!("{}0{}", "[".repeat(256), "]".repeat(256));
        assert!(result(Status::Ok, deep).is_err());
        assert_eq!(
            result(Status::Ok, "x".repeat(protocol::RESULT_LIMIT + 1)),
            Err(ScriptError::ResultTooLarge)
        );
    }

    #[test]
    fn native_errors_use_fixed_diagnostics_not_renderer_text() {
        assert_eq!(
            result(Status::StaleContext, "untrusted".into()),
            Err(ScriptError::DocumentChanged)
        );
        assert_eq!(
            result(Status::ResultTooLarge, "untrusted".into()),
            Err(ScriptError::ResultTooLarge)
        );
        for status in [
            Status::NoContext,
            Status::JavascriptException,
            Status::SerializationError,
            Status::InternalError,
            Status::Capacity,
        ] {
            assert_eq!(
                result(status, "untrusted page exception".into()),
                Err(ScriptError::Backend(status.diagnostic().into()))
            );
        }
    }
}
