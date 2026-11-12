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
    time::Duration,
};

use serde_json::{Value, json};

use crate::webview::NativePage;

/// A command for the UI thread. The answer line goes to `reply`.
pub struct Request {
    pub command: Value,
    pub reply: Sender<String>,
}

/// The requests for the UI thread, and a sender that puts a follow-up step in the same queue.
pub type Queue = (Receiver<Request>, Sender<Request>);

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
            let sent = queue.send(Request { command, reply }).is_ok();
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

/// WebKit gives the JSON text of the string that `__felifox` returned, or nothing.
fn decode(result: &str) -> String {
    serde_json::from_str(result).unwrap_or_else(|_| {
        response(Err(
            "The page gave no result. It is possibly not loaded, or the code is not one valid \
             JavaScript expression."
                .into(),
        ))
    })
}

/// Runs a page command on the UI thread. The answer comes immediately, or later from a
/// WebKit callback.
pub fn run(page: &NativePage, request: Request, queue: &Sender<Request>) {
    let Request { command, reply } = request;
    let text = |key: &str| command[key].as_str().unwrap_or_default().to_owned();
    let native = command["native"] == true;
    // Gives the answer of the page to the client.
    let evaluate = |script: String| {
        let reply = reply.clone();
        page.evaluate(&script, move |result| {
            let _ = reply.send(decode(&result));
        })
        .map(|()| None)
    };
    // JavaScript finds or focuses the element. Then `next` runs as a native step, with the
    // result of the script as `point`.
    let then = |script: String, next: Value| {
        let (reply, queue) = (reply.clone(), queue.clone());
        page.evaluate(&script, move |result| {
            let answer = decode(&result);
            let parsed: Value = serde_json::from_str(&answer).unwrap_or_default();
            if parsed["ok"] == true {
                let mut command = next.clone();
                command["point"] = parsed["value"].clone();
                let _ = queue.send(Request {
                    command,
                    reply: reply.clone(),
                });
            } else {
                let _ = reply.send(answer);
            }
        })
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
            page.screenshot(move |png| {
                let saved = png.and_then(|bytes| {
                    fs::write(&path, bytes).map_err(|error| format!("{path}: {error}"))
                });
                let _ = reply.send(response(saved.map(|()| json!(path))));
            });
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
        for line in [
            "{\"token\":\"wrong\",\"cmd\":\"tabs\"}\n",
            "{\"cmd\":\"tabs\"}\n",
            "{\"token\":\"secret\",\"cmd\":\"_click\"}\n",
            "{\"token\":\"secret\"}\n",
            "not json\n",
        ] {
            assert!(ask(line).contains(r#""ok":false"#), "{line}");
        }
    }
}
