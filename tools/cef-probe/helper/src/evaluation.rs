//! Renderer-only JavaScript evaluation. No V8 handler, extension, or global is
//! installed. The browser must enforce its own page/document scope and deadlines;
//! neither evaluation nor JSON.stringify can be forcibly interrupted by this API.

use crate::evaluation_protocol::{
    self as protocol, Admission, ContextState, Field, FieldSpec, Id, Message, Sequence, Status,
};
use cef::*;
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};

#[derive(Clone, Default)]
pub struct Bridge {
    state: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    contexts: Vec<Record>,
    sequence: Sequence,
    disabled: bool,
}

struct Record {
    browser: Browser,
    frame_id: String,
    context: V8Context,
    evaluator: V8Value,
    admission: ContextState,
}

struct Identity {
    browser: Browser,
    frame_id: String,
}

struct Handles {
    context: V8Context,
    evaluator: V8Value,
}

/// Contains both callback panics and panics from destroying a panic payload.
/// No page/script/result contents are logged.
pub fn contain<T: Default>(callback: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(payload) => {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                std::mem::forget(payload);
            }
            T::default()
        }
    }
}

impl Bridge {
    fn callback<T: Default>(&self, callback: impl FnOnce() -> T) -> T {
        // A wrong-thread invocation must not touch or destroy any V8 handles.
        if currently_on(ThreadId::RENDERER) == 0 {
            return T::default();
        }
        let result = contain(|| Some(callback()));
        match result {
            Some(value) => value,
            None => {
                contain(|| {
                    let records = {
                        let mut state = self.state.borrow_mut();
                        state.disabled = true;
                        std::mem::take(&mut state.contexts)
                    };
                    drop(records);
                });
                T::default()
            }
        }
    }

    pub fn context_created(
        &self,
        browser: Option<&mut Browser>,
        frame: Option<&mut Frame>,
        context: Option<&mut V8Context>,
    ) {
        self.callback(|| {
            let (Some(browser), Some(frame), Some(context)) = (browser, frame, context) else {
                return;
            };
            let Ok(identity) = identity(browser, frame) else {
                return;
            };
            if !context_matches(context, frame) {
                return;
            }
            let Ok(handles) = capture(context) else {
                return;
            };
            let (old, serial) = {
                let mut state = self.state.borrow_mut();
                if state.disabled {
                    return;
                }
                if state
                    .contexts
                    .iter()
                    .any(|record| record.context.is_same(Some(context)) != 0)
                {
                    return;
                }
                let old = position(&state, &identity).map(|index| state.contexts.remove(index));
                if state.contexts.len() >= protocol::CONTEXT_LIMIT {
                    return;
                }
                let Some(serial) = state.sequence.allocate() else {
                    state.disabled = true;
                    return;
                };
                state.contexts.push(Record {
                    browser: identity.browser,
                    frame_id: identity.frame_id,
                    context: handles.context,
                    evaluator: handles.evaluator,
                    admission: ContextState::new(serial),
                });
                (old, serial)
            };
            if let Some(mut old) = old {
                let released = old.admission.release();
                drop(old);
                if let Some((binding, context)) = released {
                    send(frame, Message::Released { binding, context });
                }
            }
            // Recheck after releasing old references: hints are advisory only.
            let current = self
                .state
                .borrow()
                .contexts
                .iter()
                .any(|record| record.admission.context() == serial);
            if current {
                send(frame, Message::ContextHint);
            }
        });
    }

    pub fn context_released(&self, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
        self.callback(|| {
            let Some(context) = context else { return };
            let removed = {
                let mut state = self.state.borrow_mut();
                // Match the engine context, not just its possibly reused frame.
                let index = state
                    .contexts
                    .iter()
                    .position(|record| record.context.is_same(Some(context)) != 0);
                index.map(|index| state.contexts.remove(index))
            };
            if let Some(mut record) = removed {
                let released = record.admission.release();
                drop(record);
                if let (Some(frame), Some((binding, context))) = (frame, released) {
                    send(frame, Message::Released { binding, context });
                }
            }
        });
    }

    pub fn browser_destroyed(&self, browser: Option<&mut Browser>) {
        self.callback(|| {
            let Some(browser) = browser else { return };
            let removed = {
                let mut state = self.state.borrow_mut();
                let mut removed = Vec::new();
                let mut index = 0;
                while index < state.contexts.len() {
                    if state.contexts[index].browser.is_same(Some(browser)) != 0 {
                        removed.push(state.contexts.remove(index));
                    } else {
                        index += 1;
                    }
                }
                removed
            };
            drop(removed);
        });
    }

    pub fn on_message(
        &self,
        browser: Option<&mut Browser>,
        frame: Option<&mut Frame>,
        source: ProcessId,
        message: Option<&mut ProcessMessage>,
    ) -> i32 {
        self.callback(|| {
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
            if source != ProcessId::BROWSER
                || !matches!(name.as_str(), protocol::BIND | protocol::RUN)
            {
                return 1;
            }
            let (Some(browser), Some(frame)) = (browser, frame) else {
                return 1;
            };
            let Ok(identity) = identity(browser, frame) else {
                return 1;
            };
            let Ok(message) = decode(&name, message) else {
                return 1;
            };
            match message {
                Message::Bind { binding } => self.bind(frame, &identity, binding),
                Message::Run {
                    binding,
                    context,
                    request,
                    source,
                } => self.run(frame, &identity, binding, context, request, source),
                _ => {}
            }
            1
        })
    }

    fn bind(&self, frame: &Frame, identity: &Identity, binding: Id) {
        let handles = self.handles(identity);
        let status = match handles {
            Some(ref handles) if context_matches(&handles.context, frame) => Ok(()),
            _ => Err(Status::NoContext),
        };
        let (context, status) = {
            let mut state = self.state.borrow_mut();
            if state.disabled {
                (None, Status::InternalError)
            } else if let Err(status) = status {
                (None, status)
            } else if let Some(index) = position(&state, identity) {
                let record = &mut state.contexts[index];
                match record.admission.bind(binding) {
                    Ok(()) => (Some(record.admission.context()), Status::Ok),
                    Err(status) => (None, status),
                }
            } else {
                (None, Status::NoContext)
            }
        };
        drop(handles);
        send(
            frame,
            Message::Bound {
                binding,
                context,
                status,
            },
        );
    }

    fn handles(&self, identity: &Identity) -> Option<Handles> {
        let state = self.state.borrow();
        if state.disabled {
            return None;
        }
        let record = &state.contexts[position(&state, identity)?];
        Some(Handles {
            context: record.context.clone(),
            evaluator: record.evaluator.clone(),
        })
    }

    fn run(
        &self,
        frame: &Frame,
        identity: &Identity,
        binding: Id,
        context: Id,
        request: Id,
        source: String,
    ) {
        let Some(mut handles) = self.handles(identity) else {
            failure(frame, binding, context, request, Status::StaleContext);
            return;
        };
        if !context_matches(&handles.context, frame) {
            drop(handles);
            failure(frame, binding, context, request, Status::StaleContext);
            return;
        }
        let admission = {
            let mut state = self.state.borrow_mut();
            position(&state, identity)
                .map(|index| {
                    state.contexts[index]
                        .admission
                        .begin(binding, context, request)
                })
                .unwrap_or(Admission::Reject(Status::StaleContext))
        };
        match admission {
            Admission::Replay => return,
            Admission::Reject(status) => {
                drop(handles);
                failure(frame, binding, context, request, status);
                return;
            }
            Admission::Execute => {}
        }
        // No state borrow is held while running page code, serialization, or IPC.
        let result = evaluate(&mut handles, frame, &source);
        let native_current = context_matches(&handles.context, frame);
        drop(handles);
        let current = {
            let mut state = self.state.borrow_mut();
            position(&state, identity).is_some_and(|index| {
                state.contexts[index]
                    .admission
                    .finish(binding, context, request)
            })
        };
        if !native_current || !current {
            failure(frame, binding, context, request, Status::StaleContext);
            return;
        }
        match result {
            Ok(payload) => send(
                frame,
                Message::Done {
                    binding,
                    context,
                    request,
                    status: Status::Ok,
                    payload,
                },
            ),
            Err(status) => failure(frame, binding, context, request, status),
        }
    }
}

fn position(state: &State, identity: &Identity) -> Option<usize> {
    state.contexts.iter().position(|record| {
        record.frame_id == identity.frame_id
            && record.browser.is_same(Some(&mut identity.browser.clone())) != 0
    })
}

fn identity(browser: &Browser, frame: &Frame) -> Result<Identity, Status> {
    if browser.is_valid() == 0 || frame.is_valid() == 0 || frame.is_main() == 0 {
        return Err(Status::StaleContext);
    }
    let mut owner = frame.browser().ok_or(Status::StaleContext)?;
    if browser.is_same(Some(&mut owner)) == 0 {
        return Err(Status::StaleContext);
    }
    let frame_id =
        read_text(frame.identifier(), protocol::FRAME_LIMIT).map_err(|_| Status::StaleContext)?;
    if frame_id.is_empty() {
        return Err(Status::StaleContext);
    }
    Ok(Identity {
        browser: browser.clone(),
        frame_id,
    })
}

fn context_matches(context: &V8Context, frame: &Frame) -> bool {
    if context.is_valid() == 0 || frame.is_valid() == 0 || frame.is_main() == 0 {
        return false;
    }
    frame.v8_context().is_some_and(|mut current| {
        current.is_valid() != 0 && context.is_same(Some(&mut current)) != 0
    })
}

struct Entered(V8Context);
impl Entered {
    fn new(context: &V8Context) -> Result<Self, Status> {
        if context.is_valid() == 0 || context.enter() == 0 {
            return Err(Status::StaleContext);
        }
        Ok(Self(context.clone()))
    }
}
impl Drop for Entered {
    fn drop(&mut self) {
        self.0.exit();
    }
}

// Raw results must stay in JavaScript: this CEF pin eagerly converts Date
// results via a checked ToNumber, and cannot wrap every BigInt/Symbol result.
// Only this trusted factory's function and its primitive tagged strings cross
// the CefV8Value return boundary. The strict closure is never published; source
// uses captured *indirect* eval and cannot access the closure's lexical state.
const EVALUATOR_FACTORY: &str = r#"function(evaluateSource, stringify, PromiseConstructor, hasInstance, apply, resultLimit) {
    "use strict";
    return function(source) {
        let value;
        try {
            value = evaluateSource(source);
        } catch (_) {
            return "3:";
        }
        try {
            if (value === void 0) return "0:null";
            const kind = typeof value;
            if (kind === "function" || kind === "symbol" || kind === "bigint") return "4:";
            if (value !== null && kind === "object" && apply(hasInstance, PromiseConstructor, [value])) return "4:";
            const json = stringify(value);
            if (typeof json !== "string") return "4:";
            if (json.length > resultLimit) return "5:";
            return "0:" + json;
        } catch (_) {
            return "4:";
        }
    };
}"#;

fn evaluator_source() -> String {
    format!(
        "({EVALUATOR_FACTORY})(eval, JSON.stringify, Promise, Function.prototype[Symbol.hasInstance], Reflect.apply, {})",
        protocol::RESULT_LIMIT,
    )
}

fn capture(context: &V8Context) -> Result<Handles, Status> {
    let _entered = Entered::new(context)?;
    let mut evaluator = None;
    let mut exception = None;
    let success = context.eval(
        Some(&CefString::from(evaluator_source().as_str())),
        None,
        1,
        Some(&mut evaluator),
        Some(&mut exception),
    ) != 0;
    if context.is_valid() == 0 {
        return Err(Status::StaleContext);
    }
    if !success {
        return Err(Status::SerializationError);
    }
    let evaluator = evaluator.ok_or(Status::SerializationError)?;
    if evaluator.is_valid() == 0
        || evaluator.is_function() == 0
        || evaluator.set_rethrow_exceptions(0) == 0
    {
        return Err(Status::SerializationError);
    }
    Ok(Handles {
        context: context.clone(),
        evaluator,
    })
}

fn evaluate(handles: &mut Handles, frame: &Frame, source: &str) -> Result<String, Status> {
    if !context_matches(&handles.context, frame) {
        return Err(Status::StaleContext);
    }
    let _entered = Entered::new(&handles.context)?;
    if handles.evaluator.is_valid() == 0 {
        return Err(Status::SerializationError);
    }
    let source =
        v8_value_create_string(Some(&CefString::from(source))).ok_or(Status::InternalError)?;
    let arguments = [Some(source)];
    let tagged = handles.evaluator.execute_function_with_context(
        Some(&mut handles.context),
        None,
        Some(&arguments),
    );
    if !context_matches(&handles.context, frame) {
        return Err(Status::StaleContext);
    }
    if handles.evaluator.is_valid() == 0 {
        return Err(Status::SerializationError);
    }
    if handles.evaluator.has_exception() != 0 {
        handles.evaluator.clear_exception();
        return Err(Status::SerializationError);
    }
    let tagged = tagged.ok_or(Status::SerializationError)?;
    if tagged.is_valid() == 0 || tagged.is_string() == 0 {
        return Err(Status::SerializationError);
    }
    // Include the two ASCII tag bytes in the transport budget, then check the
    // JSON payload itself. V8/stringify allocations are not interruptible/bounded.
    let tagged = read_text(tagged.string_value(), protocol::RESULT_LIMIT + 2).map_err(|error| {
        if error == protocol::ProtocolError::TooLarge {
            Status::ResultTooLarge
        } else {
            Status::SerializationError
        }
    })?;
    decode_evaluation(tagged)
}

fn decode_evaluation(mut tagged: String) -> Result<String, Status> {
    if tagged.len() > protocol::RESULT_LIMIT + 2 {
        return Err(Status::ResultTooLarge);
    }
    match tagged.as_str() {
        "3:" => return Err(Status::JavascriptException),
        "4:" => return Err(Status::SerializationError),
        "5:" => return Err(Status::ResultTooLarge),
        _ => {}
    }
    if !tagged.starts_with("0:") || tagged.len() == 2 {
        return Err(Status::SerializationError);
    }
    tagged.replace_range(..2, "");
    Ok(tagged)
}

fn failure(frame: &Frame, binding: Id, context: Id, request: Id, status: Status) {
    send(
        frame,
        Message::Done {
            binding,
            context,
            request,
            status,
            payload: status.diagnostic().into(),
        },
    );
}

fn read_text(value: CefStringUserfree, limit: usize) -> Result<String, protocol::ProtocolError> {
    let raw: Option<&sys::_cef_string_utf16_t> = (&value).into();
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
    // The userfree getter can already allocate inside CEF. Check its length
    // before the wrapper's copying conversion, then enforce the UTF-8 budget.
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

fn send(frame: &Frame, message: Message) {
    if frame.is_valid() == 0 {
        return;
    }
    let Ok((name, fields)) = message.into_wire() else {
        return;
    };
    let Some(mut message) = process_message_create(Some(&CefString::from(name))) else {
        return;
    };
    let Some(args) = message.argument_list() else {
        return;
    };
    if args.set_size(fields.len()) == 0 {
        return;
    }
    for (index, field) in fields.into_iter().enumerate() {
        let success = match field {
            Field::String(value) => args.set_string(index, Some(&CefString::from(value.as_str()))),
            Field::Int(value) => args.set_int(index, value),
        };
        if success == 0 {
            return;
        }
    }
    drop(args);
    // CEF invalidates message contents on send and does not guarantee delivery.
    frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_results_preserve_json_and_separate_exception_kinds() {
        for json in ["null", "42", "\"🦊\"", "{\"value\":[1,true]}", "\"3:\""] {
            assert_eq!(decode_evaluation(format!("0:{json}")), Ok(json.into()));
        }
        assert_eq!(
            decode_evaluation("3:".into()),
            Err(Status::JavascriptException)
        );
        assert_eq!(
            decode_evaluation("4:".into()),
            Err(Status::SerializationError)
        );
        assert_eq!(decode_evaluation("5:".into()), Err(Status::ResultTooLarge));
    }

    #[test]
    fn malformed_evaluator_returns_are_not_misclassified_as_context_changes() {
        for malformed in [
            "",
            "0:",
            "2:",
            "3:untrusted",
            "4:untrusted",
            "5:untrusted",
            "null",
            "🦊:",
        ] {
            assert_eq!(
                decode_evaluation(malformed.into()),
                Err(Status::SerializationError)
            );
        }
    }

    #[test]
    fn tagged_result_budget_excludes_tag_and_counts_utf8_bytes() {
        let payload = format!("\"{}\"", "é".repeat(protocol::RESULT_LIMIT / 2 - 1));
        assert_eq!(payload.len(), protocol::RESULT_LIMIT);
        assert_eq!(decode_evaluation(format!("0:{payload}")), Ok(payload));
        let oversized = format!("0:\"{}x\"", "é".repeat(protocol::RESULT_LIMIT / 2 - 1));
        assert_eq!(decode_evaluation(oversized), Err(Status::ResultTooLarge));
    }

    #[test]
    fn panic_containment_returns_default_and_success_value() {
        assert_eq!(contain(|| 7), 7);
        assert_eq!(contain(|| -> i32 { panic!("test callback panic") }), 0);
    }

    #[test]
    fn panic_payload_destructor_cannot_escape_callback() {
        struct BadDrop;
        impl Drop for BadDrop {
            fn drop(&mut self) {
                panic!("payload drop")
            }
        }
        assert!(!contain(|| -> bool { std::panic::panic_any(BadDrop) }));
    }
}
