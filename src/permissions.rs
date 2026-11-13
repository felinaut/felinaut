//! Site permissions for the camera and the microphone: the decisions and their file.
//!
//! One tab-separated line per decision in `Felifox/permissions.tsv` under Application Support:
//! `decision \t kind \t origin`. A decision belongs to the origin that asked (scheme, host and
//! port), not to a tab or a page address. An origin without a line gets a prompt, never a
//! grant. Deleting the file forgets every decision.

use std::{
    collections::BTreeMap,
    fs, io,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError},
    time::{Duration, Instant},
};

// Spelled out so the native tests, which include this file by path, find it too.
#[path = "permissions/native.rs"]
pub mod native;

const HEADER: &str = "# Felifox site permissions: decision\tkind\torigin";
/// More lines than a person makes by hand; a larger file is cut here.
const MAX_ENTRIES: usize = 2000;
/// The longest host name DNS permits.
const MAX_HOST: usize = 253;
const SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// After this many failed writes in a row, only the write at quit tries again.
// ponytail: no later background retry in this session. Count per version if a disk that
// comes back must get the file before quit.
const MAX_FAILURES: u32 = 5;
/// After this many "Not Now" answers a site gets no more prompts until the user resets it or
/// Felinaut starts again. A page that asks again after each refusal cannot nag.
const MAX_DISMISSALS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Camera,
    Microphone,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::Microphone => "microphone",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "camera" => Some(Self::Camera),
            "microphone" => Some(Self::Microphone),
            _ => None,
        }
    }
}

/// The kinds behind a `WKMediaCaptureType`. An unknown type has none, and is denied.
pub fn kinds(capture_type: isize) -> Option<&'static [Kind]> {
    match capture_type {
        0 => Some(&[Kind::Camera]),
        1 => Some(&[Kind::Microphone]),
        2 => Some(&[Kind::Camera, Kind::Microphone]),
        _ => None,
    }
}

/// "camera", "microphone" or "camera and microphone".
pub fn describe(kinds: &[Kind]) -> String {
    let names: Vec<_> = kinds.iter().map(|kind| kind.tag()).collect();
    names.join(" and ")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// What a request gets. There is no stored decision at first, so the default is `Ask`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Grant,
    Deny,
    Ask,
}

/// The key for a decision, such as `https://example.com` or `http://localhost:3000`, from the
/// parts of a `WKSecurityOrigin`. The parts are untrusted. `None` means the origin never gets
/// a prompt: it is not HTTPS and not loopback, or its host or port is not valid.
pub fn origin(scheme: &str, host: &str, port: i64) -> Option<String> {
    let scheme = scheme.to_ascii_lowercase();
    let mut host = host.to_ascii_lowercase();
    // An IPv6 address gets its brackets, as in a URL.
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    let valid_host = !host.is_empty()
        && host.len() <= MAX_HOST
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '[' | ']'));
    if !valid_host {
        return None;
    }
    let default_port = match scheme.as_str() {
        "https" => 443,
        "http" if is_loopback(&host) => 80,
        _ => return None,
    };
    match port {
        // WebKit gives 0 for the scheme's own port.
        0 => Some(format!("{scheme}://{host}")),
        port if port == default_port => Some(format!("{scheme}://{host}")),
        1..=65535 => Some(format!("{scheme}://{host}:{port}")),
        _ => None,
    }
}

fn is_loopback(host: &str) -> bool {
    let address = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || address.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The origin of a page address, in the same form as `origin`.
pub fn origin_of_url(url: &str) -> Option<String> {
    // The origin is at the start; never parse a huge URL on the UI thread.
    let head: String = url.chars().take(2048).collect();
    let url = url::Url::parse(&head).ok()?;
    origin(
        url.scheme(),
        url.host_str()?,
        url.port().map_or(0, i64::from),
    )
}

/// The origin to ask the user about, or `None` to deny at once. Only a page the user can see
/// may ask. `frame` is the origin of the frame that asks and `top` that of the page around it:
/// a frame from another site is denied, so a prompt always names the site in the address bar.
// ponytail: an embedded frame from another site never gets capture. Ask with both origins
// in the prompt if embedded video calls must work.
pub fn askable(visible: bool, top: Option<String>, frame: Option<String>) -> Option<String> {
    top.filter(|top| visible && frame.as_ref() == Some(top))
}

/// The most characters of an origin that the prompt shows. The bar has room for them all at
/// the minimum window width with the widest sidebar; a test in `permission_ui` checks that.
pub const ORIGIN_MAX: usize = 48;

/// An origin for the prompt. HTTPS is implied. A long host keeps its end, because the end
/// names the owner of the site.
pub fn display_origin(origin: &str) -> String {
    const MAX: usize = ORIGIN_MAX;
    let text = origin.strip_prefix("https://").unwrap_or(origin);
    let length = text.chars().count();
    if length <= MAX {
        return text.into();
    }
    let tail: String = text.chars().skip(length - (MAX - 1)).collect();
    format!("…{tail}")
}

#[derive(Default)]
pub struct Permissions {
    entries: BTreeMap<(String, Kind), Decision>,
    /// The "Not Now" answers per origin in this session. Never saved.
    dismissals: BTreeMap<String, u8>,
    /// `None` without a home, or when the file was unreadable so it is never overwritten.
    path: Option<PathBuf>,
    /// Counts the changes of `entries`. The file is behind while `written` has a lower version.
    version: u64,
    /// What reached the file. Each `Snapshot` shares it, also on a background thread.
    written: Arc<Mutex<Written>>,
    saved: Option<Instant>,
}

#[derive(Default)]
struct Written {
    version: u64,
    failures: u32,
}

/// The contents for the file at one version of the decisions.
pub struct Snapshot {
    path: PathBuf,
    contents: String,
    version: u64,
    written: Arc<Mutex<Written>>,
}

impl Snapshot {
    /// Gives the contents to `write` (`history::write`), unless the file already has this
    /// version or a newer one. One snapshot is written at a time, so the background write and
    /// the write at quit cannot mix, and an older snapshot never replaces a newer one. A
    /// failed write leaves the decisions due for the next snapshot.
    pub fn write(&self, write: impl FnOnce(&Path, &str) -> io::Result<()>) -> io::Result<()> {
        let mut written = lock(&self.written);
        if written.version >= self.version {
            return Ok(());
        }
        match write(&self.path, &self.contents) {
            Ok(()) => {
                *written = Written {
                    version: self.version,
                    failures: 0,
                };
                Ok(())
            }
            Err(error) => {
                written.failures += 1;
                Err(error)
            }
        }
    }
}

/// Nothing panics while it holds this lock, so a poisoned lock still has valid data.
fn lock(written: &Mutex<Written>) -> MutexGuard<'_, Written> {
    written.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `~/Library/Application Support/Felifox/permissions.tsv`, beside `tabs.tsv`.
pub fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Felifox/permissions.tsv"))
}

impl Permissions {
    /// A missing file means no decisions; an unreadable one is reported and never overwritten.
    pub fn load(path: Option<PathBuf>) -> Self {
        let (entries, path) = match path.as_deref().map(fs::read_to_string) {
            Some(Ok(text)) => (parse(&text), path),
            Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => (BTreeMap::new(), path),
            Some(Err(error)) => {
                eprintln!("Could not read site permissions: {error}");
                (BTreeMap::new(), None)
            }
            None => (BTreeMap::new(), None),
        };
        Self {
            entries,
            path,
            ..Self::default()
        }
    }

    /// One stored denial denies. A grant needs a stored permission for each kind. An origin
    /// with `MAX_DISMISSALS` "Not Now" answers is denied instead of asked.
    pub fn answer(&self, origin: &str, kinds: &[Kind]) -> Answer {
        let stored = |kind: &Kind| self.entries.get(&(origin.to_owned(), *kind)).copied();
        if kinds
            .iter()
            .any(|kind| stored(kind) == Some(Decision::Deny))
        {
            Answer::Deny
        } else if !kinds.is_empty()
            && kinds
                .iter()
                .all(|kind| stored(kind) == Some(Decision::Allow))
        {
            Answer::Grant
        } else if self.muted(origin) {
            Answer::Deny
        } else {
            Answer::Ask
        }
    }

    /// Counts a "Not Now" answer for the origin.
    // ponytail: counted per origin, not per page, so a page cannot reset the count with a
    // reload. Only `revoke` or a restart ends it.
    pub fn dismiss(&mut self, origin: &str) {
        let count = self.dismissals.entry(origin.to_owned()).or_default();
        *count = count.saturating_add(1);
    }

    /// Whether the origin used up its prompts for this session.
    fn muted(&self, origin: &str) -> bool {
        self.dismissals.get(origin).copied().unwrap_or(0) >= MAX_DISMISSALS
    }

    pub fn remember(&mut self, origin: &str, kinds: &[Kind], decision: Decision) {
        for kind in kinds {
            self.entries.insert((origin.to_owned(), *kind), decision);
        }
        self.version += u64::from(!kinds.is_empty());
    }

    /// Forgets each decision and each "Not Now" for the origin, so it gets a prompt again.
    pub fn revoke(&mut self, origin: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(known, _), _| known != origin);
        let removed = self.entries.len() != before;
        self.version += u64::from(removed);
        self.dismissals.remove(origin).is_some() || removed
    }

    /// The stored decisions for an origin in words, such as "Camera allowed". An origin that
    /// used up its prompts has "requests refused for now".
    pub fn summary(&self, origin: &str) -> Option<String> {
        let with = |decision| -> Vec<Kind> {
            [Kind::Camera, Kind::Microphone]
                .into_iter()
                .filter(|kind| self.entries.get(&(origin.to_owned(), *kind)) == Some(&decision))
                .collect()
        };
        let parts: Vec<_> = [(Decision::Allow, "allowed"), (Decision::Deny, "blocked")]
            .into_iter()
            .filter_map(|(decision, word)| {
                let kinds = with(decision);
                (!kinds.is_empty()).then(|| format!("{} {word}", describe(&kinds)))
            })
            .chain(
                self.muted(origin)
                    .then(|| "requests refused for now".to_owned()),
            )
            .collect();
        let text = parts.join(", ");
        let mut letters = text.chars();
        let first = letters.next()?;
        Some(first.to_uppercase().chain(letters).collect())
    }

    /// The snapshot to write in the background if the file is behind: at most one per
    /// `SAVE_INTERVAL`, none while another write runs, and none after `MAX_FAILURES` failed
    /// writes in a row. Never waits for a write, so it is safe on the UI thread.
    pub fn snapshot_due(&mut self, now: Instant) -> Option<Snapshot> {
        if self
            .saved
            .is_some_and(|saved| now.duration_since(saved) < SAVE_INTERVAL)
        {
            return None;
        }
        let written = match self.written.try_lock() {
            Ok(written) => written,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        let due = written.version < self.version && written.failures < MAX_FAILURES;
        drop(written);
        let snapshot = due.then(|| self.snapshot()).flatten()?;
        self.saved = Some(now);
        Some(snapshot)
    }

    /// The snapshot for the last write before quitting, if the file is behind. It waits for
    /// a background write that still runs, and it tries also after failed writes.
    pub fn take_snapshot(&mut self) -> Option<Snapshot> {
        let behind = lock(&self.written).version < self.version;
        behind.then(|| self.snapshot()).flatten()
    }

    fn snapshot(&self) -> Option<Snapshot> {
        Some(Snapshot {
            path: self.path.clone()?,
            contents: serialize(&self.entries),
            version: self.version,
            written: self.written.clone(),
        })
    }
}

/// What `route` knows of a tab.
pub struct TabState<'a> {
    /// The key of the tab's page in `native`, if the tab has a page.
    pub page: Option<usize>,
    /// The address of the tab. It changes as soon as a load starts.
    pub url: &'a str,
    /// Whether the user sees the page: the tab is the selected tab or its split partner, and
    /// it shows neither the home page nor a load error.
    pub on_display: bool,
}

/// The answers to give now, as request id and grant, and the request to show.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Routed {
    pub answers: Vec<(u64, bool)>,
    pub prompt: Option<native::Request>,
}

/// Decides what happens with the waiting requests, oldest first.
/// - A request of a page that no tab has, or of a tab that left the site that asked, is
///   void: the answer is a denial. A prompt therefore never outlives its document's site.
/// - A stored decision answers at once, in each tab.
/// - The oldest request without a decision in a page on display is the prompt. Each other
///   such request waits for its turn.
pub fn route(
    requests: Vec<native::Request>,
    tabs: &[TabState],
    permissions: &Permissions,
) -> Routed {
    let mut routed = Routed::default();
    for request in requests {
        let tab = tabs
            .iter()
            .find(|tab| tab.page == Some(request.page))
            .filter(|tab| origin_of_url(tab.url).as_ref() == Some(&request.origin));
        let Some(tab) = tab else {
            routed.answers.push((request.id, false));
            continue;
        };
        match permissions.answer(&request.origin, request.kinds) {
            Answer::Grant => routed.answers.push((request.id, true)),
            Answer::Deny => routed.answers.push((request.id, false)),
            Answer::Ask if routed.prompt.is_none() && tab.on_display => {
                routed.prompt = Some(request);
            }
            Answer::Ask => {}
        }
    }
    routed
}

/// Saved decisions. A line with an unknown decision or kind, a missing field or an origin that
/// could not ask is skipped. An origin must be in the exact form that `origin` makes.
pub fn parse(text: &str) -> BTreeMap<(String, Kind), Decision> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let decision = match fields.next()? {
                "allow" => Decision::Allow,
                "deny" => Decision::Deny,
                _ => return None,
            };
            let kind = Kind::from_tag(fields.next()?)?;
            let origin = fields.next()?;
            (fields.next().is_none() && origin_of_url(origin).as_deref() == Some(origin))
                .then(|| ((origin.to_owned(), kind), decision))
        })
        .take(MAX_ENTRIES)
        .collect()
}

pub fn serialize(entries: &BTreeMap<(String, Kind), Decision>) -> String {
    let mut out = String::from(HEADER);
    for ((origin, kind), decision) in entries {
        let decision = match decision {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        };
        out.push_str(&format!("\n{decision}\t{}\t{origin}", kind.tag()));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE: &str = "https://meet.example";
    const BOTH: &[Kind] = &[Kind::Camera, Kind::Microphone];

    fn save(path: &Path, contents: &str) -> io::Result<()> {
        fs::write(path, contents)
    }

    #[test]
    fn the_default_answer_is_a_prompt_and_never_a_grant() {
        let permissions = Permissions::default();
        for capture_type in 0..=2 {
            let kinds = kinds(capture_type).unwrap();
            assert_eq!(permissions.answer(SITE, kinds), Answer::Ask);
        }
        assert_eq!(permissions.answer(SITE, &[]), Answer::Ask);
        // A capture type that WebKit adds later has no kinds: the request is denied.
        assert_eq!(kinds(3), None);
        assert_eq!(kinds(-1), None);
    }

    #[test]
    fn allow_and_deny_apply_to_one_origin_and_kind() {
        let mut permissions = Permissions::default();
        permissions.remember(SITE, &[Kind::Microphone], Decision::Allow);
        assert_eq!(permissions.answer(SITE, &[Kind::Microphone]), Answer::Grant);
        // The camera has no decision, so a request for both still asks.
        assert_eq!(permissions.answer(SITE, BOTH), Answer::Ask);
        assert_eq!(permissions.answer(SITE, &[Kind::Camera]), Answer::Ask);
        // Another port, scheme or host is another origin.
        for other in [
            "https://meet.example:8443",
            "http://meet.example",
            "https://example",
        ] {
            assert_eq!(
                permissions.answer(other, &[Kind::Microphone]),
                Answer::Ask,
                "{other}"
            );
        }
        // One denial is enough to deny a request for both.
        permissions.remember(SITE, &[Kind::Camera], Decision::Deny);
        assert_eq!(permissions.answer(SITE, BOTH), Answer::Deny);
        assert_eq!(permissions.answer(SITE, &[Kind::Microphone]), Answer::Grant);
        // A later decision replaces an earlier one.
        permissions.remember(SITE, BOTH, Decision::Allow);
        assert_eq!(permissions.answer(SITE, BOTH), Answer::Grant);
    }

    #[test]
    fn revoke_forgets_one_site_and_the_summary_names_its_decisions() {
        let other = "https://other.example";
        let mut permissions = Permissions::default();
        assert_eq!(permissions.summary(SITE), None);
        assert!(!permissions.revoke(SITE), "nothing to revoke");
        permissions.remember(SITE, &[Kind::Microphone], Decision::Allow);
        permissions.remember(SITE, &[Kind::Camera], Decision::Deny);
        permissions.remember(other, BOTH, Decision::Allow);
        assert_eq!(
            permissions.summary(SITE).as_deref(),
            Some("Microphone allowed, camera blocked")
        );
        assert_eq!(
            permissions.summary(other).as_deref(),
            Some("Camera and microphone allowed")
        );

        assert!(permissions.revoke(SITE));
        assert!(!permissions.revoke(SITE), "nothing is left to revoke");
        assert_eq!(permissions.answer(SITE, BOTH), Answer::Ask);
        assert_eq!(permissions.summary(SITE), None);
        assert_eq!(permissions.answer(other, BOTH), Answer::Grant);
        // The change reaches the file.
        permissions.path = Some("/nonexistent/permissions.tsv".into());
        let contents = permissions.take_snapshot().unwrap().contents;
        assert!(!contents.contains(SITE) && contents.contains(other));
    }

    #[test]
    fn a_site_gets_no_prompt_after_repeated_dismissals() {
        let other = "https://other.example";
        let mut permissions = Permissions {
            path: Some("/nonexistent/permissions.tsv".into()),
            ..Permissions::default()
        };
        permissions.remember(SITE, &[Kind::Microphone], Decision::Allow);
        let snapshot = permissions.take_snapshot().unwrap();
        snapshot.write(|_, _| Ok(())).unwrap();
        for _ in 0..MAX_DISMISSALS - 1 {
            permissions.dismiss(SITE);
            assert_eq!(permissions.answer(SITE, &[Kind::Camera]), Answer::Ask);
        }
        assert_eq!(
            permissions.summary(SITE).as_deref(),
            Some("Microphone allowed")
        );
        permissions.dismiss(SITE);
        // The site is denied without a prompt, for each kind that has no stored permission.
        assert_eq!(permissions.answer(SITE, &[Kind::Camera]), Answer::Deny);
        assert_eq!(permissions.answer(SITE, BOTH), Answer::Deny);
        assert_eq!(permissions.answer(SITE, &[Kind::Microphone]), Answer::Grant);
        assert_eq!(
            permissions.summary(SITE).as_deref(),
            Some("Microphone allowed, requests refused for now")
        );
        // Another site still gets its prompt, and one more dismissal does not overflow.
        assert_eq!(permissions.answer(other, BOTH), Answer::Ask);
        for _ in 0..300 {
            permissions.dismiss(SITE);
        }
        assert_eq!(permissions.answer(SITE, &[Kind::Camera]), Answer::Deny);
        // The count is never saved.
        assert!(permissions.take_snapshot().is_none());

        // `route` gives the denial also to a page on display.
        let tabs = [tab(1, "https://meet.example/", true)];
        let routed = route(
            vec![request(7, 1, SITE, &[Kind::Camera])],
            &tabs,
            &permissions,
        );
        assert_eq!(routed.answers, [(7, false)]);
        assert_eq!(routed.prompt, None);

        // A reset ends it.
        assert!(permissions.revoke(SITE));
        assert_eq!(permissions.answer(SITE, &[Kind::Camera]), Answer::Ask);
        assert_eq!(permissions.summary(SITE), None);
        // Dismissals alone are also something to reset.
        for _ in 0..MAX_DISMISSALS {
            permissions.dismiss(other);
        }
        assert_eq!(
            permissions.summary(other).as_deref(),
            Some("Requests refused for now")
        );
        assert!(permissions.revoke(other));
        assert!(!permissions.revoke(other));
        assert_eq!(permissions.answer(other, BOTH), Answer::Ask);
    }

    #[test]
    fn only_https_and_loopback_origins_can_ask() {
        for (scheme, host, port, expected) in [
            ("https", "example.com", 0, Some("https://example.com")),
            ("HTTPS", "Example.COM", 443, Some("https://example.com")),
            (
                "https",
                "example.com",
                8443,
                Some("https://example.com:8443"),
            ),
            ("http", "localhost", 3000, Some("http://localhost:3000")),
            ("http", "app.localhost", 0, Some("http://app.localhost")),
            ("http", "127.0.0.1", 80, Some("http://127.0.0.1")),
            ("http", "[::1]", 8080, Some("http://[::1]:8080")),
            ("http", "::1", 8080, Some("http://[::1]:8080")),
            ("http", "example.com", 0, None),
            ("http", "localhost.example.com", 0, None),
            ("http", "127.0.0.1.example.com", 0, None),
            ("http", "192.168.1.10", 0, None),
            ("file", "", 0, None),
            ("about", "blank", 0, None),
            ("data", "example.com", 0, None),
            ("https", "", 0, None),
            ("https", "example.com", -1, None),
            ("https", "example.com", 65536, None),
            ("https", "exa mple.com", 0, None),
            ("https", "example.com\tallow", 0, None),
            ("https", "example.com/path", 0, None),
            ("https", "user@example.com", 0, None),
            ("https", "пример.example", 0, None),
        ] {
            assert_eq!(
                origin(scheme, host, port).as_deref(),
                expected,
                "{scheme} {host} {port}"
            );
        }
        assert_eq!(origin("https", &"a".repeat(254), 0), None);
        assert!(origin("https", &"a".repeat(253), 0).is_some());
    }

    #[test]
    fn page_addresses_give_the_same_origin_form() {
        for (url, expected) in [
            ("https://Example.com/a?b#c", Some("https://example.com")),
            ("https://example.com:443/", Some("https://example.com")),
            (
                "https://example.com:8443/x",
                Some("https://example.com:8443"),
            ),
            ("http://localhost:3000/call", Some("http://localhost:3000")),
            ("http://example.com/", None),
            ("about:blank", None),
            ("felinaut://newtab", None),
            ("not a url", None),
            ("", None),
        ] {
            assert_eq!(origin_of_url(url).as_deref(), expected, "{url}");
        }
    }

    #[test]
    fn hidden_pages_and_frames_from_other_sites_are_denied_without_a_prompt() {
        let site = || Some(SITE.to_owned());
        assert_eq!(askable(true, site(), site()), site());
        assert_eq!(askable(false, site(), site()), None, "a hidden page");
        assert_eq!(
            askable(true, site(), Some("https://ads.example".into())),
            None,
            "a frame from another site"
        );
        assert_eq!(askable(true, site(), None), None, "no frame origin");
        assert_eq!(
            askable(true, None, site()),
            None,
            "an origin that cannot ask"
        );
        assert_eq!(askable(true, None, None), None);
    }

    fn request(id: u64, page: usize, origin: &str, kinds: &'static [Kind]) -> native::Request {
        native::Request {
            id,
            page,
            origin: origin.into(),
            kinds,
        }
    }

    fn tab(page: usize, url: &str, on_display: bool) -> TabState<'_> {
        TabState {
            page: Some(page),
            url,
            on_display,
        }
    }

    #[test]
    fn a_request_goes_to_its_tab_and_the_oldest_one_on_display_is_the_prompt() {
        let permissions = Permissions::default();
        let tabs = [
            tab(1, "https://meet.example/room", true),
            tab(2, "https://other.example/", true),
            tab(3, "https://meet.example/lobby", false),
        ];
        let requests = vec![
            request(10, 3, SITE, BOTH),
            request(11, 2, "https://other.example", &[Kind::Camera]),
            request(12, 1, SITE, BOTH),
        ];
        // Tab 3 is in the background: its request waits. The split partner's request is
        // older than that of the selected tab, so it is the prompt.
        let routed = route(requests.clone(), &tabs, &permissions);
        assert_eq!(routed.answers, []);
        assert_eq!(routed.prompt, Some(requests[1].clone()));
        // The next request on display follows when the first has its answer.
        let routed = route(
            vec![requests[0].clone(), requests[2].clone()],
            &tabs,
            &permissions,
        );
        assert_eq!(routed.answers, []);
        assert_eq!(routed.prompt, Some(requests[2].clone()));
        // No request, or no tab on display that asks: no prompt.
        assert_eq!(route(vec![], &tabs, &permissions), Routed::default());
        let waits = route(vec![requests[0].clone()], &tabs, &permissions);
        assert_eq!(waits, Routed::default());
    }

    #[test]
    fn requests_of_tabs_that_left_the_site_are_void() {
        let permissions = Permissions::default();
        let tabs = [
            tab(1, "https://elsewhere.example/", true),
            tab(2, "https://meet.example:8443/", true),
            tab(3, "about:blank", true),
            TabState {
                page: None,
                url: "https://meet.example/",
                on_display: true,
            },
        ];
        let requests = vec![
            request(1, 1, SITE, BOTH),
            request(2, 2, SITE, BOTH),
            request(3, 3, SITE, BOTH),
            // No tab has page 9.
            request(4, 9, SITE, BOTH),
        ];
        let routed = route(requests, &tabs, &permissions);
        assert_eq!(
            routed.answers,
            [(1, false), (2, false), (3, false), (4, false)]
        );
        assert_eq!(routed.prompt, None);

        // A stored permission does not help a tab that left the site.
        let mut permissions = Permissions::default();
        permissions.remember(SITE, BOTH, Decision::Allow);
        let routed = route(vec![request(1, 1, SITE, BOTH)], &tabs, &permissions);
        assert_eq!(routed.answers, [(1, false)]);
    }

    #[test]
    fn stored_decisions_answer_in_each_tab_without_a_prompt() {
        let blocked = "https://ads.example";
        let mut permissions = Permissions::default();
        permissions.remember(SITE, &[Kind::Microphone], Decision::Allow);
        permissions.remember(blocked, &[Kind::Camera], Decision::Deny);
        let tabs = [
            tab(1, "https://meet.example/", true),
            tab(2, "https://ads.example/", true),
            tab(3, "https://meet.example/", false),
            tab(4, "https://ads.example/", false),
        ];
        let requests = vec![
            // Background tabs and the split partner get the stored answer too.
            request(1, 3, SITE, &[Kind::Microphone]),
            request(2, 4, blocked, BOTH),
            request(3, 2, blocked, &[Kind::Camera]),
            request(4, 1, SITE, &[Kind::Microphone]),
            // The camera has no decision for this site: this request asks.
            request(5, 1, SITE, BOTH),
            request(6, 3, SITE, BOTH),
        ];
        let routed = route(requests.clone(), &tabs, &permissions);
        assert_eq!(
            routed.answers,
            [(1, true), (2, false), (3, false), (4, true)]
        );
        assert_eq!(routed.prompt, Some(requests[4].clone()));
    }

    #[test]
    fn home_and_failed_tabs_get_no_prompt() {
        // The host marks a selected tab that shows the home page or a load error as not on
        // display. Its request waits, and a later request of a page on display is the prompt.
        let permissions = Permissions::default();
        let tabs = [
            tab(1, "https://meet.example/", false),
            tab(2, "https://other.example/", true),
        ];
        let later = request(2, 2, "https://other.example", BOTH);
        let routed = route(
            vec![request(1, 1, SITE, BOTH), later.clone()],
            &tabs,
            &permissions,
        );
        assert_eq!(routed.answers, []);
        assert_eq!(routed.prompt, Some(later));
    }

    #[test]
    fn decisions_survive_a_round_trip() {
        let mut permissions = Permissions::default();
        permissions.remember(SITE, BOTH, Decision::Allow);
        permissions.remember("http://localhost:3000", &[Kind::Camera], Decision::Deny);
        let text = serialize(&permissions.entries);
        assert_eq!(
            text,
            "# Felifox site permissions: decision\tkind\torigin\n\
             deny\tcamera\thttp://localhost:3000\n\
             allow\tcamera\thttps://meet.example\n\
             allow\tmicrophone\thttps://meet.example\n"
        );
        assert_eq!(parse(&text), permissions.entries);
        assert_eq!(serialize(&parse(&text)), text);
    }

    #[test]
    fn ipv6_and_loopback_origins_survive_a_round_trip() {
        let origins = [
            "http://127.0.0.1",
            "http://127.0.0.1:8080",
            "http://[::1]",
            "http://[::1]:8080",
            "http://app.localhost:3000",
            "http://localhost",
            "https://[2001:db8::1]",
            "https://[2001:db8::1]:8443",
        ];
        let mut permissions = Permissions::default();
        for (index, origin) in origins.into_iter().enumerate() {
            // Each origin is in the form that `origin` makes from WebKit's parts.
            assert_eq!(origin_of_url(origin).as_deref(), Some(origin));
            let decision = [Decision::Allow, Decision::Deny][index % 2];
            permissions.remember(origin, BOTH, decision);
        }
        let text = serialize(&permissions.entries);
        let parsed = parse(&text);
        assert_eq!(parsed.len(), 2 * origins.len());
        assert_eq!(parsed, permissions.entries);
        assert_eq!(serialize(&parsed), text);

        // WebKit gives an IPv6 host without brackets. The key is the same as the stored one.
        assert_eq!(origin("http", "::1", 8080).as_deref(), Some(origins[3]));
        assert_eq!(
            origin("https", "2001:DB8::1", 443).as_deref(),
            Some(origins[6])
        );

        // Another spelling of an address, a plain HTTP address that is not loopback and a
        // broken bracket are not in the exact form: such a line never becomes a decision.
        for line in [
            "allow\tcamera\thttp://[0:0:0:0:0:0:0:1]",
            "allow\tcamera\thttp://[::1]:80",
            "allow\tcamera\thttp://::1",
            "allow\tcamera\thttp://[::1",
            "allow\tcamera\thttp://[2001:db8::1]",
            "allow\tcamera\thttp://[::ffff:192.168.1.10]",
            "allow\tcamera\thttp://192.168.1.10",
            "allow\tcamera\thttp://127.0.0.1.example.com",
            "allow\tcamera\thttp://LOCALHOST",
        ] {
            assert!(parse(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn malformed_lines_are_skipped_and_never_become_a_grant() {
        let text = "# comment\n\
            allow\tcamera\thttps://good.example\n\
            grant\tcamera\thttps://a.example\n\
            allow\tscreen\thttps://a.example\n\
            allow\tcamera\n\
            allow\n\
            \n\
            allow\tcamera\thttp://plain.example\n\
            allow\tcamera\thttps://a.example/path\n\
            allow\tcamera\thttps://A.example\n\
            allow\tcamera\thttps://a.example:443\n\
            allow\tcamera\thttps://a.example\textra\n\
            allow\tcamera\tfile:///etc/passwd\n\
            allow camera https://a.example\n\
            \u{0}\u{1}binary\n\
            deny\tmicrophone\thttps://good.example";
        let entries = parse(text);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[&("https://good.example".to_owned(), Kind::Camera)],
            Decision::Allow
        );
        assert_eq!(
            entries[&("https://good.example".to_owned(), Kind::Microphone)],
            Decision::Deny
        );
        assert!(parse("").is_empty());
        // A later line for the same origin and kind wins.
        let entries = parse("allow\tcamera\thttps://a.example\ndeny\tcamera\thttps://a.example");
        assert_eq!(entries.into_values().collect::<Vec<_>>(), [Decision::Deny]);
        let many: String = (0..MAX_ENTRIES + 50)
            .map(|index| format!("deny\tcamera\thttps://site{index}.example\n"))
            .collect();
        assert_eq!(parse(&many).len(), MAX_ENTRIES);
    }

    #[test]
    fn files_load_save_and_stay_untouched_when_unreadable() {
        let dir = std::env::temp_dir().join(format!("felinaut-permissions-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permissions.tsv");

        // No file yet: no decisions, and nothing to write until one is made.
        let mut permissions = Permissions::load(Some(path.clone()));
        assert_eq!(permissions.answer(SITE, BOTH), Answer::Ask);
        assert!(permissions.take_snapshot().is_none());

        permissions.remember(SITE, BOTH, Decision::Allow);
        let now = Instant::now();
        let first = permissions.snapshot_due(now).unwrap();
        assert_eq!(first.path, path);
        first.write(save).unwrap();
        // A change soon after the last write waits; the write before quitting does not.
        permissions.remember(SITE, &[Kind::Camera], Decision::Deny);
        assert!(permissions.snapshot_due(now).is_none());
        assert!(permissions.take_snapshot().is_some());
        // A snapshot that nobody wrote leaves the change due.
        assert!(permissions.take_snapshot().is_some());
        assert!(permissions.snapshot_due(now + SAVE_INTERVAL).is_some());

        // The file has what was written: the first snapshot, with both allowed.
        let loaded = Permissions::load(Some(path.clone()));
        assert_eq!(loaded.answer(SITE, BOTH), Answer::Grant);

        // Not UTF-8: no decisions, and the file is never overwritten.
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let mut broken = Permissions::load(Some(path.clone()));
        assert_eq!(broken.answer(SITE, BOTH), Answer::Ask);
        broken.remember(SITE, BOTH, Decision::Allow);
        assert!(broken.take_snapshot().is_none());
        assert!(broken.snapshot_due(now).is_none());

        let mut homeless = Permissions::load(None);
        homeless.remember(SITE, BOTH, Decision::Deny);
        assert!(homeless.take_snapshot().is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_older_snapshot_never_replaces_a_newer_one() {
        let dir = std::env::temp_dir().join(format!("felinaut-perm-order-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permissions.tsv");
        let mut permissions = Permissions::load(Some(path.clone()));
        let now = Instant::now();

        // The background write has its snapshot, but it runs late: the user makes one more
        // decision and quits first.
        permissions.remember(SITE, BOTH, Decision::Allow);
        let background = permissions.snapshot_due(now).unwrap();
        permissions.remember(SITE, &[Kind::Camera], Decision::Deny);
        let quit = permissions.take_snapshot().unwrap();
        quit.write(save).unwrap();
        // The late background write does nothing: its writer must not run.
        background
            .write(|_, _| panic!("the older snapshot reached the file"))
            .unwrap();
        let loaded = Permissions::load(Some(path.clone()));
        assert_eq!(loaded.answer(SITE, BOTH), Answer::Deny);
        assert!(permissions.take_snapshot().is_none(), "nothing is left");

        // While one write runs, the UI thread gets no second snapshot and does not wait.
        permissions.revoke(SITE);
        let running = permissions.snapshot_due(now + SAVE_INTERVAL).unwrap();
        permissions.remember(SITE, BOTH, Decision::Allow);
        running
            .write(|path, contents| {
                assert!(permissions.snapshot_due(now + 2 * SAVE_INTERVAL).is_none());
                save(path, contents)
            })
            .unwrap();
        // The newer change is still due after that write.
        let last = permissions.snapshot_due(now + 2 * SAVE_INTERVAL).unwrap();
        last.write(save).unwrap();
        assert_eq!(
            Permissions::load(Some(path)).answer(SITE, BOTH),
            Answer::Grant
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_write_is_tried_again_a_bounded_number_of_times() {
        let fail = |_: &Path, _: &str| Err(io::Error::other("disk full"));
        let mut permissions = Permissions {
            path: Some("/nonexistent/permissions.tsv".into()),
            ..Permissions::default()
        };
        permissions.remember(SITE, BOTH, Decision::Allow);
        let start = Instant::now();
        let mut now = start;
        // A failure keeps the decisions due. One success ends the retries.
        assert!(permissions.snapshot_due(now).unwrap().write(fail).is_err());
        now += SAVE_INTERVAL;
        let retry = permissions.snapshot_due(now).unwrap();
        assert!(retry.contents.contains(SITE));
        retry.write(|_, _| Ok(())).unwrap();
        now += SAVE_INTERVAL;
        assert!(permissions.snapshot_due(now).is_none());
        assert!(permissions.take_snapshot().is_none());

        // After `MAX_FAILURES` failures in a row the background writes stop.
        permissions.remember(SITE, &[Kind::Camera], Decision::Deny);
        for _ in 0..MAX_FAILURES {
            now += SAVE_INTERVAL;
            assert!(permissions.snapshot_due(now).unwrap().write(fail).is_err());
        }
        now += SAVE_INTERVAL;
        assert!(permissions.snapshot_due(now).is_none());
        // The write at quit still tries, and its failure loses no state in memory.
        assert!(permissions.take_snapshot().unwrap().write(fail).is_err());
        let last = permissions.take_snapshot().unwrap();
        assert!(last.contents.contains("deny\tcamera"));
        last.write(|_, _| Ok(())).unwrap();
        assert!(permissions.take_snapshot().is_none());
    }

    #[test]
    fn long_origins_keep_their_end_in_the_prompt() {
        assert_eq!(display_origin("https://example.com"), "example.com");
        assert_eq!(
            display_origin("http://localhost:3000"),
            "http://localhost:3000"
        );
        let long = format!(
            "https://accounts.bank.example.{}.evil.example",
            "a".repeat(80)
        );
        let shown = display_origin(&long);
        assert_eq!(shown.chars().count(), 48);
        assert!(shown.starts_with('…') && shown.ends_with(".evil.example"));
        assert_eq!(describe(BOTH), "camera and microphone");
        assert_eq!(describe(&[Kind::Camera]), "camera");
    }
}
