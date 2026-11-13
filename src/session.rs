//! Open tabs, kept across launches in `Felifox/tabs.tsv` under Application Support: one
//! `flags \t url \t title` line per tab, in sidebar order. Flags: `p` pinned, `s` shown in
//! split view with the line before, `a` the active tab. Blank New Tabs are not kept. Pinned
//! tabs store their pinned URL, the others the page they were showing. The extended header
//! adds an optional JSON-string backend column; legacy files keep their original meaning.

use std::path::PathBuf;

const HEADER: &str =
    "# Felifox tabs: flags (p pinned, s split with previous, a active)\turl\ttitle";
const BACKEND_HEADER: &str =
    "# Felifox tabs: flags (p pinned, s split with previous, a active)\turl\ttitle\tbackend (JSON)";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedTab {
    pub url: String,
    pub title: String,
    pub pinned: bool,
    pub split: bool,
    pub active: bool,
    /// `None` means legacy/default WebKit. Preserve unknown IDs for explicit recovery.
    /// Malformed backend JSON is retained with an `invalid-json:` prefix, never defaulted.
    pub backend: Option<String>,
}

/// `~/Library/Application Support/Felifox/tabs.tsv`, beside the `history` folder.
pub fn default_path() -> Option<PathBuf> {
    let history = crate::history::default_path()?;
    Some(history.parent()?.parent()?.join("tabs.tsv"))
}

/// Saved tabs; lines whose URL is not HTTP(S) are skipped.
pub fn parse(text: &str) -> Vec<SavedTab> {
    let extended = text.lines().next() == Some(BACKEND_HEADER);
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.splitn(if extended { 4 } else { 3 }, '\t');
            let flags = fields.next()?;
            let url = fields.next()?.trim();
            let title = fields.next().unwrap_or("").trim();
            (url.starts_with("https://") || url.starts_with("http://")).then(|| SavedTab {
                url: url.to_owned(),
                title: title.to_owned(),
                pinned: flags.contains('p'),
                split: flags.contains('s'),
                active: flags.contains('a'),
                // Invalid JSON/types must not turn into the implicit WebKit default.
                backend: fields.next().map(|raw| {
                    serde_json::from_str::<String>(raw)
                        .unwrap_or_else(|_| format!("invalid-json:{raw}"))
                }),
            })
        })
        .collect()
}

pub fn serialize(tabs: &[SavedTab]) -> String {
    let mut out = String::from(if tabs.iter().any(|tab| tab.backend.is_some()) {
        BACKEND_HEADER
    } else {
        HEADER
    });
    for tab in tabs {
        let flags: String = [(tab.pinned, 'p'), (tab.split, 's'), (tab.active, 'a')]
            .into_iter()
            .filter_map(|(set, flag)| set.then_some(flag))
            .collect();
        let title = tab.title.replace(['\t', '\n', '\r'], " ");
        out.push('\n');
        out.push_str(&format!("{flags}\t{}\t{title}", tab.url));
        if let Some(backend) = &tab.backend {
            out.push('\t');
            // JSON escaping keeps even malformed/unknown IDs within one TSV field.
            out.push_str(&serde_json::to_string(backend).expect("strings serialize to JSON"));
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(url: &str, title: &str, pinned: bool, split: bool, active: bool) -> SavedTab {
        SavedTab {
            url: url.into(),
            title: title.into(),
            pinned,
            split,
            active,
            backend: None,
        }
    }

    #[test]
    fn round_trips_and_sanitizes() {
        let tabs = vec![
            tab(
                "https://github.com/pulls",
                "Pull\trequests\n",
                true,
                false,
                false,
            ),
            tab("https://a.b/", "A", false, false, false),
            tab("http://localhost:3000/", "", false, true, true),
        ];
        let parsed = parse(&serialize(&tabs));
        assert_eq!(parsed[0].title, "Pull requests");
        assert_eq!(parsed[1..], tabs[1..]);
        assert!(parsed[0].pinned && !parsed[0].split && !parsed[0].active);
    }

    #[test]
    fn skips_comments_and_non_web_urls() {
        let text = "# header\np\tfile:///etc/passwd\tx\n\tjavascript:alert(1)\n\n\thttps://a.b/\n";
        assert_eq!(
            parse(text),
            vec![tab("https://a.b/", "", false, false, false)]
        );
    }

    #[test]
    fn older_files_and_default_tabs_keep_webkit_implicit() {
        assert_eq!(SavedTab::default().backend, None);
        let legacy = format!("{HEADER}\na\thttps://example.test/\tOld\ttitle\n");
        let parsed = parse(&legacy);
        assert_eq!(parsed[0].backend, None);
        assert_eq!(parsed[0].title, "Old\ttitle");
        let plain = tab("https://example.test/", "Old", false, false, true);
        assert_eq!(
            serialize(&[plain]),
            format!("{HEADER}\na\thttps://example.test/\tOld\n")
        );
        let absent = format!("{BACKEND_HEADER}\n\thttps://example.test/\tOld\n");
        assert_eq!(parse(&absent)[0].backend, None);
    }

    #[test]
    fn backend_ids_round_trip_without_filtering_or_normalizing() {
        let mut tabs = vec![tab("https://example.test/", "Old", true, false, false)];
        for backend in [
            "webkit",
            "cef",
            "gecko",
            "",
            " WebKit ",
            "future\tengine\n\"\\",
        ] {
            let mut saved = tab("https://example.test/next", "Next", false, true, true);
            saved.backend = Some(backend.into());
            tabs.push(saved);
        }
        assert_eq!(parse(&serialize(&tabs)), tabs);
    }

    #[test]
    fn malformed_backend_values_remain_explicit_and_keep_tabs() {
        for raw in [
            "null",
            "true",
            "42",
            "[]",
            "{}",
            "\"unterminated",
            "",
            "cef",
            "webkit",
            "\"webkit\"\textra",
        ] {
            let text = format!("{BACKEND_HEADER}\na\thttps://example.test/\tTitle\t{raw}\n");
            let tabs = parse(&text);
            assert_eq!(tabs.len(), 1, "{raw:?}");
            assert_eq!(tabs[0].backend, Some(format!("invalid-json:{raw}")));
            assert_eq!(parse(&serialize(&tabs)), tabs);
        }
    }

    #[test]
    fn empty_sessions_and_non_web_tabs_keep_existing_behavior() {
        assert!(parse("").is_empty());
        assert!(parse(&serialize(&[])).is_empty());
        let text = format!("{BACKEND_HEADER}\na\tfile:///tmp/local\tLocal\t\"cef\"\n");
        assert!(parse(&text).is_empty());
    }

    #[test]
    fn lives_beside_history() {
        if let (Some(tabs), Some(history)) = (default_path(), crate::history::default_path()) {
            assert_eq!(tabs.parent(), history.parent().and_then(|dir| dir.parent()));
            assert_eq!(tabs.file_name().unwrap(), "tabs.tsv");
        }
    }
}
