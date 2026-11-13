//! Native-only evaluation wire contract, independent of CEF and browser UI types.
//!
//! Browser users must additionally authenticate the callback's native browser/frame,
//! bind replies to their current document scope, expire pending delivery, and parse
//! successful payloads as JSON. Wire IDs are correlation handles, not authority.
//! Deadlines cancel delivery only: CEF evaluation/stringification is not interruptible.

pub const SOURCE_LIMIT: usize = 1024 * 1024;
pub const RESULT_LIMIT: usize = 1024 * 1024;
pub const ERROR_LIMIT: usize = 4096;
pub const FRAME_LIMIT: usize = 1024;
pub const CONTEXT_LIMIT: usize = 64;
pub const ID_LENGTH: usize = 16;
pub const NAME_LIMIT: usize = 64;

pub const HINT: &str = "felinaut.eval.context-hint.v1";
pub const BIND: &str = "felinaut.eval.bind.v1";
pub const BOUND: &str = "felinaut.eval.bound.v1";
pub const RUN: &str = "felinaut.eval.run.v1";
pub const DONE: &str = "felinaut.eval.done.v1";
pub const RELEASED: &str = "felinaut.eval.released.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id(u64);

impl Id {
    pub fn new(value: u64) -> Option<Self> {
        (value != 0).then_some(Self(value))
    }

    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        if value.len() != ID_LENGTH
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(ProtocolError::InvalidId);
        }
        u64::from_str_radix(value, 16)
            .ok()
            .and_then(Self::new)
            .ok_or(ProtocolError::InvalidId)
    }

    pub fn wire(self) -> String {
        format!("{:016x}", self.0)
    }
}

#[derive(Default)]
pub struct Sequence(u64);

impl Sequence {
    pub fn allocate(&mut self) -> Option<Id> {
        self.0 = self.0.checked_add(1)?;
        Id::new(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Status {
    Ok = 0,
    NoContext = 1,
    StaleContext = 2,
    JavascriptException = 3,
    SerializationError = 4,
    ResultTooLarge = 5,
    InternalError = 6,
    Capacity = 7,
}

impl Status {
    pub fn from_wire(value: i32) -> Result<Self, ProtocolError> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::NoContext),
            2 => Ok(Self::StaleContext),
            3 => Ok(Self::JavascriptException),
            4 => Ok(Self::SerializationError),
            5 => Ok(Self::ResultTooLarge),
            6 => Ok(Self::InternalError),
            7 => Ok(Self::Capacity),
            _ => Err(ProtocolError::InvalidStatus),
        }
    }

    pub fn diagnostic(self) -> &'static str {
        match self {
            Self::Ok => "",
            Self::NoContext => "JavaScript context unavailable",
            Self::StaleContext => "JavaScript context changed",
            Self::JavascriptException => "JavaScript evaluation threw an exception",
            Self::SerializationError => "JavaScript result is not serializable JSON",
            Self::ResultTooLarge => "JavaScript result exceeds the byte limit",
            Self::InternalError => "Native JavaScript evaluation failed",
            Self::Capacity => "Native JavaScript evaluation capacity exceeded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldSpec {
    String(usize),
    Int,
}

/// Exact flat schemas. The native decoder must check these before copying strings.
pub fn schema(name: &str) -> Option<&'static [FieldSpec]> {
    use FieldSpec::{Int, String as Text};
    match name {
        HINT => Some(&[]),
        BIND => Some(&[Text(ID_LENGTH)]),
        BOUND => Some(&[Text(ID_LENGTH), Text(ID_LENGTH), Int]),
        RUN => Some(&[
            Text(ID_LENGTH),
            Text(ID_LENGTH),
            Text(ID_LENGTH),
            Text(SOURCE_LIMIT),
        ]),
        DONE => Some(&[
            Text(ID_LENGTH),
            Text(ID_LENGTH),
            Text(ID_LENGTH),
            Int,
            Text(RESULT_LIMIT),
        ]),
        RELEASED => Some(&[Text(ID_LENGTH), Text(ID_LENGTH)]),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Field {
    String(String),
    Int(i32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    UnknownMessage,
    InvalidShape,
    TooLarge,
    InvalidId,
    InvalidStatus,
    InvalidPayload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    ContextHint,
    Bind {
        binding: Id,
    },
    /// Only Ok has a context. Failure never installs a binding.
    Bound {
        binding: Id,
        context: Option<Id>,
        status: Status,
    },
    Run {
        binding: Id,
        context: Id,
        request: Id,
        source: String,
    },
    Done {
        binding: Id,
        context: Id,
        request: Id,
        status: Status,
        payload: String,
    },
    Released {
        binding: Id,
        context: Id,
    },
}

impl Message {
    pub fn decode(name: &str, fields: Vec<Field>) -> Result<Self, ProtocolError> {
        validate_wire(name, &fields)?;
        let mut fields = fields.into_iter();
        let message = match name {
            HINT => Self::ContextHint,
            BIND => Self::Bind {
                binding: take_id(&mut fields)?,
            },
            BOUND => {
                let binding = take_id(&mut fields)?;
                let context = take_string(&mut fields)?;
                let context = if context.is_empty() {
                    None
                } else {
                    Some(Id::parse(&context)?)
                };
                let status = take_status(&mut fields)?;
                Self::Bound {
                    binding,
                    context,
                    status,
                }
            }
            RUN => Self::Run {
                binding: take_id(&mut fields)?,
                context: take_id(&mut fields)?,
                request: take_id(&mut fields)?,
                source: take_string(&mut fields)?,
            },
            DONE => {
                let binding = take_id(&mut fields)?;
                let context = take_id(&mut fields)?;
                let request = take_id(&mut fields)?;
                let status = take_status(&mut fields)?;
                let payload = take_string(&mut fields)?;
                Self::Done {
                    binding,
                    context,
                    request,
                    status,
                    payload,
                }
            }
            RELEASED => Self::Released {
                binding: take_id(&mut fields)?,
                context: take_id(&mut fields)?,
            },
            _ => return Err(ProtocolError::UnknownMessage),
        };
        Ok(message)
    }

    /// Also validates locally constructed messages before they enter native IPC.
    pub fn into_wire(self) -> Result<(&'static str, Vec<Field>), ProtocolError> {
        use Field::{Int, String as Text};
        let (name, fields) = match self {
            Self::ContextHint => (HINT, vec![]),
            Self::Bind { binding } => (BIND, vec![Text(binding.wire())]),
            Self::Bound {
                binding,
                context,
                status,
            } => (
                BOUND,
                vec![
                    Text(binding.wire()),
                    Text(context.map(Id::wire).unwrap_or_default()),
                    Int(status as i32),
                ],
            ),
            Self::Run {
                binding,
                context,
                request,
                source,
            } => (
                RUN,
                vec![
                    Text(binding.wire()),
                    Text(context.wire()),
                    Text(request.wire()),
                    Text(source),
                ],
            ),
            Self::Done {
                binding,
                context,
                request,
                status,
                payload,
            } => (
                DONE,
                vec![
                    Text(binding.wire()),
                    Text(context.wire()),
                    Text(request.wire()),
                    Int(status as i32),
                    Text(payload),
                ],
            ),
            Self::Released { binding, context } => {
                (RELEASED, vec![Text(binding.wire()), Text(context.wire())])
            }
        };
        // Validate by borrowing: do not duplicate potentially megabyte payloads.
        validate_wire(name, &fields)?;
        Ok((name, fields))
    }
}

fn validate_wire(name: &str, fields: &[Field]) -> Result<(), ProtocolError> {
    let specs = schema(name).ok_or(ProtocolError::UnknownMessage)?;
    if specs.len() != fields.len() {
        return Err(ProtocolError::InvalidShape);
    }
    for (field, spec) in fields.iter().zip(specs) {
        match (field, spec) {
            (Field::String(value), FieldSpec::String(limit)) if value.len() <= *limit => {}
            (Field::Int(_), FieldSpec::Int) => {}
            (Field::String(_), FieldSpec::String(_)) => return Err(ProtocolError::TooLarge),
            _ => return Err(ProtocolError::InvalidShape),
        }
    }
    if name == BOUND {
        let Field::String(context) = &fields[1] else {
            return Err(ProtocolError::InvalidShape);
        };
        let Field::Int(status) = fields[2] else {
            return Err(ProtocolError::InvalidShape);
        };
        let status = Status::from_wire(status)?;
        if (status == Status::Ok) == context.is_empty()
            || !matches!(
                status,
                Status::Ok
                    | Status::NoContext
                    | Status::StaleContext
                    | Status::Capacity
                    | Status::InternalError
            )
        {
            return Err(ProtocolError::InvalidStatus);
        }
    }
    if name == DONE {
        let Field::Int(status) = fields[3] else {
            return Err(ProtocolError::InvalidShape);
        };
        let Field::String(payload) = &fields[4] else {
            return Err(ProtocolError::InvalidShape);
        };
        let status = Status::from_wire(status)?;
        if payload.is_empty() || (status != Status::Ok && payload.len() > ERROR_LIMIT) {
            return Err(ProtocolError::InvalidPayload);
        }
    }
    Ok(())
}

fn take_string(fields: &mut impl Iterator<Item = Field>) -> Result<String, ProtocolError> {
    match fields.next() {
        Some(Field::String(value)) => Ok(value),
        _ => Err(ProtocolError::InvalidShape),
    }
}
fn take_id(fields: &mut impl Iterator<Item = Field>) -> Result<Id, ProtocolError> {
    Id::parse(&take_string(fields)?)
}
fn take_status(fields: &mut impl Iterator<Item = Field>) -> Result<Status, ProtocolError> {
    match fields.next() {
        Some(Field::Int(value)) => Status::from_wire(value),
        _ => Err(ProtocolError::InvalidShape),
    }
}

/// Bounded UTF-16 conversion shared by native string readers and offline tests.
/// Reject malformed UTF-16 rather than silently changing an ID or script.
pub fn bounded_utf16(units: &[u16], limit: usize) -> Result<String, ProtocolError> {
    if units.len() > limit {
        return Err(ProtocolError::TooLarge);
    }
    let mut result = String::new();
    for decoded in char::decode_utf16(units.iter().copied()) {
        let c = decoded.map_err(|_| ProtocolError::InvalidPayload)?;
        if result.len().saturating_add(c.len_utf8()) > limit {
            return Err(ProtocolError::TooLarge);
        }
        result.push(c);
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Execute,
    Replay,
    Reject(Status),
}

/// Native identity checks happen before calling this pure state machine. A browser
/// binding ID must increase across rebindings; retries of the active binding are
/// idempotent and never reset replay protection. Context IDs are never recycled.
pub struct ContextState {
    context: Id,
    live: bool,
    binding: Option<Id>,
    request: Option<Id>,
    running: bool,
}

impl ContextState {
    pub fn new(context: Id) -> Self {
        Self {
            context,
            live: true,
            binding: None,
            request: None,
            running: false,
        }
    }

    pub fn context(&self) -> Id {
        self.context
    }
    pub fn binding(&self) -> Option<Id> {
        self.live.then_some(self.binding).flatten()
    }

    pub fn bind(&mut self, binding: Id) -> Result<(), Status> {
        if !self.live || self.binding.is_some_and(|old| binding < old) {
            return Err(Status::StaleContext);
        }
        if self.running {
            return Err(Status::Capacity);
        }
        if self.binding != Some(binding) {
            self.binding = Some(binding);
            self.request = None;
        }
        Ok(())
    }

    pub fn begin(&mut self, binding: Id, context: Id, request: Id) -> Admission {
        if !self.live || self.context != context || self.binding != Some(binding) {
            return Admission::Reject(Status::StaleContext);
        }
        if self.request.is_some_and(|previous| request <= previous) {
            return Admission::Replay;
        }
        if self.running {
            return Admission::Reject(Status::Capacity);
        }
        self.request = Some(request);
        self.running = true;
        Admission::Execute
    }

    pub fn finish(&mut self, binding: Id, context: Id, request: Id) -> bool {
        if self.live
            && self.binding == Some(binding)
            && self.context == context
            && self.request == Some(request)
            && self.running
        {
            self.running = false;
            true
        } else {
            false
        }
    }

    pub fn release(&mut self) -> Option<(Id, Id)> {
        let binding = self.binding();
        self.live = false;
        self.running = false;
        self.binding = None;
        binding.map(|binding| (binding, self.context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u64) -> Id {
        Id::new(n).unwrap()
    }
    fn text(s: &str) -> Field {
        Field::String(s.into())
    }
    fn roundtrip(message: Message) {
        let (name, fields) = message.clone().into_wire().unwrap();
        let decoded = Message::decode(name, fields).unwrap();
        assert_eq!(decoded, message);
        let (next_name, next_fields) = decoded.into_wire().unwrap();
        assert_eq!(name, next_name);
        assert!(Message::decode(next_name, next_fields).is_ok());
    }

    #[test]
    fn all_messages_round_trip() {
        roundtrip(Message::ContextHint);
        roundtrip(Message::Bind { binding: id(1) });
        roundtrip(Message::Bound {
            binding: id(1),
            context: Some(id(2)),
            status: Status::Ok,
        });
        roundtrip(Message::Bound {
            binding: id(1),
            context: None,
            status: Status::NoContext,
        });
        roundtrip(Message::Run {
            binding: id(1),
            context: id(2),
            request: id(3),
            source: String::new(),
        });
        roundtrip(Message::Done {
            binding: id(1),
            context: id(2),
            request: id(3),
            status: Status::Ok,
            payload: "\"🦊\"".into(),
        });
        roundtrip(Message::Released {
            binding: id(1),
            context: id(2),
        });
        for status in [
            Status::NoContext,
            Status::StaleContext,
            Status::JavascriptException,
            Status::SerializationError,
            Status::ResultTooLarge,
            Status::InternalError,
            Status::Capacity,
        ] {
            roundtrip(Message::Done {
                binding: id(1),
                context: id(2),
                request: id(3),
                status,
                payload: status.diagnostic().into(),
            });
        }
    }

    #[test]
    fn ids_are_canonical_nonzero_and_sequence_never_wraps() {
        for invalid in [
            "",
            "1",
            "0000000000000000",
            "FFFFFFFFFFFFFFFF",
            "000000000000000g",
            "00000000000000001",
            "-000000000000001",
        ] {
            assert_eq!(Id::parse(invalid), Err(ProtocolError::InvalidId));
        }
        assert_eq!(Id::parse(&id(u64::MAX).wire()), Ok(id(u64::MAX)));
        let mut sequence = Sequence::default();
        assert_eq!(sequence.allocate(), Some(id(1)));
        let mut sequence = Sequence(u64::MAX - 1);
        assert_eq!(sequence.allocate(), Some(id(u64::MAX)));
        assert_eq!(sequence.allocate(), None);
        assert_eq!(sequence.allocate(), None);
    }

    #[test]
    fn rejects_unknown_extra_missing_and_wrongly_typed_fields() {
        assert_eq!(
            Message::decode("other", vec![]),
            Err(ProtocolError::UnknownMessage)
        );
        assert_eq!(
            Message::decode(HINT, vec![Field::Int(0)]),
            Err(ProtocolError::InvalidShape)
        );
        assert_eq!(
            Message::decode(BIND, vec![]),
            Err(ProtocolError::InvalidShape)
        );
        assert_eq!(
            Message::decode(BIND, vec![Field::Int(1)]),
            Err(ProtocolError::InvalidShape)
        );
        assert_eq!(
            Message::decode(BIND, vec![text("0000000000000000")]),
            Err(ProtocolError::InvalidId)
        );
        assert_eq!(
            Message::decode(
                BOUND,
                vec![text(&id(1).wire()), text(&id(2).wire()), Field::Int(99)]
            ),
            Err(ProtocolError::InvalidStatus)
        );
        assert_eq!(
            Message::decode(BOUND, vec![text(&id(1).wire()), text(""), Field::Int(0)]),
            Err(ProtocolError::InvalidStatus)
        );
        assert_eq!(
            Message::decode(
                BOUND,
                vec![text(&id(1).wire()), text(&id(2).wire()), Field::Int(1)]
            ),
            Err(ProtocolError::InvalidStatus)
        );
    }

    #[test]
    fn payload_bounds_count_utf8_and_apply_to_both_directions() {
        let run = |source| Message::Run {
            binding: id(1),
            context: id(2),
            request: id(3),
            source,
        };
        roundtrip(run("x".repeat(SOURCE_LIMIT)));
        assert_eq!(
            run("x".repeat(SOURCE_LIMIT + 1)).into_wire(),
            Err(ProtocolError::TooLarge)
        );
        let done = |status, payload| Message::Done {
            binding: id(1),
            context: id(2),
            request: id(3),
            status,
            payload,
        };
        roundtrip(done(Status::Ok, "x".repeat(RESULT_LIMIT)));
        assert_eq!(
            done(Status::Ok, "x".repeat(RESULT_LIMIT + 1)).into_wire(),
            Err(ProtocolError::TooLarge)
        );
        roundtrip(done(Status::InternalError, "x".repeat(ERROR_LIMIT)));
        assert_eq!(
            done(Status::InternalError, "x".repeat(ERROR_LIMIT + 1)).into_wire(),
            Err(ProtocolError::InvalidPayload)
        );
        assert_eq!(
            done(Status::Ok, String::new()).into_wire(),
            Err(ProtocolError::InvalidPayload)
        );
        let fields = vec![
            text(&id(1).wire()),
            text(&id(2).wire()),
            text(&id(3).wire()),
            text(&"é".repeat(SOURCE_LIMIT / 2 + 1)),
        ];
        assert_eq!(Message::decode(RUN, fields), Err(ProtocolError::TooLarge));
    }

    #[test]
    fn every_schema_rejects_wrong_types_and_extra_fields() {
        for name in [HINT, BIND, BOUND, RUN, DONE, RELEASED] {
            let specs = schema(name).unwrap();
            let fields = || {
                specs
                    .iter()
                    .map(|spec| match spec {
                        FieldSpec::String(_) => text(&id(1).wire()),
                        FieldSpec::Int => Field::Int(0),
                    })
                    .collect::<Vec<_>>()
            };
            let mut extra = fields();
            extra.push(Field::Int(0));
            assert_eq!(
                Message::decode(name, extra),
                Err(ProtocolError::InvalidShape)
            );
            for (index, spec) in specs.iter().enumerate() {
                let mut wrong = fields();
                wrong[index] = match spec {
                    FieldSpec::String(_) => Field::Int(0),
                    FieldSpec::Int => text("0"),
                };
                assert_eq!(
                    Message::decode(name, wrong),
                    Err(ProtocolError::InvalidShape)
                );
            }
        }
    }

    #[test]
    fn invalid_results_and_bound_statuses_fail_incoming_and_outgoing_validation() {
        for status in [
            Status::JavascriptException,
            Status::SerializationError,
            Status::ResultTooLarge,
        ] {
            assert_eq!(
                Message::Bound {
                    binding: id(1),
                    context: None,
                    status
                }
                .into_wire(),
                Err(ProtocolError::InvalidStatus)
            );
        }
        let result = |status, payload| {
            vec![
                text(&id(1).wire()),
                text(&id(2).wire()),
                text(&id(3).wire()),
                Field::Int(status),
                Field::String(payload),
            ]
        };
        assert_eq!(
            Message::decode(DONE, result(-1, "error".into())),
            Err(ProtocolError::InvalidStatus)
        );
        assert_eq!(
            Message::decode(DONE, result(0, String::new())),
            Err(ProtocolError::InvalidPayload)
        );
        assert_eq!(
            Message::decode(DONE, result(6, "x".repeat(ERROR_LIMIT + 1))),
            Err(ProtocolError::InvalidPayload)
        );
        assert_eq!(
            Message::decode(DONE, result(0, "x".repeat(RESULT_LIMIT + 1))),
            Err(ProtocolError::TooLarge)
        );
    }

    #[test]
    fn bounded_utf16_covers_empty_unicode_surrogates_and_exact_boundaries() {
        assert_eq!(bounded_utf16(&[], 0), Ok(String::new()));
        for (s, bytes) in [("a", 1), ("é", 2), ("🦊", 4)] {
            let units: Vec<_> = s.encode_utf16().collect();
            assert_eq!(bounded_utf16(&units, bytes), Ok(s.into()));
            assert_eq!(
                bounded_utf16(&units, bytes - 1),
                Err(ProtocolError::TooLarge)
            );
        }
        assert_eq!(
            bounded_utf16(&[0xd800], 10),
            Err(ProtocolError::InvalidPayload)
        );
        assert_eq!(
            bounded_utf16(&[0xdc00], 10),
            Err(ProtocolError::InvalidPayload)
        );
    }

    #[test]
    fn binding_retries_do_not_reset_replay_protection() {
        let mut state = ContextState::new(id(2));
        assert_eq!(
            state.begin(id(1), id(2), id(3)),
            Admission::Reject(Status::StaleContext)
        );
        assert_eq!(state.bind(id(1)), Ok(()));
        assert_eq!(state.begin(id(1), id(2), id(3)), Admission::Execute);
        assert_eq!(state.begin(id(1), id(2), id(3)), Admission::Replay);
        assert_eq!(
            state.begin(id(1), id(2), id(4)),
            Admission::Reject(Status::Capacity)
        );
        assert_eq!(state.bind(id(4)), Err(Status::Capacity));
        assert!(state.finish(id(1), id(2), id(3)));
        assert!(!state.finish(id(1), id(2), id(3)));
        assert_eq!(state.bind(id(1)), Ok(()));
        assert_eq!(state.begin(id(1), id(2), id(3)), Admission::Replay);
        assert_eq!(state.begin(id(1), id(2), id(4)), Admission::Execute);
    }

    #[test]
    fn late_release_of_old_context_does_not_cancel_replacement() {
        let mut old = ContextState::new(id(1));
        old.bind(id(2)).unwrap();
        assert_eq!(old.begin(id(2), id(1), id(3)), Admission::Execute);
        let mut replacement = ContextState::new(id(4));
        replacement.bind(id(5)).unwrap();
        assert_eq!(replacement.begin(id(5), id(4), id(6)), Admission::Execute);
        assert_eq!(old.release(), Some((id(2), id(1))));
        assert!(!old.finish(id(2), id(1), id(3)));
        assert!(replacement.finish(id(5), id(4), id(6)));
        assert_eq!(replacement.binding(), Some(id(5)));
    }

    #[test]
    fn rebinding_and_context_release_reject_stale_work() {
        let mut old = ContextState::new(id(2));
        old.bind(id(1)).unwrap();
        assert_eq!(old.begin(id(1), id(2), id(3)), Admission::Execute);
        assert_eq!(old.release(), Some((id(1), id(2))));
        assert_eq!(old.release(), None);
        assert!(!old.finish(id(1), id(2), id(3)));
        assert_eq!(old.bind(id(4)), Err(Status::StaleContext));
        let mut new = ContextState::new(id(5));
        new.bind(id(6)).unwrap();
        assert_eq!(new.bind(id(1)), Err(Status::StaleContext));
        assert_eq!(
            new.begin(id(6), id(2), id(7)),
            Admission::Reject(Status::StaleContext)
        );
        assert_eq!(
            new.begin(id(1), id(5), id(7)),
            Admission::Reject(Status::StaleContext)
        );
        assert_eq!(new.begin(id(6), id(5), id(7)), Admission::Execute);
        assert!(new.finish(id(6), id(5), id(7)));
        new.bind(id(8)).unwrap();
        assert_eq!(
            new.begin(id(6), id(5), id(9)),
            Admission::Reject(Status::StaleContext)
        );
        assert_eq!(new.begin(id(8), id(5), id(9)), Admission::Execute);
    }
}
