//! Asana: your incomplete tasks, and those due within a week, from Asana's own API. It answers
//! the web session's cookie on a top-level load, but its pages block `fetch`, so the hidden
//! view loads `users/me` for the first workspace and traverses at most eight task pages.
//! The extractor emits no partial lists: errors use the host's `{error: string}` protocol.
//! Transient `window.name` state is cleared on completion/error; nothing is persisted.

use super::{Source, Spec};

const ME: &str = "https://app.asana.com/api/1.0/users/me?opt_fields=workspaces";

macro_rules! tasks {
    ($list:literal) => {
        concat!(
            "(() => { return (",
            include_str!("asana_extract.js"),
            ")('",
            $list,
            "'); })()"
        )
    };
}

pub static SPEC: Spec = Spec {
    key: "asana",
    name: "Asana",
    noun: "tasks",
    summary: "Your tasks, and those due this week",
    icon: crate::brand::icon::ASANA,
    host: "app.asana.com",
    sign_in_url: "https://app.asana.com/-/login",
    sign_in_paths: &["/-/login"],
    sources: &[
        Source {
            key: "due",
            label: "Due this week",
            url: ME,
            // Overdue too; soonest first.
            extractor: tasks!("due"),
        },
        Source {
            key: "mine",
            label: "My tasks",
            url: ME,
            extractor: tasks!("mine"),
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::SPEC;
    use std::{
        io::{Read, Write},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn offline_pagination_and_errors() {
        // JXA uses the system JavaScriptCore, with mocked pages and no network or UI.
        // All inputs are embedded fixtures; bound both the input and process lifetime.
        let scripts: Vec<_> = SPEC.sources.iter().map(|source| source.extractor).collect();
        let input = format!(
            "({})({})",
            include_str!("asana_fixtures.js"),
            serde_json::to_string(&scripts).unwrap()
        );
        assert!(input.len() <= 256 * 1024);
        let mut child = Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("offline fixtures require macOS /usr/bin/osascript");
        let mut stdin = child.stdin.take().unwrap();
        let writer = thread::spawn(move || stdin.write_all(input.as_bytes()));
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = writer.join();
                panic!("Asana offline fixtures exceeded 15 seconds");
            }
            thread::sleep(Duration::from_millis(10));
        };
        writer.join().unwrap().unwrap();
        let mut stdout = String::new();
        let mut stderr = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .take(8192)
            .read_to_string(&mut stdout)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .take(8192)
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(status.success(), "Asana fixture runner: {stderr}");
        assert!(
            stdout.trim().starts_with("Asana offline fixtures passed ("),
            "unexpected fixture result: {stdout}; {stderr}"
        );
    }
}
