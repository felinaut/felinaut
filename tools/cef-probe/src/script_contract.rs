//! Actual PageBackend evaluation checks; callbacks only enqueue bounded results.

use felinaut_cef_probe::page::{Page, ScriptError, ScriptResult};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

pub struct Scripts {
    incoming: Vec<(Receiver<ScriptResult>, Expected)>,
    dispatch_failed_callback: Arc<AtomicBool>,
    completed: bool,
}

#[derive(Clone)]
enum Expected {
    Json(Value),
    Error(ScriptError),
}

impl Expected {
    fn matches(&self, result: &ScriptResult) -> bool {
        match (self, result) {
            (Self::Json(expected), Ok(text)) => serde_json::from_str::<Value>(text)
                .as_ref()
                .is_ok_and(|value| value == expected),
            (Self::Error(expected), Err(actual)) => expected == actual,
            _ => false,
        }
    }
}

impl Scripts {
    /// A context may still be binding immediately after load; the caller retries
    /// only this readiness request. Once accepted, every batch dispatch must work.
    pub fn ready(page: &Page) -> Result<Option<Self>, String> {
        Self::check(page, Expected::Json(json!(42)))
    }

    pub fn blocked(page: &Page) -> Result<Option<Self>, String> {
        Self::check(
            page,
            Expected::Error(ScriptError::Backend(
                "JavaScript evaluation threw an exception".into(),
            )),
        )
    }

    fn check(page: &Page, expected: Expected) -> Result<Option<Self>, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        if let Err(error) = page.evaluate(
            "42",
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        ) {
            if error == "CEF evaluation context is not bound" {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(Some(Self {
            incoming: vec![(receiver, expected)],
            dispatch_failed_callback: Arc::new(AtomicBool::new(false)),
            completed: false,
        }))
    }

    pub fn start(page: &Page) -> Result<Option<Self>, String> {
        let Some(mut scripts) = Self::ready(page)? else {
            return Ok(None);
        };
        let dispatch_failed_callback = scripts.dispatch_failed_callback.clone();
        let delivered = dispatch_failed_callback.clone();
        if page
            .evaluate(
                &" ".repeat(1024 * 1024 + 1),
                Box::new(move |_| {
                    delivered.store(true, Ordering::Release);
                }),
            )
            .is_ok()
        {
            return Err("Oversized CEF source was accepted".into());
        }

        let serialization = || {
            Expected::Error(ScriptError::Backend(
                "JavaScript result is not serializable JSON".into(),
            ))
        };
        for (source, expected) in [
            (
                "({answer:42,text:'meow'})",
                Expected::Json(json!({"answer":42,"text":"meow"})),
            ),
            (
                "'{\"wrapped\":true}'",
                Expected::Json(json!("{\"wrapped\":true}")),
            ),
            ("undefined", Expected::Json(Value::Null)),
            ("", Expected::Json(Value::Null)),
            (
                "throw new Error('fixture')",
                Expected::Error(ScriptError::Backend(
                    "JavaScript evaluation threw an exception".into(),
                )),
            ),
            ("(()=>{const a={};a.self=a;return a})()", serialization()),
            ("1n", serialization()),
            ("Symbol()", serialization()),
            ("(()=>{})", serialization()),
            ("new Promise(()=>{})", serialization()),
            ("({toJSON(){throw 1}})", serialization()),
            (
                "(()=>{const d=new Date(0);Object.defineProperty(d,Symbol.toPrimitive,{value(){throw 1}});return d})()",
                serialization(),
            ),
            (
                "(()=>{JSON.stringify=()=> 'corrupted';return {captured:true}})()",
                Expected::Json(json!({"captured":true})),
            ),
            (
                "'x'.repeat(1048576)",
                Expected::Error(ScriptError::ResultTooLarge),
            ),
            ("42", Expected::Json(json!(42))),
        ] {
            scripts.push(page, source, expected)?;
        }
        // The source limit is inclusive and counted in UTF-8 bytes.
        scripts.push(page, &" ".repeat(1024 * 1024), Expected::Json(Value::Null))?;
        Ok(Some(scripts))
    }

    fn push(&mut self, page: &Page, source: &str, expected: Expected) -> Result<(), String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        page.evaluate(
            source,
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        )?;
        self.incoming.push((receiver, expected));
        Ok(())
    }

    pub fn cancellation(page: &Page, error: ScriptError) -> Result<Self, String> {
        let mut scripts = Self {
            incoming: Vec::new(),
            dispatch_failed_callback: Arc::new(AtomicBool::new(false)),
            completed: false,
        };
        scripts.push(page, "42", Expected::Error(error))?;
        Ok(scripts)
    }

    pub fn finished(&mut self) -> Result<bool, String> {
        if self.dispatch_failed_callback.load(Ordering::Acquire) {
            return Err("Rejected CEF dispatch invoked its callback".into());
        }
        if self.completed {
            return Ok(true);
        }
        let mut waiting = Vec::new();
        for (index, (receiver, expected)) in self.incoming.drain(..).enumerate() {
            match receiver.try_recv() {
                Ok(result) if expected.matches(&result) => {}
                Ok(result) => {
                    let outcome = match result {
                        Ok(text) => format!("JSON result, {} bytes", text.len()),
                        Err(error) => error.to_string(),
                    };
                    return Err(format!(
                        "CEF native script case {index} result mismatch: {outcome}"
                    ));
                }
                Err(TryRecvError::Empty) => waiting.push((receiver, expected)),
                Err(TryRecvError::Disconnected) => {
                    return Err("CEF accepted callback disappeared".into());
                }
            }
        }
        self.incoming = waiting;
        self.completed = self.incoming.is_empty();
        Ok(self.completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_require_typed_errors_and_preserve_json_strings() {
        assert!(Expected::Json(json!(42)).matches(&Ok("42".into())));
        assert!(!Expected::Json(json!(42)).matches(&Ok("\"42\"".into())));
        assert!(
            Expected::Json(json!("{\"wrapped\":true}"))
                .matches(&Ok("\"{\\\"wrapped\\\":true}\"".into()))
        );
        assert!(!Expected::Json(Value::Null).matches(&Err(ScriptError::DocumentChanged)));
        assert!(!Expected::Json(Value::Null).matches(&Ok("not json".into())));
        assert!(
            Expected::Error(ScriptError::DocumentChanged)
                .matches(&Err(ScriptError::DocumentChanged))
        );
        assert!(
            !Expected::Error(ScriptError::DocumentChanged).matches(&Err(ScriptError::PageClosed))
        );
    }

    #[test]
    fn pending_callbacks_wait_and_missing_or_rejected_callbacks_fail() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut scripts = Scripts {
            incoming: vec![(receiver, Expected::Json(Value::Null))],
            dispatch_failed_callback: Arc::new(AtomicBool::new(false)),
            completed: false,
        };
        assert!(!scripts.finished().unwrap());
        sender.send(Ok("null".into())).unwrap();
        assert!(scripts.finished().unwrap());
        scripts
            .dispatch_failed_callback
            .store(true, Ordering::Release);
        assert!(scripts.finished().is_err());
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut scripts = Scripts {
            incoming: vec![(receiver, Expected::Json(Value::Null))],
            dispatch_failed_callback: Arc::new(AtomicBool::new(false)),
            completed: false,
        };
        drop(sender);
        assert!(scripts.finished().is_err());
    }
}
