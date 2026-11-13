//! The saved SOCKS5 proxies and which one is in use: pure data, validation and the file format.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde_json::{Value, json};

pub const MAX_PROXIES: usize = 32;
/// The port SOCKS servers listen on unless told otherwise (RFC 1928).
pub const DEFAULT_PORT: u16 = 1080;
const MAX_NAME_CHARS: usize = 40;
const MAX_HOST_BYTES: usize = 253;
/// RFC 1929 sends user name and password with a one-byte length.
pub const MAX_CREDENTIAL_BYTES: usize = 255;

/// One saved SOCKS5 proxy. The password lives in the Keychain, never in this struct's file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    pub name: String,
    /// A host name, an IPv4 address, or an IPv6 address without brackets.
    pub host: String,
    pub port: u16,
    /// Present when the proxy needs RFC 1929 user name and password authentication.
    pub username: Option<String>,
}

impl Proxy {
    /// `host:port`, with brackets around an IPv6 address.
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The Keychain account under which this proxy's password is stored.
    pub fn keychain_account(&self) -> Option<String> {
        self.username
            .as_ref()
            .map(|user| format!("{user}@{}", self.address()))
    }

    fn same_endpoint(&self, other: &Proxy) -> bool {
        self.host.eq_ignore_ascii_case(&other.host)
            && self.port == other.port
            && self.username == other.username
    }
}

/// What the user typed into the Add Proxy form.
#[derive(Clone, Copy, Debug, Default)]
pub struct Draft<'a> {
    pub name: &'a str,
    pub host: &'a str,
    pub port: &'a str,
    pub username: &'a str,
    pub password: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyError {
    MissingHost,
    InvalidHost,
    InvalidPort,
    /// The host field says one port and the port field another.
    ConflictingPort,
    NameTooLong,
    InvalidName,
    InvalidUsername,
    InvalidPassword,
    PasswordWithoutUsername,
    Duplicate,
    TooMany,
}

impl ProxyError {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingHost => "Enter the proxy's host name or IP address.",
            Self::InvalidHost => "That host is not a valid host name or IP address.",
            Self::InvalidPort => "The port must be a number from 1 to 65535.",
            Self::ConflictingPort => "The host and the port field name different ports.",
            Self::NameTooLong => "Keep the name to 40 characters or fewer.",
            Self::InvalidName => "The name cannot contain control characters.",
            Self::InvalidUsername => {
                "The user name must be 1–255 bytes without control characters."
            }
            Self::InvalidPassword => {
                "The password must be 255 bytes or fewer without control characters."
            }
            Self::PasswordWithoutUsername => "A password needs a user name.",
            Self::Duplicate => "This proxy is already saved.",
            Self::TooMany => "You can save up to 32 proxies. Remove one first.",
        }
    }
}

/// Turn the form into a proxy, or say what is wrong with it.
pub fn validate(draft: &Draft, existing: &[Proxy]) -> Result<Proxy, ProxyError> {
    if existing.len() >= MAX_PROXIES {
        return Err(ProxyError::TooMany);
    }
    let (host, embedded_port) = parse_host(draft.host)?;
    let typed_port = match draft.port.trim() {
        "" => None,
        port => Some(parse_port(port)?),
    };
    let port = match (embedded_port, typed_port) {
        (Some(a), Some(b)) if a != b => return Err(ProxyError::ConflictingPort),
        (Some(port), _) | (None, Some(port)) => port,
        (None, None) => DEFAULT_PORT,
    };

    let username = match draft.username.trim() {
        "" => None,
        user if user.len() > MAX_CREDENTIAL_BYTES || has_control(user) => {
            return Err(ProxyError::InvalidUsername);
        }
        user => Some(user.to_owned()),
    };
    // Passwords are taken exactly as typed: spaces may be part of them.
    if draft.password.len() > MAX_CREDENTIAL_BYTES || has_control(draft.password) {
        return Err(ProxyError::InvalidPassword);
    }
    if username.is_none() && !draft.password.is_empty() {
        return Err(ProxyError::PasswordWithoutUsername);
    }

    let mut proxy = Proxy {
        name: String::new(),
        host,
        port,
        username,
    };
    proxy.name = match draft.name.trim() {
        "" => proxy.address(),
        name if has_control(name) => return Err(ProxyError::InvalidName),
        name if name.chars().count() > MAX_NAME_CHARS => return Err(ProxyError::NameTooLong),
        name => name.to_owned(),
    };
    if existing.iter().any(|saved| saved.same_endpoint(&proxy)) {
        return Err(ProxyError::Duplicate);
    }
    Ok(proxy)
}

fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

fn parse_port(text: &str) -> Result<u16, ProxyError> {
    match text.parse::<u16>() {
        Ok(port) if port > 0 && text.bytes().all(|b| b.is_ascii_digit()) => Ok(port),
        _ => Err(ProxyError::InvalidPort),
    }
}

/// Accept `host`, `host:port`, `[v6]`, `[v6]:port`, a bare IPv6 address, and an optional
/// `socks5://` or `socks5h://` prefix, as people paste them from provider dashboards.
fn parse_host(text: &str) -> Result<(String, Option<u16>), ProxyError> {
    let mut text = text.trim();
    for scheme in ["socks5h://", "socks5://", "socks://"] {
        if text.len() >= scheme.len() && text[..scheme.len()].eq_ignore_ascii_case(scheme) {
            text = &text[scheme.len()..];
            break;
        }
    }
    let text = text.strip_suffix('/').unwrap_or(text);
    if text.is_empty() {
        return Err(ProxyError::MissingHost);
    }
    // Other schemes (`http://`), paths and user info are not part of a SOCKS address.
    if text.contains(['/', '@']) || text.chars().any(char::is_whitespace) {
        return Err(ProxyError::InvalidHost);
    }

    if let Some(rest) = text.strip_prefix('[') {
        let (inside, after) = rest.split_once(']').ok_or(ProxyError::InvalidHost)?;
        let address = inside
            .parse::<Ipv6Addr>()
            .map_err(|_| ProxyError::InvalidHost)?;
        let port = match after {
            "" => None,
            _ => Some(parse_port(
                after.strip_prefix(':').ok_or(ProxyError::InvalidHost)?,
            )?),
        };
        return Ok((address.to_string(), port));
    }
    if text.matches(':').count() > 1 {
        let address = text
            .parse::<Ipv6Addr>()
            .map_err(|_| ProxyError::InvalidHost)?;
        return Ok((address.to_string(), None));
    }
    let (host, port) = match text.split_once(':') {
        Some((host, port)) => (host, Some(parse_port(port)?)),
        None => (text, None),
    };
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.parse::<Ipv4Addr>().is_ok() || is_host_name(host) {
        Ok((host.to_ascii_lowercase(), port))
    } else {
        Err(ProxyError::InvalidHost)
    }
}

fn is_host_name(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST_BYTES
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        // A name made only of digits and dots is a malformed IPv4 address, not a host.
        && !host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

/// The saved proxies and the one all pages use, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyList {
    pub proxies: Vec<Proxy>,
    pub active: Option<usize>,
}

impl ProxyList {
    pub fn active_proxy(&self) -> Option<&Proxy> {
        self.active.and_then(|index| self.proxies.get(index))
    }

    /// Save a validated proxy and return its index.
    pub fn add(&mut self, proxy: Proxy) -> usize {
        self.proxies.push(proxy);
        self.proxies.len() - 1
    }

    /// Use proxy `index`, or none. Returns whether anything changed; unknown indices change
    /// nothing.
    pub fn select(&mut self, index: Option<usize>) -> bool {
        if index.is_some_and(|index| index >= self.proxies.len()) || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Forget proxy `index`. Removing the proxy in use switches pages back to a direct
    /// connection; the others keep their selection.
    pub fn remove(&mut self, index: usize) -> Option<Proxy> {
        if index >= self.proxies.len() {
            return None;
        }
        self.active = match self.active {
            Some(active) if active == index => None,
            Some(active) if active > index => Some(active - 1),
            active => active,
        };
        Some(self.proxies.remove(index))
    }

    /// The file format: versioned JSON without passwords.
    pub fn serialize(&self) -> String {
        let proxies: Vec<Value> = self
            .proxies
            .iter()
            .map(|proxy| {
                let mut entry = json!({
                    "name": proxy.name,
                    "host": proxy.host,
                    "port": proxy.port,
                });
                if let Some(user) = &proxy.username {
                    entry["username"] = json!(user);
                }
                entry
            })
            .collect();
        let file = json!({ "version": 1, "active": self.active, "proxies": proxies });
        format!("{file:#}\n")
    }

    /// Read the file leniently: entries that fail today's validation are dropped, and the
    /// selection is kept only if its entry survives. Anything unreadable is an empty list.
    pub fn parse(text: &str) -> Self {
        let Ok(file) = serde_json::from_str::<Value>(text) else {
            return Self::default();
        };
        let active = file["active"].as_u64();
        let mut list = Self::default();
        let Some(entries) = file["proxies"].as_array() else {
            return list;
        };
        for (index, entry) in entries.iter().enumerate() {
            let port = entry["port"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|&port| port > 0);
            let (Some(host), Some(port)) = (entry["host"].as_str(), port) else {
                continue;
            };
            let draft = Draft {
                name: entry["name"].as_str().unwrap_or(""),
                host,
                port: &port.to_string(),
                username: entry["username"].as_str().unwrap_or(""),
                password: "",
            };
            if let Ok(proxy) = validate(&draft, &list.proxies) {
                let position = list.add(proxy);
                if active == Some(index as u64) {
                    list.active = Some(position);
                }
            }
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(host: &str) -> Draft<'_> {
        Draft {
            host,
            ..Draft::default()
        }
    }

    fn proxy(host: &str, port: u16) -> Proxy {
        validate(
            &Draft {
                host,
                port: &port.to_string(),
                ..Draft::default()
            },
            &[],
        )
        .unwrap()
    }

    #[test]
    fn accepts_the_ways_people_write_a_socks_address() {
        let cases = [
            ("proxy.example.com", "proxy.example.com", 1080),
            ("  Proxy.Example.COM.  ", "proxy.example.com", 1080),
            ("proxy.example.com:9050", "proxy.example.com", 9050),
            ("socks5://10.0.0.2:1081", "10.0.0.2", 1081),
            (
                "SOCKS5H://de-fra_1.example.net:443/",
                "de-fra_1.example.net",
                443,
            ),
            ("socks://localhost", "localhost", 1080),
            ("127.0.0.1", "127.0.0.1", 1080),
            ("[::1]:1086", "::1", 1086),
            ("[2001:DB8::1]", "2001:db8::1", 1080),
            ("2001:db8::7", "2001:db8::7", 1080),
        ];
        for (typed, host, port) in cases {
            let proxy = validate(&draft(typed), &[]).unwrap_or_else(|e| panic!("{typed}: {e:?}"));
            assert_eq!((proxy.host.as_str(), proxy.port), (host, port), "{typed}");
            assert_eq!(proxy.username, None);
        }
    }

    #[test]
    fn rejects_hosts_that_are_not_hosts() {
        assert_eq!(validate(&draft(""), &[]), Err(ProxyError::MissingHost));
        assert_eq!(
            validate(&draft("  socks5:// "), &[]),
            Err(ProxyError::MissingHost)
        );
        for typed in [
            "user@proxy.example.com",
            "proxy.example.com/path",
            "proxy example.com",
            "-proxy.example.com",
            "proxy-.example.com",
            "proxy..example.com",
            "pröxy.example.com",
            "http://proxy.example.com",
            "999.1.1.1",
            "1.2.3",
            "[::1",
            "[::1]1080",
            "[not-v6]:1080",
            "1:2:3",
            "a\u{0}b",
        ] {
            assert_eq!(
                validate(&draft(typed), &[]),
                Err(ProxyError::InvalidHost),
                "{typed:?}"
            );
        }
        let label = "a".repeat(63);
        assert!(validate(&draft(&format!("{label}.example")), &[]).is_ok());
        assert_eq!(
            validate(&draft(&format!("a{label}.example")), &[]),
            Err(ProxyError::InvalidHost)
        );
        let long = vec!["a".repeat(50); 5].join(".");
        assert!(long.len() > MAX_HOST_BYTES);
        assert_eq!(validate(&draft(&long), &[]), Err(ProxyError::InvalidHost));
    }

    #[test]
    fn ports_are_checked_and_must_agree() {
        for (port, expected) in [
            ("1", Ok(1)),
            ("65535", Ok(65535)),
            (" 1080 ", Ok(1080)),
            ("0", Err(ProxyError::InvalidPort)),
            ("65536", Err(ProxyError::InvalidPort)),
            ("-1", Err(ProxyError::InvalidPort)),
            ("+80", Err(ProxyError::InvalidPort)),
            ("80a", Err(ProxyError::InvalidPort)),
        ] {
            let result = validate(
                &Draft {
                    host: "proxy.example",
                    port,
                    ..Draft::default()
                },
                &[],
            );
            assert_eq!(result.map(|proxy| proxy.port), expected, "{port:?}");
        }
        let both = |port| Draft {
            host: "proxy.example:1081",
            port,
            ..Draft::default()
        };
        assert_eq!(validate(&both("1081"), &[]).unwrap().port, 1081);
        assert_eq!(
            validate(&both("1082"), &[]),
            Err(ProxyError::ConflictingPort)
        );
        assert_eq!(
            validate(&draft("proxy.example:0"), &[]),
            Err(ProxyError::InvalidPort)
        );
        assert_eq!(
            validate(&draft("proxy.example:"), &[]),
            Err(ProxyError::InvalidPort)
        );
    }

    #[test]
    fn credentials_follow_rfc_1929_limits() {
        let with = |username, password| Draft {
            host: "proxy.example",
            username,
            password,
            ..Draft::default()
        };
        let proxy = validate(&with("  alice ", " secret "), &[]).unwrap();
        assert_eq!(proxy.username.as_deref(), Some("alice"));
        assert_eq!(
            proxy.keychain_account().as_deref(),
            Some("alice@proxy.example:1080")
        );
        assert!(validate(&with("alice", ""), &[]).is_ok());

        let max = "x".repeat(MAX_CREDENTIAL_BYTES);
        let over = "x".repeat(MAX_CREDENTIAL_BYTES + 1);
        assert!(validate(&with(&max, &max), &[]).is_ok());
        assert_eq!(
            validate(&with(&over, ""), &[]),
            Err(ProxyError::InvalidUsername)
        );
        assert_eq!(
            validate(&with("alice", &over), &[]),
            Err(ProxyError::InvalidPassword)
        );
        assert_eq!(
            validate(&with("al\tice", ""), &[]),
            Err(ProxyError::InvalidUsername)
        );
        assert_eq!(
            validate(&with("alice", "pa\nss"), &[]),
            Err(ProxyError::InvalidPassword)
        );
        assert_eq!(
            validate(&with("", "secret"), &[]),
            Err(ProxyError::PasswordWithoutUsername)
        );
        assert_eq!(proxy_without_user().keychain_account(), None);
    }

    fn proxy_without_user() -> Proxy {
        proxy("proxy.example", 1080)
    }

    #[test]
    fn names_default_to_the_address_and_are_bounded() {
        assert_eq!(validate(&draft("[::1]:9"), &[]).unwrap().name, "[::1]:9");
        let named = |name| Draft {
            name,
            host: "proxy.example",
            ..Draft::default()
        };
        assert_eq!(
            validate(&named("  Frankfurt  "), &[]).unwrap().name,
            "Frankfurt"
        );
        let forty = "é".repeat(MAX_NAME_CHARS);
        assert_eq!(validate(&named(&forty), &[]).unwrap().name, forty);
        let more = "é".repeat(MAX_NAME_CHARS + 1);
        assert_eq!(validate(&named(&more), &[]), Err(ProxyError::NameTooLong));
        assert_eq!(
            validate(&named("a\u{7}b"), &[]),
            Err(ProxyError::InvalidName)
        );
    }

    #[test]
    fn duplicates_and_the_limit_are_refused() {
        let saved = vec![proxy("proxy.example", 1080)];
        assert_eq!(
            validate(&draft("PROXY.example:1080"), &saved),
            Err(ProxyError::Duplicate)
        );
        assert!(validate(&draft("proxy.example:1081"), &saved).is_ok());
        let as_user = Draft {
            host: "proxy.example",
            username: "bob",
            ..Draft::default()
        };
        assert!(
            validate(&as_user, &saved).is_ok(),
            "another account is another proxy"
        );

        let full: Vec<_> = (1..=MAX_PROXIES as u16)
            .map(|port| proxy("p.example", port))
            .collect();
        assert_eq!(
            validate(&draft("other.example"), &full),
            Err(ProxyError::TooMany)
        );
        assert!(validate(&draft("other.example"), &full[1..]).is_ok());
    }

    #[test]
    fn every_error_has_a_message() {
        use ProxyError::*;
        for error in [
            MissingHost,
            InvalidHost,
            InvalidPort,
            ConflictingPort,
            NameTooLong,
            InvalidName,
            InvalidUsername,
            InvalidPassword,
            PasswordWithoutUsername,
            Duplicate,
            TooMany,
        ] {
            assert!(error.message().ends_with('.'), "{error:?}");
        }
    }

    fn list(count: u16, active: Option<usize>) -> ProxyList {
        ProxyList {
            proxies: (1..=count).map(|port| proxy("p.example", port)).collect(),
            active,
        }
    }

    #[test]
    fn selecting_ignores_unknown_and_unchanged_choices() {
        let mut proxies = list(2, None);
        assert!(!proxies.select(None));
        assert!(!proxies.select(Some(2)));
        assert_eq!(proxies.active_proxy(), None);
        assert!(proxies.select(Some(1)));
        assert_eq!(proxies.active_proxy().unwrap().port, 2);
        assert!(!proxies.select(Some(1)));
        assert!(proxies.select(None));
        assert_eq!(proxies.active, None);
        assert!(!ProxyList::default().select(Some(0)));
    }

    #[test]
    fn removing_keeps_the_selection_on_the_same_proxy() {
        // Before the active one: it shifts down.
        let mut proxies = list(3, Some(2));
        assert_eq!(proxies.remove(0).unwrap().port, 1);
        assert_eq!(proxies.active_proxy().unwrap().port, 3);
        // After it: unchanged.
        let mut proxies = list(3, Some(0));
        proxies.remove(2);
        assert_eq!(proxies.active_proxy().unwrap().port, 1);
        // The active one: back to a direct connection.
        let mut proxies = list(3, Some(1));
        proxies.remove(1);
        assert_eq!(proxies.active, None);
        // Unknown index: nothing happens.
        let mut proxies = list(1, Some(0));
        assert_eq!(proxies.remove(1), None);
        assert_eq!(proxies, list(1, Some(0)));
        let mut proxies = list(1, None);
        proxies.remove(0);
        assert_eq!(proxies, ProxyList::default());
    }

    #[test]
    fn file_round_trips_without_passwords() {
        let mut proxies = list(2, Some(1));
        let with_user = validate(
            &Draft {
                name: "Zürich \"exit\"",
                host: "[2001:db8::2]:1081",
                username: "alice",
                password: "never-written",
                ..Draft::default()
            },
            &proxies.proxies,
        )
        .unwrap();
        proxies.add(with_user);
        let text = proxies.serialize();
        assert!(!text.contains("never-written"));
        assert!(text.contains("\"version\": 1"));
        assert_eq!(ProxyList::parse(&text), proxies);
        assert_eq!(
            ProxyList::parse(&ProxyList::default().serialize()),
            ProxyList::default()
        );
    }

    #[test]
    fn unreadable_files_are_an_empty_list() {
        for text in ["", "{", "[]", "null", "{\"proxies\": 3}", "\u{feff}garbage"] {
            assert_eq!(ProxyList::parse(text), ProxyList::default(), "{text:?}");
        }
    }

    #[test]
    fn invalid_entries_are_dropped_and_the_selection_follows_its_entry() {
        let text = r#"{
            "active": 3,
            "proxies": [
                {"name": "ok", "host": "a.example", "port": 1080},
                {"name": "bad host", "host": "a b", "port": 1080},
                {"name": "bad port", "host": "b.example", "port": 70000},
                {"name": "chosen", "host": "c.example", "port": 1081, "username": "u"},
                {"name": "duplicate", "host": "A.example", "port": 1080},
                {"name": "no port", "host": "d.example"},
                "not an object"
            ]
        }"#;
        let parsed = ProxyList::parse(text);
        let names: Vec<_> = parsed.proxies.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["ok", "chosen"]);
        assert_eq!(parsed.active_proxy().unwrap().name, "chosen");

        let dropped = text.replace("\"active\": 3", "\"active\": 1");
        assert_eq!(ProxyList::parse(&dropped).active, None);
        for active in ["99", "-1", "\"0\"", "null"] {
            let text = text.replace("\"active\": 3", &format!("\"active\": {active}"));
            assert_eq!(ProxyList::parse(&text).active, None, "{active}");
        }
    }

    #[test]
    fn files_without_a_version_key_still_load() {
        let text = r#"{"active": 0, "proxies": [{"host": "a.example", "port": 9050}]}"#;
        let parsed = ProxyList::parse(text);
        assert_eq!(parsed.active_proxy().unwrap().name, "a.example:9050");
    }
}
