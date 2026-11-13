//! Opt-in control server for agents: `FELINAUT_AGENT=1` opens a Unix socket that drives the
//! active page. It is off by default, because each client can use the logged-in sessions.
//!
//! One request is one JSON line, `{"token": …, "cmd": …}`. One answer is one JSON line,
//! `{"ok": true, "value": …}` or `{"ok": false, "error": …}`. See `bin/felinaut-ctl.rs`.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::page::{PageBackend, RequestScope};

/// A command for the UI thread. The answer line goes to `reply`.
pub struct Request {
    pub command: Value,
    pub reply: Sender<String>,
    /// Trusted continuation identity; never populated from client JSON.
    pub(crate) scope: Option<RequestScope>,
    /// The original preflight deadline; never populated from client JSON.
    pub(crate) expires_at: Option<Instant>,
}

/// The requests for the UI thread, and a sender that puts a follow-up step in the same queue.
pub type Queue = (Receiver<Request>, Sender<Request>);

const STALE_CONTINUATION: &str = "Native input canceled: stale page or document.";
const CONTINUATION_BUDGET: Duration = Duration::from_secs(10);
const EXPIRED_CONTINUATION: &str = "Native input canceled: continuation deadline expired.";
const MISSING_CONTINUATION_BUDGET: &str = "Native input canceled: missing continuation deadline.";

fn check_continuation_deadline(expires_at: Option<Instant>, now: Instant) -> Result<(), String> {
    match expires_at {
        None => Err(MISSING_CONTINUATION_BUDGET.into()),
        Some(deadline) if now >= deadline => Err(EXPIRED_CONTINUATION.into()),
        Some(_) => Ok(()),
    }
}

pub fn response(result: Result<Value, String>) -> String {
    match result {
        Ok(value) => json!({"ok": true, "value": value}),
        Err(error) => json!({"ok": false, "error": error}),
    }
    .to_string()
}

/// `~/Library/Application Support/Felifox/agent.sock` and `agent.token`.
fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Felifox"))
}

/// Starts the listener thread when `FELINAUT_AGENT=1`.
pub fn start() -> Option<Queue> {
    if !crate::webview::agent_enabled() {
        return None;
    }
    match listen() {
        Ok(queue) => Some(queue),
        Err(error) => {
            eprintln!("Could not start the agent control server: {error}");
            None
        }
    }
}

fn listen() -> Result<Queue, String> {
    let dir = dir().ok_or("No home directory")?;
    let error = |error: std::io::Error| error.to_string();
    fs::create_dir_all(&dir).map_err(error)?;

    let mut random = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(error)?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    // A new file each time: an old file can have permissions that are too open.
    let token_path = dir.join("agent.token");
    let _ = fs::remove_file(&token_path);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&token_path)
        .and_then(|mut file| file.write_all(token.as_bytes()))
        .map_err(error)?;

    let socket = dir.join("agent.sock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(error)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).map_err(error)?;
    eprintln!("Agent control is on: {}", socket.display());

    let (sender, receiver) = mpsc::channel();
    let follow_up = sender.clone();
    // ponytail: one client at a time. Use a thread for each client if agents work in parallel.
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve(stream, &token, &sender);
        }
    });
    Ok((receiver, follow_up))
}

fn serve(stream: UnixStream, token: &str, queue: &Sender<Request>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new((&stream).take(1 << 20)).read_line(&mut line)?;
    let answer = match serde_json::from_str::<Value>(&line) {
        Err(error) => response(Err(format!("Invalid request: {error}"))),
        Ok(command) if command["token"].as_str() != Some(token) => {
            response(Err("Wrong token".into()))
        }
        // Names with `_` are the native follow-up steps. Only this process can send them.
        Ok(command)
            if command["cmd"]
                .as_str()
                .is_none_or(|cmd| cmd.starts_with('_')) =>
        {
            response(Err("Unknown command".into()))
        }
        Ok(command) => {
            let (reply, answer) = mpsc::channel();
            let sent = queue
                .send(Request {
                    command,
                    reply,
                    scope: None,
                    expires_at: None,
                })
                .is_ok();
            // A page that has not loaded drops the script, and thus the sender.
            match answer.recv_timeout(Duration::from_secs(15)) {
                Ok(answer) => answer,
                Err(_) if sent => response(Err(
                    "No answer from the page. It is possibly not loaded.".into(),
                )),
                Err(_) => response(Err("The browser window is closed.".into())),
            }
        }
    };
    (&stream).write_all(format!("{answer}\n").as_bytes())
}

/// The JavaScript call for `[method, argument…]`. JSON text is also a JavaScript expression.
fn call(arguments: Value) -> String {
    format!("__felifox.apply(null,{arguments})")
}

/// The page gives the JSON text of the string that `__felifox` returned, or nothing.
fn decode(result: &str) -> String {
    serde_json::from_str(result).unwrap_or_else(|_| {
        response(Err(
            "The page gave no result. It is possibly not loaded, or the code is not one valid \
             JavaScript expression."
                .into(),
        ))
    })
}

/// Runs a page command on the UI thread. The answer comes immediately, or later from an
/// engine callback.
pub fn run(page: &dyn PageBackend, request: Request, queue: &Sender<Request>) {
    let Request {
        command,
        reply,
        scope,
        expires_at,
    } = request;
    if let Some(scope) = scope {
        if !scope.is_current()
            || page
                .request_scope()
                .is_none_or(|current| !scope.matches(&current))
        {
            let _ = reply.send(response(Err(STALE_CONTINUATION.into())));
            return;
        }
        if let Err(error) = check_continuation_deadline(expires_at, Instant::now()) {
            let _ = reply.send(response(Err(error)));
            return;
        }
    }
    let text = |key: &str| command[key].as_str().unwrap_or_default().to_owned();
    let native = command["native"] == true;
    // Gives the answer of the page to the client.
    let evaluate = |script: String| {
        let reply = reply.clone();
        page.evaluate(
            &script,
            Box::new(move |result| {
                let answer = match result {
                    Ok(result) => decode(&result),
                    Err(error) => response(Err(error.to_string())),
                };
                let _ = reply.send(answer);
            }),
        )
        .map(|()| None)
    };
    // JavaScript finds or focuses the element. Then `next` runs as a native step, with the
    // result of the script as `point`.
    let then = |script: String, next: Value| {
        let scope = page
            .request_scope()
            .ok_or_else(|| page.unsupported("document-scoped native input"))?;
        if !scope.is_current() {
            return Err(STALE_CONTINUATION.into());
        }
        let expires_at = Instant::now()
            .checked_add(CONTINUATION_BUDGET)
            .ok_or("Native input canceled: continuation deadline exceeds clock capacity.")?;
        let (reply, queue) = (reply.clone(), queue.clone());
        page.evaluate(
            &script,
            Box::new(move |result| {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => {
                        let _ = reply.send(response(Err(error.to_string())));
                        return;
                    }
                };
                if !scope.is_current() {
                    let _ = reply.send(response(Err(STALE_CONTINUATION.into())));
                    return;
                }
                let answer = decode(&result);
                let parsed: Value = serde_json::from_str(&answer).unwrap_or_default();
                if parsed["ok"] == true {
                    if let Err(error) =
                        check_continuation_deadline(Some(expires_at), Instant::now())
                    {
                        let _ = reply.send(response(Err(error)));
                        return;
                    }
                    let mut command = next.clone();
                    command["point"] = parsed["value"].clone();
                    let _ = queue.send(Request {
                        command,
                        reply: reply.clone(),
                        scope: Some(scope.clone()),
                        expires_at: Some(expires_at),
                    });
                } else {
                    let _ = reply.send(answer);
                }
            }),
        )
        .map(|()| None)
    };
    // `None`: a callback gives the answer.
    let result: Result<Option<Value>, String> = match command["cmd"].as_str().unwrap_or_default() {
        "snapshot" => evaluate(call(json!(["snapshot"]))),
        "click" if native => then(
            call(json!(["point", command["ref"]])),
            json!({"cmd": "_click"}),
        ),
        "click" => evaluate(call(json!(["click", command["ref"]]))),
        "type" if native => then(
            call(json!(["focus", command["ref"]])),
            json!({"cmd": "_type", "text": command["text"]}),
        ),
        "type" => evaluate(call(json!(["type", command["ref"], command["text"]]))),
        "press" if native => page.native_key(&text("key")).map(|()| Some(Value::Null)),
        "press" => evaluate(call(json!(["press", command["key"]]))),
        // The code is in the script itself, because a page policy can forbid `eval`.
        "eval" => evaluate(format!("__felifox('eval',()=>({}\n))", text("code"))),
        "screenshot" => {
            let (reply, path) = (reply.clone(), text("path"));
            page.screenshot(Box::new(move |png| {
                let saved = png.and_then(|bytes| {
                    fs::write(&path, bytes).map_err(|error| format!("{path}: {error}"))
                });
                let _ = reply.send(response(saved.map(|()| json!(path))));
            }));
            Ok(None)
        }
        "_click" => {
            let point = &command["point"];
            match (point["x"].as_f64(), point["y"].as_f64()) {
                (Some(x), Some(y)) => page.native_click(x, y).map(|()| Some(Value::Null)),
                _ => Err("The page gave no position.".into()),
            }
        }
        "_type" => page.native_text(&text("text")).map(|()| Some(Value::Null)),
        other => Err(format!("Unknown command: {other}")),
    };
    match result {
        Ok(None) => {}
        Ok(Some(value)) => drop(reply.send(response(Ok(value)))),
        Err(error) => drop(reply.send(response(Err(error)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::{
        BackendInfo, Capabilities, Event, RuntimeIdentity, ScriptCallback, ScriptError,
        ScriptRequests,
    };
    use gpui::{Bounds, Pixels};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct FakePage {
        requests: ScriptRequests,
        without_scope: Cell<bool>,
        dispatch_error: Cell<bool>,
        callback: RefCell<Option<ScriptCallback>>,
        scripts: RefCell<Vec<String>>,
        input: RefCell<Vec<Value>>,
    }

    impl FakePage {
        fn complete(&self, result: Result<String, ScriptError>) {
            let callback = self.callback.borrow_mut().take().expect("pending script");
            callback(result);
        }
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
        fn navigate(&self, _: &str) -> Result<(), String> {
            self.requests.advance_document();
            Ok(())
        }
        fn back(&self) -> Result<(), String> {
            self.requests.advance_document();
            Ok(())
        }
        fn forward(&self) -> Result<(), String> {
            self.requests.advance_document();
            Ok(())
        }
        fn reload(&self) -> Result<(), String> {
            self.requests.advance_document();
            Ok(())
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
            None
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
            Vec::new()
        }
        fn request_scope(&self) -> Option<RequestScope> {
            (!self.without_scope.get()).then(|| self.requests.scope())
        }
        fn evaluate(&self, script: &str, callback: ScriptCallback) -> Result<(), String> {
            self.scripts.borrow_mut().push(script.into());
            if self.dispatch_error.get() {
                return Err("script dispatch failed".into());
            }
            assert!(self.callback.borrow_mut().replace(callback).is_none());
            Ok(())
        }
        fn native_click(&self, x: f64, y: f64) -> Result<(), String> {
            self.input.borrow_mut().push(json!(["click", x, y]));
            Ok(())
        }
        fn native_text(&self, text: &str) -> Result<(), String> {
            self.input.borrow_mut().push(json!(["type", text]));
            Ok(())
        }
        fn native_key(&self, key: &str) -> Result<(), String> {
            self.input.borrow_mut().push(json!(["press", key]));
            Ok(())
        }
    }

    fn native_commands() -> [Value; 2] {
        [
            json!({"cmd": "click", "native": true, "ref": "@e1"}),
            json!({"cmd": "type", "native": true, "ref": "@e1", "text": "hello"}),
        ]
    }

    fn start_command(
        page: &dyn PageBackend,
        command: Value,
        queue: &Sender<Request>,
    ) -> Receiver<String> {
        let (reply, answer) = mpsc::channel();
        run(
            page,
            Request {
                command,
                reply,
                scope: None,
                expires_at: None,
            },
            queue,
        );
        answer
    }

    fn script_success() -> String {
        serde_json::to_string(&response(Ok(json!({"x": 12.5, "y": 24.0})))).unwrap()
    }

    fn assert_error(answer: &Receiver<String>, error: &str) {
        let answer: Value = serde_json::from_str(&answer.try_recv().unwrap()).unwrap();
        assert_eq!(answer, json!({"ok": false, "error": error}));
    }

    #[test]
    fn native_continuations_run_on_the_same_page_and_document() {
        for command in native_commands() {
            let page = FakePage::default();
            let (queue, pending) = mpsc::channel();
            let before_preflight = Instant::now();
            let answer = start_command(&page, command.clone(), &queue);
            let after_preflight = Instant::now();
            assert!(page.input.borrow().is_empty());
            assert!(pending.try_recv().is_err());
            assert!(answer.try_recv().is_err());
            page.complete(Ok(script_success()));
            let continuation = pending.try_recv().unwrap();
            let deadline = continuation.expires_at.expect("continuation deadline");
            assert!(deadline >= before_preflight + CONTINUATION_BUDGET);
            assert!(deadline <= after_preflight + CONTINUATION_BUDGET);
            assert!(
                continuation
                    .scope
                    .as_ref()
                    .unwrap()
                    .matches(&page.requests.scope())
            );
            assert!(page.input.borrow().is_empty());
            run(&page, continuation, &queue);
            let expected = if command["cmd"] == "click" {
                json!(["click", 12.5, 24.0])
            } else {
                json!(["type", "hello"])
            };
            assert_eq!(*page.input.borrow(), vec![expected]);
            assert_eq!(answer.try_recv().unwrap(), response(Ok(Value::Null)));
            assert!(answer.try_recv().is_err());
            assert!(pending.try_recv().is_err());
        }
    }

    #[test]
    fn queued_native_continuations_reject_expired_or_missing_budgets() {
        for command in native_commands() {
            for expires_at in [
                Some(Instant::now().checked_sub(Duration::from_secs(1)).unwrap()),
                None,
            ] {
                let page = FakePage::default();
                let (queue, pending) = mpsc::channel();
                let answer = start_command(&page, command.clone(), &queue);
                page.complete(Ok(script_success()));
                let mut continuation = pending.try_recv().unwrap();
                assert!(
                    continuation
                        .scope
                        .as_ref()
                        .unwrap()
                        .matches(&page.requests.scope())
                );
                continuation.expires_at = expires_at;
                run(&page, continuation, &queue);
                assert_error(
                    &answer,
                    if expires_at.is_some() {
                        EXPIRED_CONTINUATION
                    } else {
                        MISSING_CONTINUATION_BUDGET
                    },
                );
                assert!(page.input.borrow().is_empty());
                assert!(pending.try_recv().is_err());
            }
        }
    }

    #[test]
    fn continuation_deadlines_expire_at_the_exact_boundary() {
        let now = Instant::now();
        assert_eq!(
            check_continuation_deadline(Some(now), now),
            Err(EXPIRED_CONTINUATION.into())
        );
        assert_eq!(
            check_continuation_deadline(Some(now + CONTINUATION_BUDGET), now),
            Ok(())
        );
        assert_eq!(
            check_continuation_deadline(None, now),
            Err(MISSING_CONTINUATION_BUDGET.into())
        );
    }

    #[test]
    fn queued_native_continuations_cannot_target_another_page() {
        for command in native_commands() {
            for without_scope in [false, true] {
                let page_a = FakePage::default();
                let page_b = FakePage::default();
                page_b.without_scope.set(without_scope);
                let (queue, pending) = mpsc::channel();
                let answer = start_command(&page_a, command.clone(), &queue);
                page_a.complete(Ok(script_success()));
                run(&page_b, pending.try_recv().unwrap(), &queue);
                assert_error(&answer, STALE_CONTINUATION);
                assert!(page_a.input.borrow().is_empty());
                assert!(page_b.input.borrow().is_empty());
                assert!(page_b.scripts.borrow().is_empty());
            }
        }
    }

    #[test]
    fn navigation_after_queueing_cancels_native_input() {
        for command in native_commands() {
            let page = FakePage::default();
            let (queue, pending) = mpsc::channel();
            let answer = start_command(&page, command, &queue);
            page.complete(Ok(script_success()));
            page.navigate("https://fixture.test/next").unwrap();
            run(&page, pending.try_recv().unwrap(), &queue);
            assert_error(&answer, STALE_CONTINUATION);
            assert!(page.input.borrow().is_empty());
        }
    }

    #[test]
    fn navigation_before_callback_does_not_enqueue_native_input() {
        for command in native_commands() {
            let page = FakePage::default();
            let (queue, pending) = mpsc::channel();
            let answer = start_command(&page, command, &queue);
            page.navigate("https://fixture.test/next").unwrap();
            page.complete(Ok(script_success()));
            assert_error(&answer, STALE_CONTINUATION);
            assert!(pending.try_recv().is_err());
            assert!(page.input.borrow().is_empty());
        }
    }

    #[test]
    fn failed_or_canceled_scripts_never_enqueue_native_input() {
        for command in native_commands() {
            for error in [
                ScriptError::Backend(r#"{"ok":true,"value":{"x":1,"y":2}}"#.into()),
                ScriptError::DocumentChanged,
                ScriptError::PageClosed,
                ScriptError::TimedOut,
                ScriptError::Discarded,
            ] {
                let page = FakePage::default();
                let (queue, pending) = mpsc::channel();
                let answer = start_command(&page, command.clone(), &queue);
                page.complete(Err(error.clone()));
                assert_error(&answer, &error.to_string());
                assert!(pending.try_recv().is_err());
                assert!(page.input.borrow().is_empty());
            }
        }
    }

    #[test]
    fn ordinary_script_commands_preserve_callback_errors() {
        for command in [
            json!({"cmd": "snapshot"}),
            json!({"cmd": "click", "ref": "@e1"}),
            json!({"cmd": "type", "ref": "@e1", "text": "hello"}),
            json!({"cmd": "press", "key": "Enter"}),
            json!({"cmd": "eval", "code": "null"}),
        ] {
            let page = FakePage::default();
            let (queue, pending) = mpsc::channel();
            let answer = start_command(&page, command, &queue);
            let error = ScriptError::Backend(r#"{"ok":true,"value":"not a result"}"#.into());
            page.complete(Err(error.clone()));
            assert_error(&answer, &error.to_string());
            assert!(pending.try_recv().is_err());
            assert!(page.input.borrow().is_empty());
        }
    }

    #[test]
    fn unsuccessful_or_empty_script_results_do_not_enqueue_native_input() {
        for command in native_commands() {
            for result in [
                serde_json::to_string(&response(Err("element not found".into()))).unwrap(),
                String::new(),
                "null".into(),
            ] {
                let page = FakePage::default();
                let (queue, pending) = mpsc::channel();
                let answer = start_command(&page, command.clone(), &queue);
                let expected = decode(&result);
                page.complete(Ok(result));
                assert_eq!(answer.try_recv().unwrap(), expected);
                assert!(expected.contains(r#""ok":false"#));
                assert!(pending.try_recv().is_err());
                assert!(page.input.borrow().is_empty());
            }
        }
    }

    #[test]
    fn native_preflight_requires_a_current_scope_without_fallback() {
        for command in native_commands() {
            for without_scope in [false, true] {
                let page = FakePage::default();
                page.without_scope.set(without_scope);
                if !without_scope {
                    page.requests.close();
                }
                let (queue, pending) = mpsc::channel();
                let answer = start_command(&page, command.clone(), &queue);
                let expected = if without_scope {
                    page.unsupported("document-scoped native input")
                } else {
                    STALE_CONTINUATION.into()
                };
                assert_error(&answer, &expected);
                assert!(page.scripts.borrow().is_empty());
                assert!(page.callback.borrow().is_none());
                assert!(page.input.borrow().is_empty());
                assert!(pending.try_recv().is_err());
            }
        }
    }

    #[test]
    fn native_preflight_dispatch_errors_are_returned_without_continuations() {
        for command in native_commands() {
            let page = FakePage::default();
            page.dispatch_error.set(true);
            let (queue, pending) = mpsc::channel();
            let answer = start_command(&page, command, &queue);
            assert_error(&answer, "script dispatch failed");
            assert!(page.callback.borrow().is_none());
            assert!(page.input.borrow().is_empty());
            assert!(pending.try_recv().is_err());
        }
    }

    #[test]
    fn native_keys_do_not_require_script_scopes() {
        let page = FakePage::default();
        page.without_scope.set(true);
        let (queue, pending) = mpsc::channel();
        let answer = start_command(
            &page,
            json!({"cmd": "press", "native": true, "key": "Enter", "expires_at": 0}),
            &queue,
        );
        assert_eq!(answer.try_recv().unwrap(), response(Ok(Value::Null)));
        assert_eq!(*page.input.borrow(), vec![json!(["press", "Enter"])]);
        assert!(page.scripts.borrow().is_empty());
        assert!(pending.try_recv().is_err());
    }

    #[test]
    fn answers_and_scripts_are_single_json_lines() {
        assert_eq!(
            response(Ok(json!(["a\nb"]))),
            r#"{"ok":true,"value":["a\nb"]}"#
        );
        assert_eq!(response(Err("no".into())), r#"{"ok":false,"error":"no"}"#);
        // Text from a client stays inside its string literal.
        assert_eq!(
            call(json!(["type", "@e1", "\");alert(1);(\""])),
            r#"__felifox.apply(null,["type","@e1","\");alert(1);(\""])"#
        );
        assert_eq!(decode(r#""{\"ok\":true}""#), r#"{"ok":true}"#);
        assert!(decode("").contains(r#""ok":false"#));
    }

    #[test]
    fn socket_rejects_wrong_tokens_and_internal_commands() {
        let ask = |line: &str| {
            let (client, server) = UnixStream::pair().unwrap();
            let (queue, requests) = mpsc::channel::<Request>();
            (&client).write_all(line.as_bytes()).unwrap();
            let answering = std::thread::spawn(move || {
                if let Ok(request) = requests.recv_timeout(Duration::from_secs(1)) {
                    assert!(request.scope.is_none());
                    assert!(request.expires_at.is_none());
                    let _ = request
                        .reply
                        .send(response(Ok(request.command["cmd"].clone())));
                }
            });
            serve(server, "secret", &queue).unwrap();
            drop(queue);
            answering.join().unwrap();
            let mut answer = String::new();
            BufReader::new(client).read_line(&mut answer).unwrap();
            answer
        };
        assert!(ask("{\"token\":\"secret\",\"cmd\":\"tabs\"}\n").contains(r#""value":"tabs""#));
        assert!(
            ask("{\"token\":\"secret\",\"cmd\":\"tabs\",\"scope\":{\"epoch\":0}}\n")
                .contains(r#""value":"tabs""#)
        );
        for line in [
            "{\"token\":\"secret\",\"cmd\":\"tabs\",\"expires_at\":0}\n",
            "{\"token\":\"secret\",\"cmd\":\"tabs\",\"expires_at\":{\"secs\":9999999999}}\n",
        ] {
            assert!(ask(line).contains(r#""value":"tabs""#), "{line}");
        }
        for line in [
            "{\"token\":\"wrong\",\"cmd\":\"tabs\"}\n",
            "{\"cmd\":\"tabs\"}\n",
            "{\"token\":\"secret\",\"cmd\":\"_click\"}\n",
            "{\"token\":\"secret\",\"cmd\":\"_type\",\"scope\":{\"epoch\":0}}\n",
            "{\"token\":\"secret\"}\n",
            "not json\n",
        ] {
            assert!(ask(line).contains(r#""ok":false"#), "{line}");
        }
    }
}
