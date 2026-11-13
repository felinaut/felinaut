//! Bounded, engine-neutral script requests with a two-phase native dispatch handshake.
//!
//! Call `completion()` once, hand that token to the engine, then call `accept()` only
//! if dispatch succeeded (otherwise `reject()` or drop the dispatch). Callbacks run
//! on the thread resolving the request; they must enqueue browser/UI work themselves.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use super::ScriptCallback;

const REQUEST_LIMIT: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RESULT_LIMIT: usize = 1024 * 1024;

type ScriptResult = Result<String, ScriptError>;
type Delivery = (ScriptCallback, ScriptResult);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptError {
    PageClosed,
    DocumentChanged,
    TimedOut,
    Discarded,
    TooManyRequests,
    ResultTooLarge,
    Backend(String),
    Capacity,
    SequenceExhausted,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageClosed => f.write_str("page closed"),
            Self::DocumentChanged => f.write_str("document changed"),
            Self::TimedOut => f.write_str("script request timed out"),
            Self::Discarded => f.write_str("script completion discarded"),
            Self::TooManyRequests => f.write_str("too many script requests"),
            Self::ResultTooLarge => f.write_str("script result exceeds the byte limit"),
            Self::Backend(message) => write!(f, "script backend error: {message}"),
            Self::Capacity => f.write_str("script request deadline exceeds clock capacity"),
            Self::SequenceExhausted => f.write_str("script request sequence exhausted"),
        }
    }
}

impl std::error::Error for ScriptError {}

/// Clones share one bounded ledger. Only these owners keep the ledger alive.
#[derive(Clone)]
pub struct ScriptRequests {
    owner: Arc<Owner>,
}

impl Default for ScriptRequests {
    fn default() -> Self {
        Self {
            owner: Arc::new(Owner {
                state: Mutex::new(State {
                    closed: false,
                    epoch: 0,
                    next_request: 0,
                    requests: BTreeMap::new(),
                }),
            }),
        }
    }
}

impl ScriptRequests {
    /// Reserve a slot, including while native dispatch is still provisional.
    /// An error drops the supplied callback without invoking it.
    pub fn prepare(&self, now: Instant, callback: ScriptCallback) -> Result<Dispatch, ScriptError> {
        let mut callback = Some(callback);
        let mut deliveries = Vec::new();
        let result = {
            let mut state = self.owner.lock();
            if state.closed {
                Err(ScriptError::PageClosed)
            } else if state.requests.len() >= REQUEST_LIMIT {
                Err(ScriptError::TooManyRequests)
            } else if let Some(next) = state.next_request.checked_add(1) {
                if let Some(deadline) = now.checked_add(REQUEST_TIMEOUT) {
                    let id = state.next_request;
                    state.next_request = next;
                    state.requests.insert(
                        id,
                        Entry {
                            callback: callback.take().expect("reserved callback"),
                            deadline,
                            accepted: false,
                            completion_issued: false,
                            cancelled: false,
                            result: None,
                        },
                    );
                    Ok(Dispatch {
                        owner: Arc::downgrade(&self.owner),
                        id,
                        active: true,
                    })
                } else {
                    Err(ScriptError::Capacity)
                }
            } else {
                state.closed = true;
                deliveries = state.cancel(ScriptError::SequenceExhausted, |_| true);
                Err(ScriptError::SequenceExhausted)
            }
        };
        if let Some(callback) = callback {
            dispose(callback);
        }
        deliver_all(deliveries);
        result
    }

    /// Invalidate the old document before delivering its accepted cancellations.
    /// Provisional requests retain the error until accepted, or vanish on rejection.
    pub fn advance_document(&self) {
        let deliveries = {
            let mut state = self.owner.lock();
            if state.closed {
                return;
            }
            let error = if let Some(epoch) = state.epoch.checked_add(1) {
                state.epoch = epoch;
                ScriptError::DocumentChanged
            } else {
                state.closed = true;
                ScriptError::SequenceExhausted
            };
            state.cancel(error, |_| true)
        };
        deliver_all(deliveries);
    }

    /// Idempotent page teardown. No later request can be prepared successfully.
    pub fn close(&self) {
        let deliveries = {
            let mut state = self.owner.lock();
            state.closed = true;
            state.cancel(ScriptError::PageClosed, |_| true)
        };
        deliver_all(deliveries);
    }

    /// Deadlines are inclusive and measured from `prepare`, not dispatch acceptance.
    /// Acceptance and completion also enforce deadlines. Poll this for requests whose
    /// engines never complete; the ledger starts no timers or threads.
    pub fn expire(&self, now: Instant) {
        let deliveries = self
            .owner
            .lock()
            .cancel(ScriptError::TimedOut, |entry| entry.deadline <= now);
        deliver_all(deliveries);
    }

    pub fn scope(&self) -> RequestScope {
        RequestScope {
            owner: Arc::downgrade(&self.owner),
            epoch: self.owner.lock().epoch,
        }
    }
}

/// Opaque document identity: no global IDs, page data, or native pointers.
#[derive(Clone)]
pub struct RequestScope {
    owner: Weak<Owner>,
    epoch: u64,
}

impl RequestScope {
    pub fn is_current(&self) -> bool {
        let Some(owner) = self.owner.upgrade() else {
            return false;
        };
        let state = owner.lock();
        !state.closed && state.epoch == self.epoch
    }

    /// Both scopes must identify the same still-open document in the same ledger.
    pub fn matches(&self, other: &Self) -> bool {
        self.epoch == other.epoch && self.owner.ptr_eq(&other.owner) && self.is_current()
    }
}

/// A provisional reservation. Dropping it is equivalent to rejecting dispatch.
#[must_use = "accept only after native dispatch succeeds; otherwise reject or drop"]
pub struct Dispatch {
    owner: Weak<Owner>,
    id: u64,
    active: bool,
}

impl Dispatch {
    /// Issue the unique native completion token. Repeated calls return inert tokens
    /// rather than panicking or letting a second token discard the first one's work.
    pub fn completion(&self) -> Completion {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.lock();
            if let Some(entry) = state.requests.get_mut(&self.id)
                && !entry.completion_issued
            {
                entry.completion_issued = true;
                return Completion {
                    owner: self.owner.clone(),
                    id: self.id,
                };
            }
        }
        Completion {
            owner: Weak::new(),
            id: self.id,
        }
    }

    pub fn accept(self) {
        self.accept_at(Instant::now());
    }

    fn accept_at(mut self, now: Instant) {
        self.active = false;
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let delivery = {
            let mut state = owner.lock();
            let Some(mut entry) = state.requests.remove(&self.id) else {
                return;
            };
            entry.check_deadline(now);
            if let Some(result) = entry.result.take() {
                Some((entry.callback, result))
            } else if !entry.completion_issued {
                // Misuse must not leave an unfinishable accepted request behind.
                Some((entry.callback, Err(ScriptError::Discarded)))
            } else {
                entry.accepted = true;
                state.requests.insert(self.id, entry);
                None
            }
        };
        if let Some((callback, result)) = delivery {
            deliver(callback, result);
        }
    }

    pub fn reject(self) {
        // Drop performs the silent removal, even if a synchronous result arrived.
    }
}

impl Drop for Dispatch {
    fn drop(&mut self) {
        if self.active
            && let Some(owner) = self.owner.upgrade()
        {
            let entry = owner.lock().requests.remove(&self.id);
            if let Some(entry) = entry {
                dispose(entry.callback);
            }
        }
    }
}

/// Weak, Send + Sync native completion token. Not cloneable; duplicate calls to
/// `complete` are harmless. Dropping an unresolved token completes with Discarded
/// unless its deadline has elapsed or a lifecycle cancellation already won.
#[must_use = "retain in the native callback; dropping reports Discarded after acceptance"]
pub struct Completion {
    owner: Weak<Owner>,
    id: u64,
}

impl Completion {
    pub fn complete(&self, result: ScriptResult) {
        self.complete_at(Instant::now(), result);
    }

    fn complete_at(&self, now: Instant, result: ScriptResult) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let delivery = {
            let mut state = owner.lock();
            let Some(entry) = state.requests.get_mut(&self.id) else {
                return;
            };
            entry.check_deadline(now);
            if !entry.accepted && entry.result.is_some() {
                return;
            }
            let result = if let Some(result) = entry.result.take() {
                result
            } else {
                // Bound both successful values and backend-provided error messages
                // before buffering anything in the ledger. String::len counts bytes.
                match &result {
                    Ok(value) | Err(ScriptError::Backend(value)) if value.len() > RESULT_LIMIT => {
                        Err(ScriptError::ResultTooLarge)
                    }
                    _ => result,
                }
            };
            if entry.accepted {
                let entry = state.requests.remove(&self.id).expect("pending request");
                Some((entry.callback, result))
            } else {
                entry.result = Some(result);
                None
            }
        };
        if let Some((callback, result)) = delivery {
            deliver(callback, result);
        }
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        self.complete(Err(ScriptError::Discarded));
    }
}

struct Owner {
    state: Mutex<State>,
}

impl Owner {
    fn lock(&self) -> MutexGuard<'_, State> {
        // User code never runs under this lock, including callback destruction.
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        let entries = std::mem::take(&mut state.requests);
        for entry in entries.into_values() {
            if entry.accepted {
                deliver(entry.callback, Err(ScriptError::PageClosed));
            } else {
                // No strong owner survives, so a provisional dispatch cannot be accepted.
                dispose(entry.callback);
            }
        }
    }
}

struct State {
    closed: bool,
    epoch: u64,
    next_request: u64,
    requests: BTreeMap<u64, Entry>,
}

impl State {
    fn cancel(&mut self, error: ScriptError, predicate: impl Fn(&Entry) -> bool) -> Vec<Delivery> {
        let ids: Vec<_> = self
            .requests
            .iter()
            .filter(|(_, entry)| predicate(entry))
            .map(|(&id, _)| id)
            .collect();
        let mut deliveries = Vec::new();
        for id in ids {
            if self.requests[&id].accepted {
                let entry = self.requests.remove(&id).expect("pending request");
                deliveries.push((entry.callback, Err(error.clone())));
            } else {
                let entry = self.requests.get_mut(&id).expect("provisional request");
                if !entry.cancelled {
                    // Lifecycle cancellation overrides buffered native results, but
                    // the first cancellation wins over later close/expire/navigation.
                    entry.cancelled = true;
                    entry.result = Some(Err(error.clone()));
                }
            }
        }
        deliveries
    }
}

struct Entry {
    callback: ScriptCallback,
    deadline: Instant,
    accepted: bool,
    completion_issued: bool,
    cancelled: bool,
    result: Option<ScriptResult>,
}

impl Entry {
    fn check_deadline(&mut self, now: Instant) {
        if !self.cancelled && self.deadline <= now {
            // Timeout supersedes buffered native results, not earlier lifecycle
            // cancellations. Record it now so later cancellation preserves it too.
            self.cancelled = true;
            self.result = Some(Err(ScriptError::TimedOut));
        }
    }
}

fn deliver_all(deliveries: Vec<Delivery>) {
    for (callback, result) in deliveries {
        deliver(callback, result);
    }
}

fn deliver(callback: ScriptCallback, result: ScriptResult) {
    contain_panic(move || callback(result));
}

fn dispose(callback: ScriptCallback) {
    contain_panic(move || drop(callback));
}

fn contain_panic(action: impl FnOnce()) {
    let Err(payload) = catch_unwind(AssertUnwindSafe(action)) else {
        return;
    };
    // Release ordinary panic payloads without logging them. A hostile payload's
    // destructor may itself panic; contain that too at the native boundary.
    if let Err(nested) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
        std::mem::forget(nested);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, TryRecvError, channel};
    use std::thread;

    fn callback() -> (ScriptCallback, Receiver<ScriptResult>) {
        let (sender, receiver) = channel();
        (
            Box::new(move |result| sender.send(result).unwrap()),
            receiver,
        )
    }

    fn pending(
        requests: &ScriptRequests,
        now: Instant,
    ) -> (Dispatch, Completion, Receiver<ScriptResult>) {
        let (callback, receiver) = callback();
        let dispatch = requests.prepare(now, callback).unwrap();
        let completion = dispatch.completion();
        (dispatch, completion, receiver)
    }

    fn empty(receiver: &Receiver<ScriptResult>) {
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    }

    fn disconnected(receiver: &Receiver<ScriptResult>) {
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn success_empty_backend_error_and_duplicates() {
        let requests = ScriptRequests::default();
        for result in [
            Ok("value".into()),
            Ok(String::new()),
            Err(ScriptError::Backend("failed".into())),
        ] {
            let (dispatch, completion, receiver) = pending(&requests, Instant::now());
            dispatch.accept();
            empty(&receiver);
            completion.complete(result.clone());
            completion.complete(Ok("duplicate".into()));
            drop(completion);
            assert_eq!(receiver.try_recv().unwrap(), result);
            disconnected(&receiver);
        }
    }

    #[test]
    fn synchronous_result_is_buffered_until_accept() {
        let requests = ScriptRequests::default();
        let (dispatch, completion, receiver) = pending(&requests, Instant::now());
        completion.complete(Ok("first".into()));
        completion.complete(Ok("second".into()));
        drop(completion);
        empty(&receiver);
        dispatch.accept();
        assert_eq!(receiver.try_recv().unwrap(), Ok("first".into()));
        disconnected(&receiver);
    }

    #[test]
    fn synchronous_drop_is_discarded_only_after_accept() {
        let requests = ScriptRequests::default();
        let (dispatch, completion, receiver) = pending(&requests, Instant::now());
        drop(completion);
        empty(&receiver);
        dispatch.accept();
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::Discarded));
        disconnected(&receiver);
    }

    #[test]
    fn rejecting_or_dropping_dispatch_never_invokes_callback() {
        let requests = ScriptRequests::default();
        for reject in [false, true] {
            for synchronous in [false, true] {
                let (dispatch, completion, receiver) = pending(&requests, Instant::now());
                if synchronous {
                    completion.complete(Ok("buffered".into()));
                }
                if reject {
                    dispatch.reject();
                } else {
                    drop(dispatch);
                }
                disconnected(&receiver);
                completion.complete(Ok("late".into()));
                drop(completion);
                disconnected(&receiver);
            }
        }
        assert!(requests.owner.lock().requests.is_empty());
    }

    #[test]
    fn completion_can_only_be_issued_once_and_missing_token_is_discarded() {
        let requests = ScriptRequests::default();
        let (dispatch, completion, receiver) = pending(&requests, Instant::now());
        let duplicate = dispatch.completion();
        duplicate.complete(Ok("not authorized".into()));
        drop(duplicate);
        dispatch.accept();
        empty(&receiver);
        completion.complete(Ok("real token".into()));
        assert_eq!(receiver.try_recv().unwrap(), Ok("real token".into()));

        let (callback, receiver) = callback();
        requests.prepare(Instant::now(), callback).unwrap().accept();
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::Discarded));
    }

    #[test]
    fn dropped_accepted_completion_reports_discarded_once() {
        let requests = ScriptRequests::default();
        let (dispatch, completion, receiver) = pending(&requests, Instant::now());
        dispatch.accept();
        drop(completion);
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::Discarded));
        disconnected(&receiver);
    }

    #[test]
    fn navigation_and_close_before_or_after_accept() {
        for close in [false, true] {
            for accepted in [false, true] {
                for buffered in [false, true] {
                    let requests = ScriptRequests::default();
                    let scope = requests.scope();
                    let (dispatch, completion, receiver) = pending(&requests, Instant::now());
                    // Buffer only while provisional; a previously delivered result
                    // cannot be retroactively cancelled.
                    if buffered {
                        completion.complete(Ok("old document".into()));
                    }
                    let dispatch = if accepted {
                        dispatch.accept();
                        None
                    } else {
                        Some(dispatch)
                    };
                    if close {
                        requests.close();
                    } else {
                        requests.advance_document();
                    }
                    assert!(!scope.is_current());
                    if let Some(dispatch) = dispatch {
                        empty(&receiver);
                        dispatch.accept();
                    }
                    let expected = if buffered && accepted {
                        Ok("old document".into())
                    } else if close {
                        Err(ScriptError::PageClosed)
                    } else {
                        Err(ScriptError::DocumentChanged)
                    };
                    completion.complete(Ok("late".into()));
                    drop(completion);
                    assert_eq!(receiver.try_recv().unwrap(), expected);
                    disconnected(&receiver);
                    if close {
                        let (callback, receiver) = callback();
                        assert!(matches!(
                            requests.prepare(Instant::now(), callback),
                            Err(ScriptError::PageClosed)
                        ));
                        disconnected(&receiver);
                    } else {
                        assert!(requests.scope().is_current());
                    }
                }
            }
        }
    }

    #[test]
    fn cancelled_provisional_rejection_is_silent_and_first_cancellation_wins() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let (rejected, rejected_completion, rejected_receiver) = pending(&requests, now);
        let (accepted, completion, receiver) = pending(&requests, now);
        requests.advance_document();
        requests.close();
        requests.expire(now + REQUEST_TIMEOUT);
        requests.close();
        rejected.reject();
        disconnected(&rejected_receiver);
        drop(rejected_completion);
        accepted.accept();
        assert_eq!(
            receiver.try_recv().unwrap(),
            Err(ScriptError::DocumentChanged)
        );
        completion.complete(Ok("late".into()));
        disconnected(&receiver);
    }

    #[test]
    fn expiry_is_inclusive_and_uses_prepare_time() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let (dispatch, completion, receiver) = pending(&requests, now);
        dispatch.accept();
        requests.expire(now + REQUEST_TIMEOUT - Duration::from_nanos(1));
        empty(&receiver);
        requests.expire(now + REQUEST_TIMEOUT);
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::TimedOut));
        requests.expire(now + REQUEST_TIMEOUT + Duration::from_secs(1));
        completion.complete(Ok("late".into()));
        disconnected(&receiver);

        let (dispatch, completion, receiver) = pending(&requests, now);
        completion.complete(Ok("buffered".into()));
        requests.expire(now + REQUEST_TIMEOUT);
        empty(&receiver);
        dispatch.accept();
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::TimedOut));
    }

    #[test]
    fn completion_enforces_deadline_without_prior_expire_including_native_errors() {
        let now = Instant::now();
        for elapsed in [
            REQUEST_TIMEOUT - Duration::from_nanos(1),
            REQUEST_TIMEOUT,
            REQUEST_TIMEOUT + Duration::from_nanos(1),
        ] {
            for result in [
                Ok("native value".into()),
                Err(ScriptError::Backend("native failure".into())),
            ] {
                let requests = ScriptRequests::default();
                let (dispatch, completion, receiver) = pending(&requests, now);
                dispatch.accept_at(now);
                completion.complete_at(now + elapsed, result.clone());
                let expected = if elapsed < REQUEST_TIMEOUT {
                    result
                } else {
                    Err(ScriptError::TimedOut)
                };
                assert_eq!(receiver.try_recv().unwrap(), expected);
                completion.complete_at(now + elapsed, Ok("duplicate".into()));
                drop(completion);
                disconnected(&receiver);
                assert!(requests.owner.lock().requests.is_empty());
            }
        }
    }

    #[test]
    fn acceptance_enforces_deadline_even_with_a_buffered_native_result() {
        let now = Instant::now();
        for elapsed in [
            REQUEST_TIMEOUT - Duration::from_nanos(1),
            REQUEST_TIMEOUT,
            REQUEST_TIMEOUT + Duration::from_nanos(1),
        ] {
            for buffered in [
                None,
                Some(Ok("buffered".into())),
                Some(Err(ScriptError::Backend("buffered error".into()))),
                Some(Err(ScriptError::Discarded)),
            ] {
                let requests = ScriptRequests::default();
                let (dispatch, completion, receiver) = pending(&requests, now);
                if let Some(result) = &buffered {
                    completion.complete_at(now, result.clone());
                }
                empty(&receiver);
                dispatch.accept_at(now + elapsed);
                completion.complete_at(now + elapsed, Ok("after acceptance".into()));
                let expected = if elapsed < REQUEST_TIMEOUT {
                    buffered.unwrap_or(Ok("after acceptance".into()))
                } else {
                    Err(ScriptError::TimedOut)
                };
                assert_eq!(receiver.try_recv().unwrap(), expected);
                drop(completion);
                disconnected(&receiver);
                assert!(requests.owner.lock().requests.is_empty());
            }
        }
    }

    #[test]
    fn earlier_document_or_page_cancellation_survives_late_completion_and_acceptance() {
        let now = Instant::now();
        let late = now + REQUEST_TIMEOUT + Duration::from_secs(1);
        for close in [false, true] {
            for phase in 0..3 {
                let requests = ScriptRequests::default();
                let (dispatch, completion, receiver) = pending(&requests, now);
                if phase == 2 {
                    completion.complete_at(now, Ok("buffered old document".into()));
                }
                let dispatch = if phase == 0 {
                    dispatch.accept_at(now);
                    None
                } else {
                    Some(dispatch)
                };
                if close {
                    requests.close();
                } else {
                    requests.advance_document();
                }
                // A later page close must not replace DocumentChanged either.
                requests.close();
                completion.complete_at(late, Err(ScriptError::Backend("late error".into())));
                if let Some(dispatch) = dispatch {
                    empty(&receiver);
                    dispatch.accept_at(late);
                }
                let expected = if close {
                    ScriptError::PageClosed
                } else {
                    ScriptError::DocumentChanged
                };
                assert_eq!(receiver.try_recv().unwrap(), Err(expected));
                drop(completion);
                disconnected(&receiver);
            }
        }
    }

    #[test]
    fn provisional_late_completion_records_timeout_as_first_cancellation() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let late = now + REQUEST_TIMEOUT;
        let (dispatch, completion, receiver) = pending(&requests, now);
        completion.complete_at(late, Err(ScriptError::Backend("late native error".into())));
        empty(&receiver);
        requests.advance_document();
        requests.close();
        dispatch.accept_at(late);
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::TimedOut));
        drop(completion);
        disconnected(&receiver);
    }

    #[test]
    fn expiry_keeps_newer_requests_and_releases_capacity() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let (old, old_completion, old_receiver) = pending(&requests, now);
        let (new, new_completion, new_receiver) = pending(&requests, now + Duration::from_secs(1));
        old.accept();
        new.accept();
        requests.expire(now + REQUEST_TIMEOUT);
        assert_eq!(old_receiver.try_recv().unwrap(), Err(ScriptError::TimedOut));
        empty(&new_receiver);
        new_completion.complete(Ok("new".into()));
        assert_eq!(new_receiver.try_recv().unwrap(), Ok("new".into()));
        drop(old_completion);
        assert!(requests.owner.lock().requests.is_empty());
    }

    #[test]
    fn limit_includes_provisional_and_cancelled_reservations_and_recovers() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let mut entries: Vec<_> = (0..REQUEST_LIMIT)
            .map(|_| pending(&requests, now))
            .collect();
        let assert_full = || {
            let (callback, receiver) = callback();
            assert!(matches!(
                requests.prepare(now, callback),
                Err(ScriptError::TooManyRequests)
            ));
            disconnected(&receiver);
        };
        assert_full();
        let (dispatch, completion, receiver) = entries.pop().unwrap();
        dispatch.accept();
        assert_full();
        completion.complete(Ok("done".into()));
        assert_eq!(receiver.try_recv().unwrap(), Ok("done".into()));
        entries.push(pending(&requests, now));
        assert_full();
        requests.advance_document();
        assert_full();
        let (dispatch, _, receiver) = entries.pop().unwrap();
        dispatch.reject();
        disconnected(&receiver);
        let (dispatch, completion, receiver) = pending(&requests, now);
        dispatch.accept();
        drop(completion);
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::Discarded));
        drop(entries);
        assert!(requests.owner.lock().requests.is_empty());
    }

    #[test]
    fn result_limit_counts_utf8_bytes_and_bounds_backend_errors() {
        let requests = ScriptRequests::default();
        for synchronous in [false, true] {
            for (value, too_large) in [
                ("a".repeat(RESULT_LIMIT), false),
                ("a".repeat(RESULT_LIMIT + 1), true),
                ("\u{1f680}".repeat(RESULT_LIMIT / 4), false),
                ("\u{1f680}".repeat(RESULT_LIMIT / 4 + 1), true),
            ] {
                for backend_error in [false, true] {
                    let (dispatch, completion, receiver) = pending(&requests, Instant::now());
                    let result = if backend_error {
                        Err(ScriptError::Backend(value.clone()))
                    } else {
                        Ok(value.clone())
                    };
                    let expected = if too_large {
                        Err(ScriptError::ResultTooLarge)
                    } else {
                        result.clone()
                    };
                    if synchronous {
                        completion.complete(result);
                        empty(&receiver);
                        dispatch.accept();
                    } else {
                        dispatch.accept();
                        completion.complete(result);
                    }
                    assert_eq!(receiver.try_recv().unwrap(), expected);
                    disconnected(&receiver);
                }
            }
        }
    }

    #[test]
    fn sequence_overflow_closes_without_reusing_ids() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        let scope = requests.scope();
        let (accepted, completion, receiver) = pending(&requests, now);
        accepted.accept();
        let (provisional, provisional_completion, provisional_receiver) = pending(&requests, now);
        requests.owner.lock().next_request = u64::MAX - 1;
        let (last, last_completion, last_receiver) = pending(&requests, now);
        assert_eq!(last.id, u64::MAX - 1);
        let (callback, rejected_receiver) = callback();
        assert!(matches!(
            requests.prepare(now, callback),
            Err(ScriptError::SequenceExhausted)
        ));
        disconnected(&rejected_receiver);
        assert!(!scope.is_current());
        assert_eq!(
            receiver.try_recv().unwrap(),
            Err(ScriptError::SequenceExhausted)
        );
        provisional.accept();
        last.accept();
        assert_eq!(
            provisional_receiver.try_recv().unwrap(),
            Err(ScriptError::SequenceExhausted)
        );
        assert_eq!(
            last_receiver.try_recv().unwrap(),
            Err(ScriptError::SequenceExhausted)
        );
        completion.complete(Ok("late".into()));
        drop((provisional_completion, last_completion));
        assert!(matches!(
            requests.prepare(now, Box::new(|_| {})),
            Err(ScriptError::PageClosed)
        ));
    }

    #[test]
    fn document_epoch_overflow_closes_instead_of_matching_old_scopes() {
        let requests = ScriptRequests::default();
        let original = requests.scope();
        requests.owner.lock().epoch = u64::MAX;
        let last = requests.scope();
        let (dispatch, completion, receiver) = pending(&requests, Instant::now());
        dispatch.accept();
        requests.advance_document();
        assert_eq!(
            receiver.try_recv().unwrap(),
            Err(ScriptError::SequenceExhausted)
        );
        assert!(!original.is_current());
        assert!(!last.is_current());
        assert!(!requests.scope().is_current());
        drop(completion);
    }

    #[test]
    fn scopes_are_allocation_and_document_specific() {
        let first = ScriptRequests::default();
        let second = ScriptRequests::default();
        let old = first.scope();
        let clone = old.clone();
        assert!(old.is_current());
        assert!(old.matches(&clone));
        assert!(old.matches(&first.clone().scope()));
        assert!(!old.matches(&second.scope()));
        first.advance_document();
        let current = first.scope();
        assert!(!old.matches(&clone));
        assert!(!old.matches(&current));
        assert!(!current.matches(&old));
        assert!(current.matches(&first.scope()));
        first.close();
        assert!(!current.matches(&current));
        assert!(second.scope().is_current());
    }

    #[test]
    fn last_owner_drop_closes_accepted_and_disposes_provisional_with_only_weak_handles() {
        let requests = ScriptRequests::default();
        let clone = requests.clone();
        let scope = requests.scope();
        let weak = Arc::downgrade(&requests.owner);
        let (accepted, completion, receiver) = pending(&requests, Instant::now());
        accepted.accept();
        let (provisional, provisional_completion, provisional_receiver) =
            pending(&requests, Instant::now());
        drop(requests);
        assert!(scope.is_current());
        empty(&receiver);
        drop(clone);
        assert!(weak.upgrade().is_none());
        assert!(!scope.is_current());
        assert!(!scope.matches(&scope.clone()));
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::PageClosed));
        disconnected(&receiver);
        disconnected(&provisional_receiver);
        completion.complete(Ok("late".into()));
        provisional_completion.complete(Ok("late".into()));
        provisional.accept();
        drop((completion, provisional_completion));
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn callbacks_can_reenter_ledger_without_a_lock_or_deadlock() {
        let requests = ScriptRequests::default();
        let reentrant = requests.clone();
        let (sender, receiver) = channel();
        let dispatch = requests
            .prepare(
                Instant::now(),
                Box::new(move |result| {
                    assert_eq!(result, Ok("done".into()));
                    assert!(reentrant.owner.state.try_lock().is_ok());
                    assert!(reentrant.scope().is_current());
                    let (nested, completion, nested_receiver) = pending(&reentrant, Instant::now());
                    nested.accept();
                    completion.complete(Ok("nested".into()));
                    assert_eq!(nested_receiver.try_recv().unwrap(), Ok("nested".into()));
                    reentrant.advance_document();
                    reentrant.expire(Instant::now());
                    reentrant.close();
                    sender.send(()).unwrap();
                }),
            )
            .unwrap();
        let completion = dispatch.completion();
        dispatch.accept();
        completion.complete(Ok("done".into()));
        receiver.try_recv().unwrap();
        assert!(!requests.scope().is_current());
    }

    #[test]
    fn cancellation_callbacks_run_after_state_transition_and_outside_lock() {
        for close in [false, true] {
            let requests = ScriptRequests::default();
            let old_scope = requests.scope();
            let weak = Arc::downgrade(&requests.owner);
            let (sender, receiver) = channel();
            let dispatch = requests
                .prepare(
                    Instant::now(),
                    Box::new(move |result| {
                        let owner = weak.upgrade().unwrap();
                        assert!(owner.state.try_lock().is_ok());
                        assert!(!old_scope.is_current());
                        sender.send(result).unwrap();
                    }),
                )
                .unwrap();
            let completion = dispatch.completion();
            dispatch.accept();
            if close {
                requests.close();
            } else {
                requests.advance_document();
            }
            assert_eq!(
                receiver.try_recv().unwrap(),
                Err(if close {
                    ScriptError::PageClosed
                } else {
                    ScriptError::DocumentChanged
                })
            );
            drop(completion);
        }
    }

    struct DropProbe {
        owner: Weak<Owner>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            if let Some(owner) = self.owner.upgrade() {
                assert!(owner.state.try_lock().is_ok());
            }
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn callback_captures_are_dropped_once_and_outside_lock() {
        let requests = ScriptRequests::default();
        let drops = Arc::new(AtomicUsize::new(0));
        for resolve in [false, true] {
            let probe = DropProbe {
                owner: Arc::downgrade(&requests.owner),
                drops: drops.clone(),
            };
            let dispatch = requests
                .prepare(
                    Instant::now(),
                    Box::new(move |_| {
                        let _ = &probe;
                    }),
                )
                .unwrap();
            let completion = dispatch.completion();
            if resolve {
                dispatch.accept();
                completion.complete(Ok(String::new()));
            } else {
                dispatch.reject();
            }
            drop(completion);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        requests.close();
        let probe = DropProbe {
            owner: Arc::downgrade(&requests.owner),
            drops: drops.clone(),
        };
        assert!(matches!(
            requests.prepare(
                Instant::now(),
                Box::new(move |_| {
                    let _ = &probe;
                })
            ),
            Err(ScriptError::PageClosed)
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn worker_thread_can_complete_or_discard_and_scope_is_send() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<ScriptRequests>();
        send_sync::<RequestScope>();
        send_sync::<Dispatch>();
        send_sync::<Completion>();
        let requests = ScriptRequests::default();
        for discard in [false, true] {
            let (dispatch, completion, receiver) = pending(&requests, Instant::now());
            dispatch.accept();
            let scope = requests.scope();
            thread::spawn(move || {
                assert!(scope.is_current());
                if !discard {
                    completion.complete(Ok("worker".into()));
                }
            })
            .join()
            .unwrap();
            assert_eq!(
                receiver.try_recv().unwrap(),
                if discard {
                    Err(ScriptError::Discarded)
                } else {
                    Ok("worker".into())
                }
            );
            disconnected(&receiver);
        }
    }

    #[test]
    fn unrepresentable_deadline_rejects_silently_without_consuming_sequence_or_capacity() {
        let requests = ScriptRequests::default();
        let now = Instant::now();
        // Find the platform's representable clock boundary using checked arithmetic.
        let (mut low, mut high) = (0, u64::MAX);
        while low < high {
            let middle = low + (high - low) / 2 + 1;
            if now.checked_add(Duration::from_secs(middle)).is_some() {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let boundary = now.checked_add(Duration::from_secs(low)).unwrap();
        let (callback, receiver) = callback();
        assert!(matches!(
            requests.prepare(boundary, callback),
            Err(ScriptError::Capacity)
        ));
        disconnected(&receiver);
        assert!(requests.owner.lock().requests.is_empty());
        assert_eq!(requests.owner.lock().next_request, 0);
        assert!(requests.scope().is_current());
        let (dispatch, completion, receiver) = pending(&requests, now);
        dispatch.accept();
        completion.complete(Ok("recovered".into()));
        assert_eq!(receiver.try_recv().unwrap(), Ok("recovered".into()));
    }

    #[test]
    fn panic_is_contained_during_accept_drop_expiry_navigation_and_last_owner_teardown() {
        for path in 0..5 {
            let requests = ScriptRequests::default();
            let now = Instant::now();
            let dispatch = requests
                .prepare(now, Box::new(|_| panic!("test lifecycle callback")))
                .unwrap();
            let completion = dispatch.completion();
            let (other, other_completion, receiver) = pending(&requests, now);
            other.accept();
            let mut requests = Some(requests);
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    if path == 0 {
                        completion.complete(Ok(String::new()));
                        dispatch.accept();
                    } else {
                        dispatch.accept();
                        match path {
                            1 => drop(completion),
                            2 => requests.as_ref().unwrap().expire(now + REQUEST_TIMEOUT),
                            3 => requests.as_ref().unwrap().advance_document(),
                            4 => drop(requests.take()),
                            _ => unreachable!(),
                        }
                    }
                }))
                .is_ok()
            );
            if path < 2 {
                other_completion.complete(Ok("still usable".into()));
                assert_eq!(receiver.try_recv().unwrap(), Ok("still usable".into()));
            } else {
                let error = match path {
                    2 => ScriptError::TimedOut,
                    3 => ScriptError::DocumentChanged,
                    4 => ScriptError::PageClosed,
                    _ => unreachable!(),
                };
                assert_eq!(receiver.try_recv().unwrap(), Err(error));
            }
            disconnected(&receiver);
        }
    }

    #[test]
    fn callback_disposal_panics_are_contained_and_ordinary_panic_payloads_are_released() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("test callback disposal");
            }
        }
        let requests = ScriptRequests::default();
        for closed in [false, true] {
            if closed {
                requests.close();
            }
            let probe = PanicOnDrop;
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    let dispatch = requests.prepare(
                        Instant::now(),
                        Box::new(move |_| {
                            let _ = &probe;
                        }),
                    );
                    drop(dispatch);
                }))
                .is_ok()
            );
            assert!(!requests.owner.state.is_poisoned());
        }

        struct Payload(Arc<AtomicUsize>);
        impl Drop for Payload {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let payload = Payload(drops.clone());
        assert!(
            catch_unwind(AssertUnwindSafe(|| contain_panic(
                || std::panic::panic_any(payload)
            )))
            .is_ok()
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(
            catch_unwind(AssertUnwindSafe(|| contain_panic(
                || std::panic::panic_any(PanicOnDrop)
            )))
            .is_ok()
        );
    }

    #[test]
    fn panicking_callbacks_do_not_unwind_poison_or_interrupt_cancellation_batch() {
        let requests = ScriptRequests::default();
        let dispatch = requests
            .prepare(Instant::now(), Box::new(|_| panic!("test callback")))
            .unwrap();
        let completion = dispatch.completion();
        dispatch.accept();
        assert!(catch_unwind(AssertUnwindSafe(|| completion.complete(Ok(String::new())))).is_ok());
        assert!(!requests.owner.state.is_poisoned());

        let dispatch = requests
            .prepare(Instant::now(), Box::new(|_| panic!("test cancellation")))
            .unwrap();
        let completion = dispatch.completion();
        dispatch.accept();
        let (other, other_completion, receiver) = pending(&requests, Instant::now());
        other.accept();
        assert!(catch_unwind(AssertUnwindSafe(|| requests.close())).is_ok());
        assert_eq!(receiver.try_recv().unwrap(), Err(ScriptError::PageClosed));
        assert!(!requests.owner.state.is_poisoned());
        drop((completion, other_completion));
    }
}
