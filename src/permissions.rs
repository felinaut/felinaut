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
    /// `None` without a home, or when the file was unreadable so it is never overwritten.
    path: Option<PathBuf>,
    dirty: bool,
    saved: Option<Instant>,
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

    /// One stored denial denies. A grant needs a stored permission for each kind.
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
        } else {
            Answer::Ask
        }
    }

    pub fn remember(&mut self, origin: &str, kinds: &[Kind], decision: Decision) {
        for kind in kinds {
            self.entries.insert((origin.to_owned(), *kind), decision);
        }
        self.dirty |= !kinds.is_empty();
    }

    /// Forgets each decision for the origin, so it gets a prompt again.
    pub fn revoke(&mut self, origin: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(known, _), _| known != origin);
        let removed = self.entries.len() != before;
        self.dirty |= removed;
        removed
    }

    /// The stored decisions for an origin in words, such as "Camera allowed".
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
            .collect();
        let text = parts.join(", ");
        let mut letters = text.chars();
        let first = letters.next()?;
        Some(first.to_uppercase().chain(letters).collect())
    }

    /// The path and contents to write if a decision changed, at most once per `SAVE_INTERVAL`
    /// so two background writes never race.
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
        Some((path, serialize(&self.entries)))
    }
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
        let (_, contents) = permissions.take_snapshot().unwrap();
        assert!(!contents.contains(SITE) && contents.contains(other));
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
        assert_eq!(permissions.take_snapshot(), None);

        permissions.remember(SITE, BOTH, Decision::Allow);
        let now = Instant::now();
        let (target, contents) = permissions.snapshot_due(now).unwrap();
        assert_eq!(target, path);
        fs::write(&path, contents).unwrap();
        // A change soon after the last write waits; the write before quitting does not.
        permissions.remember(SITE, &[Kind::Camera], Decision::Deny);
        assert_eq!(permissions.snapshot_due(now), None);
        assert!(permissions.take_snapshot().is_some());
        assert_eq!(permissions.take_snapshot(), None, "nothing changed since");

        // The file has what was written: the first snapshot, with both allowed.
        let loaded = Permissions::load(Some(path.clone()));
        assert_eq!(loaded.answer(SITE, BOTH), Answer::Grant);

        // Not UTF-8: no decisions, and the file is never overwritten.
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let mut broken = Permissions::load(Some(path.clone()));
        assert_eq!(broken.answer(SITE, BOTH), Answer::Ask);
        broken.remember(SITE, BOTH, Decision::Allow);
        assert_eq!(broken.take_snapshot(), None);

        let mut homeless = Permissions::load(None);
        homeless.remember(SITE, BOTH, Decision::Deny);
        assert_eq!(homeless.take_snapshot(), None);
        fs::remove_dir_all(&dir).unwrap();
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
