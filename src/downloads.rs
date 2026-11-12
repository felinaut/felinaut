//! Downloads: the list, its state transitions, safe file naming and persistence.
//!
//! One tab-separated line per download in `Felifox/downloads.tsv` under Application Support,
//! oldest first: `state \t started (unix seconds) \t bytes \t source host \t path`. Only the
//! host a file came from is kept, never its URL. A download that was running when Felinaut
//! quit comes back as interrupted. Deleting the file clears the list, not the saved files.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

// Spelled out so the native tests, which include this file by path, find it too.
#[path = "downloads/native.rs"]
pub mod native;

const HEADER: &str = "# Felifox downloads: state\tstarted (unix seconds)\tbytes\tsource host\tpath";
// ponytail: fixed cap, oldest finished entries dropped; add a history view if it ever matters.
const MAX_ENTRIES: usize = 200;
/// macOS allows 255 bytes per name; this leaves room for a ` (999)` suffix.
const MAX_NAME_BYTES: usize = 240;
const MAX_COPIES: u32 = 999;
const SAVE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Done,
    Failed,
    Cancelled,
    /// Still running when Felinaut quit; WebKit cannot continue it.
    Interrupted,
}

impl State {
    /// The name in `downloads.tsv` and in agent control's answers.
    pub fn tag(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            // Nothing can be running in a file read at launch.
            "running" | "interrupted" => Self::Interrupted,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    pub id: u64,
    /// The native download behind a running item.
    key: Option<u64>,
    pub path: PathBuf,
    /// The host the file came from; empty for `data:` and other host-less sources.
    pub host: String,
    pub state: State,
    pub received: u64,
    pub total: Option<u64>,
    pub started: u64,
}

impl Download {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// How far a running download is, when the server said how large the file is.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|total| *total > 0)?;
        (self.state == State::Running).then(|| (self.received as f32 / total as f32).min(1.))
    }

    /// The state in words, with sizes while running and once done.
    pub fn status(&self) -> String {
        match (self.state, self.total) {
            (State::Running, Some(total)) if total > 0 => {
                format!("{} of {}", format_bytes(self.received), format_bytes(total))
            }
            (State::Running, _) => format!("{} so far", format_bytes(self.received)),
            (State::Done, _) => format_bytes(self.received),
            (State::Failed, _) => "Failed".into(),
            (State::Cancelled, _) => "Cancelled".into(),
            (State::Interrupted, _) => "Interrupted".into(),
        }
    }
}

/// What the native side reports, by the key it gave the download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started {
        key: u64,
        url: String,
        path: PathBuf,
        total: Option<u64>,
    },
    Progress {
        key: u64,
        received: u64,
        total: Option<u64>,
    },
    Finished {
        key: u64,
        received: u64,
    },
    Failed {
        key: u64,
    },
}

/// Overall progress of the running downloads, for the toolbar button.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Activity {
    Idle,
    /// At least one running download has no known size.
    Indeterminate,
    Fraction(f32),
}

#[derive(Default)]
pub struct Downloads {
    items: Vec<Download>,
    next_id: u64,
    /// `None` without a home, or when the file was unreadable so it is never overwritten.
    path: Option<PathBuf>,
    dirty: bool,
    saved: Option<Instant>,
}

/// `~/Library/Application Support/Felifox/downloads.tsv`, beside `tabs.tsv`.
pub fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Felifox/downloads.tsv"))
}

/// `~/Downloads`, where every download is saved.
pub fn folder() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.is_absolute().then(|| home.join("Downloads"))
}

impl Downloads {
    /// A missing file is an empty list; an unreadable one is reported and never overwritten.
    pub fn load(path: Option<PathBuf>) -> Self {
        let (items, path) = match path.as_deref().map(fs::read_to_string) {
            Some(Ok(text)) => (parse(&text), path),
            Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), path),
            Some(Err(error)) => {
                eprintln!("Could not read downloads: {error}");
                (Vec::new(), None)
            }
            None => (Vec::new(), None),
        };
        Self {
            next_id: items.len() as u64,
            items,
            path,
            ..Self::default()
        }
    }

    /// Oldest first.
    pub fn items(&self) -> &[Download] {
        &self.items
    }

    /// Applies a native report; false if it changed nothing (an unknown or settled download).
    pub fn apply(&mut self, event: Event, now: u64) -> bool {
        match event {
            Event::Started {
                key,
                url,
                path,
                total,
            } => {
                self.next_id += 1;
                self.items.push(Download {
                    id: self.next_id,
                    key: Some(key),
                    path,
                    host: source_host(&url),
                    state: State::Running,
                    received: 0,
                    total,
                    started: now,
                });
                self.trim();
                self.dirty = true;
                true
            }
            Event::Progress {
                key,
                received,
                total,
            } => self.running(key).is_some_and(|item| {
                item.received = received;
                // The response's length stands when the progress object has none.
                item.total = total.or(item.total);
                true
            }),
            Event::Finished { key, received } => self.settle(key, |item| {
                item.state = State::Done;
                item.received = received;
                item.total = Some(received);
            }),
            Event::Failed { key } => self.settle(key, |item| item.state = State::Failed),
        }
    }

    /// Marks a running download cancelled and returns the native key to stop.
    pub fn cancel(&mut self, id: u64) -> Option<u64> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id && item.state == State::Running)?;
        item.state = State::Cancelled;
        self.dirty = true;
        item.key.take()
    }

    /// Forgets a download that is not running; its file stays.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items
            .retain(|item| item.id != id || item.state == State::Running);
        let removed = self.items.len() != before;
        self.dirty |= removed;
        removed
    }

    /// Forgets everything that is not running.
    pub fn clear_finished(&mut self) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.state == State::Running);
        let removed = self.items.len() != before;
        self.dirty |= removed;
        removed
    }

    pub fn activity(&self) -> Activity {
        let mut running = self
            .items
            .iter()
            .filter(|item| item.state == State::Running)
            .peekable();
        if running.peek().is_none() {
            return Activity::Idle;
        }
        let mut received = 0u64;
        let mut expected = 0u64;
        for item in running {
            let Some(total) = item.total.filter(|total| *total > 0) else {
                return Activity::Indeterminate;
            };
            received = received.saturating_add(item.received.min(total));
            expected = expected.saturating_add(total);
        }
        Activity::Fraction(received as f32 / expected as f32)
    }

    /// The path and contents to write if the list changed, at most once per `SAVE_INTERVAL`
    /// so two background writes never race. Progress alone is never written.
    pub fn snapshot_due(&mut self, now: Instant) -> Option<(PathBuf, String)> {
        if self
            .saved
            .is_some_and(|saved| now.duration_since(saved) < SAVE_INTERVAL)
        {
            return None;
        }
        let snapshot = self.take_snapshot()?;
        self.saved = Some(now);
        Some(snapshot)
    }

    /// As `snapshot_due`, without the wait: for the last write before quitting.
    pub fn take_snapshot(&mut self) -> Option<(PathBuf, String)> {
        if !self.dirty {
            return None;
        }
        let path = self.path.clone()?;
        self.dirty = false;
        Some((path, serialize(&self.items)))
    }

    fn running(&mut self, key: u64) -> Option<&mut Download> {
        self.items
            .iter_mut()
            .find(|item| item.key == Some(key) && item.state == State::Running)
    }

    fn settle(&mut self, key: u64, change: impl FnOnce(&mut Download)) -> bool {
        let Some(item) = self.running(key) else {
            return false;
        };
        change(item);
        item.key = None;
        self.dirty = true;
        true
    }

    fn trim(&mut self) {
        while self.items.len() > MAX_ENTRIES {
            let Some(oldest) = self
                .items
                .iter()
                .position(|item| item.state != State::Running)
            else {
                break;
            };
            self.items.remove(oldest);
        }
    }
}

/// Saved downloads. Lines with an unknown state, too few fields or an unsafe path are skipped.
pub fn parse(text: &str) -> Vec<Download> {
    let mut items: Vec<Download> = text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.splitn(5, '\t');
            let state = State::from_tag(fields.next()?)?;
            let started = fields.next()?.parse().unwrap_or(0);
            let bytes = fields.next()?.parse().unwrap_or(0);
            let host = clean_host(fields.next()?);
            let path = PathBuf::from(fields.next()?);
            let done = state == State::Done;
            storable(&path).then(|| Download {
                id: 0,
                key: None,
                path,
                host,
                state,
                received: if done { bytes } else { 0 },
                total: done.then_some(bytes),
                started,
            })
        })
        .collect();
    // Keep the newest when a hand-edited file is over the cap.
    let excess = items.len().saturating_sub(MAX_ENTRIES);
    items.drain(..excess);
    for (index, item) in items.iter_mut().enumerate() {
        item.id = index as u64 + 1;
    }
    items
}

pub fn serialize(items: &[Download]) -> String {
    let mut out = String::from(HEADER);
    for item in items.iter().filter(|item| storable(&item.path)) {
        let bytes = if item.state == State::Done {
            item.received
        } else {
            0
        };
        out.push('\n');
        out.push_str(&format!(
            "{}\t{}\t{bytes}\t{}\t{}",
            item.state.tag(),
            item.started,
            clean_host(&item.host),
            item.path.display()
        ));
    }
    out.push('\n');
    out
}

/// An absolute UTF-8 path with a file name, no `..` and nothing that would break a line.
fn storable(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().is_some()
        && path
            .components()
            .all(|part| !matches!(part, Component::ParentDir))
        && path
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control))
}

fn clean_host(host: &str) -> String {
    host.chars()
        .filter(|character| !character.is_control() && !character.is_whitespace())
        .take(253)
        .collect()
}

/// The host a download came from, for display: `blob:` URLs name their page's host, and
/// `data:` or malformed URLs have none.
pub fn source_host(url: &str) -> String {
    let url = url.strip_prefix("blob:").unwrap_or(url);
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return String::new();
    }
    // The host is at the start; never parse a huge URL on the UI thread.
    let head: String = url.chars().take(2048).collect();
    url::Url::parse(&head)
        .ok()
        .and_then(|url| url.host_str().map(clean_host))
        .unwrap_or_default()
}

/// Bidirectional controls can make `exe.pdf` read as `fdp.exe`.
fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// The stem and the extension with its dot. `.tar.gz` and friends stay together.
fn split_extension(name: &str) -> (&str, &str) {
    let Some(dot) = name.rfind('.').filter(|dot| *dot > 0) else {
        return (name, "");
    };
    let dot = match name[..dot].strip_suffix(".tar") {
        Some(stem) if !stem.is_empty() => stem.len(),
        _ => dot,
    };
    // Something this long after a dot is part of the name, not a file type.
    if name.len() - dot > 32 {
        (name, "")
    } else {
        name.split_at(dot)
    }
}

/// A file name that is safe to create in the downloads folder, from a name the server chose:
/// one path component, visible, without control characters, and short enough for the disk.
pub fn safe_name(suggested: &str) -> String {
    let last = suggested.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .filter(|character| !character.is_control() && !is_bidi_control(*character))
        // Finder shows a colon as a slash.
        .map(|character| if character == ':' { '-' } else { character })
        .collect();
    // No hidden files, and no `.` or `..`.
    let name = cleaned.trim().trim_start_matches('.').trim_start();
    let (stem, extension) = split_extension(name);
    let mut end = stem.len().min(MAX_NAME_BYTES - extension.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    let name = format!("{}{extension}", stem[..end].trim_end());
    if name.is_empty() || name == extension {
        format!("download{extension}")
    } else {
        name
    }
}

/// `name`, or `name (1).ext`, `name (2).ext`… for the first one that `taken` does not claim.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> Option<String> {
    if !taken(name) {
        return Some(name.to_owned());
    }
    let (stem, extension) = split_extension(name);
    (1..=MAX_COPIES)
        .map(|copy| format!("{stem} ({copy}){extension}"))
        .find(|candidate| !taken(candidate))
}

/// Where to save a download the server calls `suggested`: always directly inside `folder`,
/// never over a path that `taken` claims. `None` if the folder is unusable or too crowded.
pub fn destination(
    folder: &Path,
    suggested: &str,
    taken: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if !folder.is_absolute() {
        return None;
    }
    let name = unique_name(&safe_name(suggested), |name| taken(&folder.join(name)))?;
    let path = folder.join(name);
    // `safe_name` already guarantees this; a download must never leave the folder.
    (path.parent() == Some(folder) && storable(&path)).then_some(path)
}

/// Sizes as Finder shows them: decimal units, one decimal from a megabyte up.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} KB")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(key: u64, name: &str, total: Option<u64>) -> Event {
        Event::Started {
            key,
            url: "https://files.example.com/get?token=secret".into(),
            path: PathBuf::from("/Users/cat/Downloads").join(name),
            total,
        }
    }

    fn list() -> Downloads {
        Downloads {
            path: Some(PathBuf::from("/profile/downloads.tsv")),
            ..Downloads::default()
        }
    }

    #[test]
    fn a_download_runs_reports_progress_and_finishes() {
        let mut downloads = list();
        assert_eq!(downloads.activity(), Activity::Idle);
        assert!(downloads.apply(started(7, "a.zip", Some(2_000_000)), 100));
        let item = &downloads.items()[0];
        assert_eq!(
            (item.name().as_str(), item.host.as_str(), item.state),
            ("a.zip", "files.example.com", State::Running)
        );
        assert_eq!(item.fraction(), Some(0.));

        assert!(downloads.apply(
            Event::Progress {
                key: 7,
                received: 500_000,
                total: None
            },
            101
        ));
        let item = &downloads.items()[0];
        assert_eq!(item.fraction(), Some(0.25));
        assert_eq!(item.status(), "500 KB of 2.0 MB");
        assert_eq!(downloads.activity(), Activity::Fraction(0.25));

        assert!(downloads.apply(
            Event::Finished {
                key: 7,
                received: 2_000_000
            },
            102
        ));
        let item = &downloads.items()[0];
        assert_eq!(
            (item.state, item.fraction(), item.status().as_str()),
            (State::Done, None, "2.0 MB")
        );
        assert_eq!(downloads.activity(), Activity::Idle);
        // A late report for a settled download changes nothing.
        assert!(!downloads.apply(Event::Failed { key: 7 }, 103));
        assert_eq!(downloads.items()[0].state, State::Done);
    }

    #[test]
    fn unknown_sizes_are_indeterminate_not_guessed() {
        let mut downloads = list();
        downloads.apply(started(1, "known.bin", Some(100)), 0);
        downloads.apply(started(2, "unknown.bin", None), 0);
        downloads.apply(
            Event::Progress {
                key: 2,
                received: 1_500,
                total: None,
            },
            0,
        );
        let item = &downloads.items()[1];
        assert_eq!(item.fraction(), None);
        assert_eq!(item.status(), "2 KB so far");
        assert_eq!(downloads.activity(), Activity::Indeterminate);
        // A size learned later is used.
        downloads.apply(
            Event::Progress {
                key: 2,
                received: 50,
                total: Some(100),
            },
            0,
        );
        assert_eq!(downloads.activity(), Activity::Fraction(0.25));
        // A zero length is not a size.
        downloads.apply(started(3, "empty.bin", Some(0)), 0);
        assert_eq!(downloads.items()[2].fraction(), None);
        assert_eq!(downloads.activity(), Activity::Indeterminate);
    }

    #[test]
    fn failures_and_cancels_settle_and_ignore_later_reports() {
        let mut downloads = list();
        downloads.apply(started(1, "fails.bin", Some(10)), 0);
        downloads.apply(started(2, "cancelled.bin", Some(10)), 0);
        assert!(downloads.apply(Event::Failed { key: 1 }, 0));
        assert_eq!(downloads.items()[0].state, State::Failed);
        assert_eq!(downloads.items()[0].status(), "Failed");

        let id = downloads.items()[1].id;
        assert_eq!(downloads.cancel(id), Some(2));
        assert_eq!(downloads.items()[1].state, State::Cancelled);
        // Cancelling twice, or something finished or unknown, does nothing.
        assert_eq!(downloads.cancel(id), None);
        assert_eq!(downloads.cancel(downloads.items()[0].id), None);
        assert_eq!(downloads.cancel(999), None);
        // WebKit may still report the cancelled download; it stays cancelled.
        for event in [
            Event::Failed { key: 2 },
            Event::Finished {
                key: 2,
                received: 10,
            },
            Event::Progress {
                key: 2,
                received: 5,
                total: None,
            },
            Event::Progress {
                key: 42,
                received: 5,
                total: None,
            },
        ] {
            assert!(!downloads.apply(event, 0));
        }
        assert_eq!(downloads.items()[1].state, State::Cancelled);
        assert_eq!(downloads.activity(), Activity::Idle);
    }

    #[test]
    fn removing_and_clearing_spare_running_downloads() {
        let mut downloads = list();
        downloads.apply(started(1, "running.bin", None), 0);
        downloads.apply(started(2, "done.bin", None), 0);
        downloads.apply(started(3, "failed.bin", None), 0);
        downloads.apply(
            Event::Finished {
                key: 2,
                received: 1,
            },
            0,
        );
        downloads.apply(Event::Failed { key: 3 }, 0);
        let ids: Vec<u64> = downloads.items().iter().map(|item| item.id).collect();
        assert!(!downloads.remove(ids[0]), "running downloads stay");
        assert!(!downloads.remove(999));
        assert!(downloads.remove(ids[1]));
        assert_eq!(downloads.items().len(), 2);
        assert!(downloads.clear_finished());
        assert!(!downloads.clear_finished());
        assert_eq!(downloads.items().len(), 1);
        assert_eq!(downloads.items()[0].name(), "running.bin");
        // Ids are never reused within a session.
        downloads.apply(started(4, "next.bin", None), 0);
        assert!(!ids.contains(&downloads.items()[1].id));
    }

    #[test]
    fn the_list_is_capped_without_dropping_running_downloads() {
        let mut downloads = list();
        downloads.apply(started(0, "running.bin", None), 0);
        for key in 1..=MAX_ENTRIES as u64 + 5 {
            downloads.apply(started(key, &format!("{key}.bin"), None), 0);
            downloads.apply(Event::Failed { key }, 0);
        }
        assert_eq!(downloads.items().len(), MAX_ENTRIES);
        assert_eq!(downloads.items()[0].name(), "running.bin");
        assert_eq!(downloads.items()[1].name(), "7.bin");
    }

    #[test]
    fn hostile_names_stay_inside_the_folder() {
        let folder = Path::new("/Users/cat/Downloads");
        for (suggested, expected) in [
            ("report.pdf", "report.pdf"),
            ("../../etc/passwd", "passwd"),
            ("..\\..\\windows\\system32\\evil.dll", "evil.dll"),
            ("/etc/cron.d/job", "job"),
            ("~/.ssh/authorized_keys", "authorized_keys"),
            ("", "download"),
            ("   ", "download"),
            (".", "download"),
            ("..", "download"),
            ("...", "download"),
            ("a/", "download"),
            (".bashrc", "bashrc"),
            (".. /.hidden.txt", "hidden.txt"),
            ("a\0b\n.txt", "ab.txt"),
            ("invoice\u{202e}fdp.exe", "invoicefdp.exe"),
            ("12:30 notes.txt", "12-30 notes.txt"),
            ("  spaced name.txt  ", "spaced name.txt"),
            (".png", "png"),
            ("ünïcödé 🐈.zip", "ünïcödé 🐈.zip"),
        ] {
            assert_eq!(safe_name(suggested), expected, "{suggested:?}");
            let path = destination(folder, suggested, |_| false).unwrap();
            assert_eq!(path.parent(), Some(folder), "{suggested:?}");
            assert_eq!(path, folder.join(expected), "{suggested:?}");
        }
        assert_eq!(
            destination(Path::new("Downloads"), "a.txt", |_| false),
            None
        );
        assert_eq!(destination(Path::new(""), "a.txt", |_| false), None);
    }

    #[test]
    fn very_long_names_are_cut_at_a_character_and_keep_their_type() {
        let name = safe_name(&format!("{}.tar.gz", "ä".repeat(400)));
        assert!(name.len() <= MAX_NAME_BYTES, "{}", name.len());
        assert!(name.ends_with("ä.tar.gz"));
        let copy = unique_name(&name, |candidate| candidate == name).unwrap();
        assert!(copy.len() <= 255 && copy.ends_with(" (1).tar.gz"));

        let no_type = safe_name(&"x".repeat(1000));
        assert_eq!(no_type.len(), MAX_NAME_BYTES);
        // A huge "extension" is treated as part of the name and cut too.
        let long_type = safe_name(&format!("a.{}", "b".repeat(1000)));
        assert_eq!(long_type.len(), MAX_NAME_BYTES);
    }

    #[test]
    fn name_collisions_count_up_before_the_extension() {
        let taken = ["file.zip", "file (1).zip", "notes", "v1.2.tar.gz", ".tar"];
        let pick = |name: &str| unique_name(name, |candidate| taken.contains(&candidate));
        assert_eq!(pick("new.zip").as_deref(), Some("new.zip"));
        assert_eq!(pick("file.zip").as_deref(), Some("file (2).zip"));
        assert_eq!(pick("notes").as_deref(), Some("notes (1)"));
        assert_eq!(pick("v1.2.tar.gz").as_deref(), Some("v1.2 (1).tar.gz"));
        assert_eq!(pick(".tar").as_deref(), Some(".tar (1)"));
        assert_eq!(unique_name("a.b", |_| true), None, "bounded");

        let folder = Path::new("/d");
        let on_disk = |path: &Path| path == Path::new("/d/file.zip");
        assert_eq!(
            destination(folder, "x/../file.zip", on_disk),
            Some(PathBuf::from("/d/file (1).zip"))
        );
        assert_eq!(destination(folder, "file.zip", |_| true), None);
    }

    #[test]
    fn source_hosts_come_only_from_web_urls() {
        for (url, host) in [
            ("https://example.com/a.zip?token=1", "example.com"),
            ("http://127.0.0.1:8000/a", "127.0.0.1"),
            ("https://user:secret@cdn.example.org/a", "cdn.example.org"),
            ("blob:https://app.example.com/1234-5678", "app.example.com"),
            ("data:text/plain;base64,aGk=", ""),
            ("file:///etc/passwd", ""),
            ("javascript:alert(1)", ""),
            ("https://", ""),
            ("not a url", ""),
            ("", ""),
        ] {
            assert_eq!(source_host(url), host, "{url}");
        }
        let huge = format!("https://big.example.com/{}", "x".repeat(1_000_000));
        assert_eq!(source_host(&huge), "big.example.com");
    }

    #[test]
    fn sizes_use_decimal_units() {
        for (bytes, text) in [
            (0, "0 B"),
            (999, "999 B"),
            (1_000, "1 KB"),
            (1_499, "1 KB"),
            (999_949, "1000 KB"),
            (1_000_000, "1.0 MB"),
            (3_250_000, "3.2 MB"),
            (999_950_000, "1.0 GB"),
            (1_500_000_000, "1.5 GB"),
            (2_000_000_000_000, "2.0 TB"),
            (u64::MAX, "18446744.1 TB"),
        ] {
            assert_eq!(format_bytes(bytes), text, "{bytes}");
        }
    }

    #[test]
    fn round_trips_and_running_downloads_come_back_interrupted() {
        let mut downloads = list();
        downloads.apply(started(1, "done.zip", Some(2_000)), 10);
        downloads.apply(started(2, "failed.zip", None), 20);
        downloads.apply(started(3, "cancelled.zip", None), 30);
        downloads.apply(started(4, "active.zip", Some(500)), 40);
        downloads.apply(
            Event::Finished {
                key: 1,
                received: 2_000,
            },
            11,
        );
        downloads.apply(Event::Failed { key: 2 }, 21);
        let cancelled = downloads.items()[2].id;
        downloads.cancel(cancelled);

        let (path, text) = downloads.take_snapshot().unwrap();
        assert_eq!(path, Path::new("/profile/downloads.tsv"));
        assert!(!text.contains("token"), "only the host is stored: {text}");
        let restored = parse(&text);
        let summary: Vec<_> = restored
            .iter()
            .map(|item| (item.id, item.name(), item.state, item.started))
            .collect();
        assert_eq!(
            summary,
            [
                (1, "done.zip".to_owned(), State::Done, 10),
                (2, "failed.zip".to_owned(), State::Failed, 20),
                (3, "cancelled.zip".to_owned(), State::Cancelled, 30),
                (4, "active.zip".to_owned(), State::Interrupted, 40),
            ]
        );
        assert_eq!(restored[0].status(), "2 KB");
        assert_eq!(restored[0].host, "files.example.com");
        assert_eq!(restored[3].status(), "Interrupted");
        assert!(restored.iter().all(|item| item.fraction().is_none()));
        // What was read is written back unchanged, except that nothing is running.
        assert_eq!(serialize(&restored), text.replace("running", "interrupted"));
    }

    #[test]
    fn snapshots_follow_changes_not_progress() {
        let mut downloads = list();
        let start = Instant::now();
        assert_eq!(downloads.snapshot_due(start), None, "nothing changed");
        downloads.apply(started(1, "a.zip", None), 0);
        assert!(downloads.snapshot_due(start).is_some());
        downloads.apply(
            Event::Progress {
                key: 1,
                received: 5,
                total: None,
            },
            0,
        );
        assert_eq!(downloads.snapshot_due(start + SAVE_INTERVAL), None);
        downloads.apply(Event::Failed { key: 1 }, 0);
        assert_eq!(
            downloads.snapshot_due(start + SAVE_INTERVAL / 2),
            None,
            "debounced"
        );
        assert!(downloads.snapshot_due(start + SAVE_INTERVAL).is_some());
        downloads.clear_finished();
        assert!(downloads.take_snapshot().is_some(), "quitting never waits");
        assert_eq!(downloads.take_snapshot(), None);

        // Without a profile path nothing is ever written.
        let mut homeless = Downloads::default();
        homeless.apply(started(1, "a.zip", None), 0);
        assert_eq!(homeless.take_snapshot(), None);
    }

    #[test]
    fn malformed_and_unsafe_lines_are_skipped() {
        let text = [
            "# an older header with other columns",
            "",
            "done",
            "done\t1\t2",
            "done\t1\t2\texample.com",
            "paused\t1\t2\texample.com\t/Users/cat/Downloads/unknown-state.zip",
            "done\t1\t2\texample.com\trelative/path.zip",
            "done\t1\t2\texample.com\t/Users/cat/Downloads/../../../etc/passwd",
            "done\t1\t2\texample.com\t/",
            "done\t1\t2\texample.com\t",
            "done\tsoon\tmany\tex ample\u{7}.com\t/Users/cat/Downloads/odd numbers.zip",
            "done\t1\t2\texample.com\t/Users/cat/Downloads/a tab\tin the name.zip",
            "failed\t5\t99\t\t/Users/cat/Downloads/no host.zip",
            "\u{0}\u{1}binary\u{2}garbage",
        ]
        .join("\n");
        let items = parse(&text);
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(
            (items[0].started, items[0].received, items[0].host.as_str()),
            (0, 0, "example.com")
        );
        assert_eq!(items[0].name(), "odd numbers.zip");
        // Sizes are only kept for finished files.
        assert_eq!((items[1].received, items[1].total), (0, None));
        assert_eq!(items[1].host, "");
        assert_eq!(serialize(&items).lines().count(), 3);
        assert_eq!(parse(""), Vec::new());
        assert_eq!(parse(HEADER), Vec::new());
    }

    #[test]
    fn an_oversized_file_keeps_the_newest_entries() {
        let text: String = (0..MAX_ENTRIES + 10)
            .map(|index| format!("done\t{index}\t1\th\t/d/{index}.zip\n"))
            .collect();
        let items = parse(&text);
        assert_eq!(items.len(), MAX_ENTRIES);
        assert_eq!(items[0].name(), "10.zip");
        assert_eq!(items[0].id, 1);
    }

    #[test]
    fn loading_tolerates_a_missing_file_and_protects_an_unreadable_one() {
        let dir = std::env::temp_dir().join(format!("felinaut-downloads-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("downloads.tsv");
        let mut downloads = Downloads::load(Some(missing.clone()));
        assert!(downloads.items().is_empty());
        downloads.apply(started(1, "a.zip", None), 0);
        assert_eq!(downloads.take_snapshot().unwrap().0, missing);

        fs::write(&missing, "done\t1\t2\th\t/d/kept.zip\n").unwrap();
        let mut downloads = Downloads::load(Some(missing));
        assert_eq!(downloads.items()[0].name(), "kept.zip");
        downloads.apply(started(1, "new.zip", None), 0);
        assert_eq!(downloads.items()[1].id, 2);

        // A directory in the file's place cannot be read; it must not be replaced.
        let mut unreadable = Downloads::load(Some(dir.clone()));
        unreadable.apply(started(1, "a.zip", None), 0);
        assert_eq!(unreadable.take_snapshot(), None);
        assert!(Downloads::load(None).items().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lives_in_the_profile_and_saves_to_the_downloads_folder() {
        if let (Some(file), Some(folder)) = (default_path(), folder()) {
            assert!(file.ends_with("Library/Application Support/Felifox/downloads.tsv"));
            assert!(folder.is_absolute() && folder.ends_with("Downloads"));
        }
    }
}
