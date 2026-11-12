//! Live folders: premade integrations that list pages from a site in the sidebar, refreshed in
//! a hidden page that shares the user's WebKit logins, so no token is needed. GitHub lists open
//! pull requests that ask for your review, that you wrote and that mention you; Asana lists the
//! incomplete tasks assigned to you, and those due within a week.
//!
//! Kept in `Felifox/live.tsv` under Application Support: `F \t integration \t open` per folder,
//! `S \t section \t open \t shown` per section, then `I \t seen \t url \t updated \t dismissed
//! \t detail \t title` per item it showed. `dismissed` is the `updated` time the user closed it at.

use std::path::PathBuf;
use std::time::{Duration, Instant};

const HEADER: &str = "# Felifox live folders: F\tintegration\topen; S\tsection\topen\tshown; I\tseen\turl\tupdated\tdismissed\tdetail\ttitle";
pub const MAX_ITEMS: usize = 50;
const MAX_TITLE: usize = 200;
const EVERY: Duration = Duration::from_secs(5 * 60);
/// While signed out, check again this often in case the user signed in elsewhere.
const SIGNED_OUT_EVERY: Duration = Duration::from_secs(60);

/// One premade integration, as data: everything it does on the page is in its sources'
/// extractors. Adding one is a file in `live/` with its `SPEC`, an entry in `ALL` and an icon.
#[derive(Debug, PartialEq)]
pub struct Spec {
    /// Saved in `live.tsv`; never change it.
    pub key: &'static str,
    pub name: &'static str,
    /// What its items are, for the folder's notes: "Then your tasks show up here".
    pub noun: &'static str,
    /// One line under its name in the settings.
    pub summary: &'static str,
    /// A `brand::icon` path.
    pub icon: &'static str,
    /// Items must link here; a tab finishing a load here re-checks a signed-out folder.
    pub host: &'static str,
    /// Opens in a normal tab when the folder is signed out.
    pub sign_in_url: &'static str,
    /// Path prefixes of the site's sign-in pages, for sites that redirect a lapsed session.
    pub sign_in_paths: &'static [&'static str],
    pub sources: &'static [Source],
}

/// A folder's integration.
pub type Integration = &'static Spec;

/// One list inside a folder: the page the hidden view loads, the script that reads it, and
/// what it is called.
///
/// The extractor is one JavaScript expression, run about once a second once the page has
/// loaded, until it answers or 20 seconds pass. It returns:
/// - `null` while the list is not there yet. It may navigate the page meanwhile, as
///   `api!`'s hop does, and is run again on the next page.
/// - `'signed-out'` when the site says the session is not signed in.
/// - `[{title, url, updated, detail}]` otherwise. `url` must be on the spec's host, `updated`
///   is any text that changes when the item does, `detail` is shown in the status bar.
#[derive(Debug, PartialEq)]
pub struct Source {
    /// Saved in `live.tsv`, unique within its integration.
    pub key: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub extractor: &'static str,
}

/// An extractor for links in a rendered page. Each JavaScript expression sees `a`, the link,
/// and `row`, its closest `row` ancestor. Not ready until a link or the `empty` element is
/// there; rows matching `skip` are left out. Rows outside the viewport have no layout, so
/// titles fall back to `textContent`. The selectors go in single quotes, so they must not
/// contain one.
#[rustfmt::skip]
macro_rules! dom {
    (links: $links:expr, empty: $empty:expr, row: $row:expr, skip: $skip:expr, updated: $updated:expr, detail: $detail:expr $(,)?) => {
        concat!(
            "(() => { const links = [...document.querySelectorAll('", $links, "')]; ",
            "if (!links.length && !document.querySelector('", $empty, "')) return null; ",
            "return links.slice(0, 200).map(a => [a, a.closest('", $row, "') || a])",
            ".filter(([a, row]) => !row.matches('", $skip, "') && !row.querySelector('", $skip, "'))",
            ".map(([a, row]) => ({ title: (a.innerText || a.textContent || '').trim(), url: a.href, ",
            "updated: ", $updated, ", detail: ", $detail, " })); })()"
        )
    };
}

/// An extractor for a JSON API page. Each part is a JavaScript function of the parsed body:
/// `signed_out` says the session lapsed, `hop` may return a URL to load next (for an id the
/// first page tells), `list` gives the items and `map` turns one into `{title, url, updated,
/// detail}`.
#[rustfmt::skip]
macro_rules! api {
    (signed_out: $signed_out:expr, hop: $hop:expr, list: $list:expr, map: $map:expr $(,)?) => {
        concat!(
            "(() => { let d; try { d = JSON.parse(document.body.innerText); } catch (e) { return null; } ",
            "if ((", $signed_out, ")(d)) return 'signed-out'; ",
            "const next = (", $hop, ")(d); if (next) { location.replace(next); return null; } ",
            "return (", $list, ")(d).map(", $map, "); })()"
        )
    };
}

mod asana;
mod github;
mod state;

pub use state::restore_items;

pub static ALL: [Integration; 2] = [&github::SPEC, &asana::SPEC];

impl Spec {
    /// Whether a page the hidden view landed on is this site's sign-in page.
    pub fn is_sign_in(&self, url: &str) -> bool {
        url::Url::parse(url).is_ok_and(|url| {
            url.host_str() == Some(self.host)
                && self
                    .sign_in_paths
                    .iter()
                    .any(|prefix| url.path().starts_with(prefix))
        })
    }
}

fn from_key(key: &str) -> Option<Integration> {
    ALL.iter().copied().find(|spec| spec.key == key)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    pub url: String,
    pub seen: bool,
    /// The site's last-updated time for the item, as it gave it.
    pub updated: String,
    /// The `updated` time at which the user closed the item; it stays hidden until it changes.
    pub dismissed: Option<String>,
    /// Where it belongs, for the status bar: `owner/repo #12`, `Project · due Nov 14`.
    pub detail: String,
}

impl Item {
    pub fn shown(&self) -> bool {
        self.dismissed.is_none()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub key: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub extractor: &'static str,
    pub open: bool,
    /// Chosen in the live folder settings; hidden sections are neither shown nor fetched.
    pub shown: bool,
    pub items: Vec<Item>,
    /// Why the last refresh failed; not saved.
    pub error: Option<String>,
    /// When the last refresh ended; `None` refreshes as soon as the window is active.
    pub last: Option<Instant>,
}

impl Section {
    /// Takes a refresh's items and returns the URLs that left the list (merged or closed).
    /// Known items keep their seen and dismissed state, except that an updated item is new
    /// again, and a dismissed one comes back. The first refresh counts as seen, so a new folder is not all
    /// dots.
    pub fn update(&mut self, mut items: Vec<Item>) -> Vec<String> {
        let first = self.last.is_none() && self.items.is_empty();
        for item in &mut items {
            let old = self.items.iter().find(|old| old.url == item.url);
            state::retain_item_state(item, old, first);
        }
        let gone = self
            .items
            .iter()
            .filter(|old| !items.iter().any(|item| item.url == old.url))
            .map(|old| old.url.clone())
            .collect();
        self.items = items;
        self.error = None;
        gone
    }
    /// A failed refresh records its retry time and error without removing cached items.
    pub fn finish_refresh(
        &mut self,
        result: Result<Vec<Item>, String>,
        now: Instant,
    ) -> Vec<String> {
        let gone = match result {
            Ok(items) => self.update(items),
            Err(error) => {
                self.error = Some(error);
                Vec::new()
            }
        };
        self.last = Some(now);
        gone
    }

    pub fn shown_items(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.shown())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveFolder {
    pub integration: Integration,
    pub open: bool,
    pub sections: Vec<Section>,
    /// The last refresh landed on a sign-in page; not saved.
    pub signed_out: bool,
}

impl LiveFolder {
    pub fn new(integration: Integration) -> Self {
        Self {
            integration,
            open: true,
            sections: integration
                .sources
                .iter()
                .map(|source| Section {
                    key: source.key,
                    label: source.label,
                    url: source.url,
                    extractor: source.extractor,
                    open: true,
                    shown: true,
                    items: Vec::new(),
                    error: None,
                    last: None,
                })
                .collect(),
            signed_out: false,
        }
    }

    pub fn has_enabled_sections(&self) -> bool {
        self.sections.iter().any(|section| section.shown)
    }

    /// Just added: nothing to show until the first enabled refresh has checked the sign-in.
    pub fn checking(&self) -> bool {
        self.has_enabled_sections()
            && !self.signed_out
            && self
                .sections
                .iter()
                .filter(|section| section.shown)
                .all(|section| section.last.is_none() && section.items.is_empty())
    }

    pub fn unseen(&self) -> usize {
        self.sections
            .iter()
            .filter(|section| section.shown)
            .flat_map(|section| &section.items)
            .filter(|item| item.shown() && !item.seen)
            .count()
    }

    /// The first section whose refresh is due.
    pub fn due(&self) -> Option<usize> {
        let every = if self.signed_out {
            SIGNED_OUT_EVERY
        } else {
            EVERY
        };
        self.sections.iter().position(|section| {
            section.shown && section.last.is_none_or(|last| last.elapsed() >= every)
        })
    }

    /// Refreshes every section at the next poll.
    pub fn refresh(&mut self) {
        for section in &mut self.sections {
            section.last = None;
        }
    }

    /// The hidden page met a sign-in page: hold every section until the next check.
    pub fn sign_out(&mut self, now: Instant) {
        self.signed_out = true;
        for section in &mut self.sections {
            section.last = Some(now);
        }
    }
}

/// What an extractor returns, as JSON, when the site says the session is not signed in.
pub const SIGNED_OUT: &str = "\"signed-out\"";

/// Turns the extractor's JSON into items: web links on `host` only, each once, at most 50,
/// titles trimmed to 200 characters. `None` while the page has not rendered its list.
pub fn parse_items(json: &str, host: &str) -> Option<Vec<Item>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let mut items: Vec<Item> = Vec::new();
    for value in values {
        let Some(Ok(mut url)) = value["url"].as_str().map(url::Url::parse) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") || url.host_str() != Some(host) {
            continue;
        }
        url.set_fragment(None);
        let url = url.to_string();
        if items.iter().any(|item| item.url == url) {
            continue;
        }
        let title = clip(&value["title"]);
        items.push(Item {
            title: if title.is_empty() { url.clone() } else { title },
            url,
            seen: false,
            detail: clip(&value["detail"]),
            updated: value["updated"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect(),
            dismissed: None,
        });
        if items.len() == MAX_ITEMS {
            break;
        }
    }
    Some(items)
}

/// A JSON string as one line of at most 200 characters; anything else is empty.
fn clip(value: &serde_json::Value) -> String {
    let text = value.as_str().unwrap_or("").split_whitespace();
    text.collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE)
        .collect()
}

/// `~/Library/Application Support/Felifox/live.tsv`, beside `tabs.tsv`.
pub fn default_path() -> Option<PathBuf> {
    Some(crate::session::default_path()?.with_file_name("live.tsv"))
}

pub fn parse(text: &str) -> Vec<LiveFolder> {
    let mut folders: Vec<LiveFolder> = Vec::new();
    // The section that `I` lines belong to; `None` after an unknown section.
    let mut section: Option<usize> = None;
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["F", key, rest @ ..] => {
                section = None;
                let Some(integration) = from_key(key) else {
                    continue;
                };
                if folders.iter().any(|f| f.integration == integration) {
                    continue;
                }
                let mut folder = LiveFolder::new(integration);
                folder.open = rest.first() != Some(&"0");
                folders.push(folder);
            }
            ["S", key, rest @ ..] => {
                let folder = folders.last_mut();
                section = folder.and_then(|folder| {
                    let index = folder.sections.iter().position(|s| s.key == *key)?;
                    folder.sections[index].open = rest.first() != Some(&"0");
                    folder.sections[index].shown = rest.get(1) != Some(&"0");
                    Some(index)
                });
            }
            ["I", seen, url, updated, dismissed, rest @ ..] => {
                let (Some(folder), Some(index)) = (folders.last_mut(), section) else {
                    continue;
                };
                let host = folder.integration.host;
                // Files from before `detail` have the title right after `dismissed`.
                let (detail, title) = match rest {
                    [detail, title, ..] => (*detail, *title),
                    [title] => ("", *title),
                    [] => ("", ""),
                };
                let line = |text: &str| text.chars().take(MAX_TITLE).collect::<String>();
                let items = &mut folder.sections[index].items;
                let on_host = url::Url::parse(url).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host_str() == Some(host)
                });
                if items.len() < MAX_ITEMS && on_host {
                    items.push(Item {
                        title: line(title),
                        detail: line(detail),
                        url: (*url).to_owned(),
                        seen: *seen == "1",
                        updated: (*updated).to_owned(),
                        dismissed: (*dismissed != "-").then(|| (*dismissed).to_owned()),
                    });
                }
            }
            _ => {}
        }
    }
    folders
}

pub fn serialize(folders: &[LiveFolder]) -> String {
    let clean = |text: &str| text.replace(['\t', '\n', '\r'], " ");
    let mut out = String::from(HEADER);
    for folder in folders {
        out.push_str(&format!(
            "\nF\t{}\t{}",
            folder.integration.key,
            u8::from(folder.open)
        ));
        for section in &folder.sections {
            out.push_str(&format!(
                "\nS\t{}\t{}\t{}",
                section.key,
                u8::from(section.open),
                u8::from(section.shown)
            ));
            for item in &section.items {
                // `-` marks an item that is not dismissed; an empty field is a dismissal of an
                // item without an updated time.
                out.push_str(&format!(
                    "\nI\t{}\t{}\t{}\t{}\t{}\t{}",
                    u8::from(item.seen),
                    clean(&item.url),
                    clean(&item.updated),
                    item.dismissed.as_deref().map_or("-".into(), clean),
                    clean(&item.detail),
                    clean(&item.title)
                ));
            }
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(url: &str, title: &str, seen: bool) -> Item {
        Item {
            title: title.into(),
            url: url.into(),
            seen,
            updated: String::new(),
            dismissed: None,
            detail: String::new(),
        }
    }

    fn updated(url: &str, at: &str) -> Item {
        Item {
            updated: at.into(),
            ..item(url, "t", false)
        }
    }

    #[test]
    fn round_trips_and_sanitizes() {
        let mut github = LiveFolder::new(&github::SPEC);
        github.open = false;
        github.sections[1].open = false;
        github.sections[2].shown = false;
        github.sections[0].items = vec![item("https://github.com/a/b/pull/1", "Fix\tit", true)];
        github.sections[1].items = vec![
            Item {
                dismissed: Some("2026-01-01T00:00:00Z".into()),
                ..updated("https://github.com/a/b/pull/3", "2026-01-01T00:00:00Z")
            },
            Item {
                dismissed: Some(String::new()),
                ..item("https://github.com/a/b/pull/4", "", true)
            },
        ];
        github.sections[2].items = vec![item("https://github.com/a/b/pull/2", "New", false)];
        let parsed = parse(&serialize(std::slice::from_ref(&github)));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].sections[0].items[0].title, "Fix it");
        github.sections[0].items[0].title = "Fix it".into();
        assert_eq!(parsed[0], github);
    }

    #[test]
    fn skips_unknown_and_foreign_lines() {
        let text = "# x\nF\tgitlab\t1\nS\treviews\t1\t1\nI\t1\thttps://github.com/x\t\t-\tx\nF\tgithub\t1\nS\tnope\t1\t1\nI\t1\thttps://github.com/y\t\t-\ty\nS\tmine\t0\nI\t0\tjavascript:alert(1)\t\t-\tx\nI\t0\thttps://evil.example/\t\t-\tx\nI\t0\thttps://github.com/a/b/pull/1\t\t-\tOne\nI\t0\thttps://github.com/a/b/pull/2\tOld format\nF\tgithub\t1\n";
        let parsed = parse(text);
        assert_eq!(parsed.len(), 1);
        let sections = &parsed[0].sections;
        assert!(sections[0].items.is_empty() && sections[2].items.is_empty());
        assert!(!sections[1].open && sections[1].shown);
        assert_eq!(
            sections[1].items,
            vec![item("https://github.com/a/b/pull/1", "One", false)]
        );
    }

    #[test]
    fn keeps_own_host_web_links_bounded() {
        let long = "x ".repeat(300);
        let json = serde_json::json!([
            {"title": "  Fix\n  the   bug ", "url": "https://github.com/a/b/pull/1#top"},
            {"title": "dup", "url": "https://github.com/a/b/pull/1"},
            {"title": "x", "url": "javascript:alert(1)"},
            {"title": "x", "url": "https://gist.github.com/1"},
            {"title": "", "url": "https://github.com/a/b/pull/2"},
            {"title": long, "url": "https://github.com/a/b/pull/3"},
            {"title": "no url"},
        ])
        .to_string();
        let items = parse_items(&json, "github.com").unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "Fix the bug");
        assert_eq!(items[0].url, "https://github.com/a/b/pull/1");
        assert_eq!(items[1].title, "https://github.com/a/b/pull/2");
        assert_eq!(items[2].title.chars().count(), 200);
        let many: Vec<_> = (0..80)
            .map(|n| serde_json::json!({"title": "t", "url": format!("https://github.com/{n}")}))
            .collect();
        let many = serde_json::to_string(&many).unwrap();
        assert_eq!(parse_items(&many, "github.com").unwrap().len(), 50);
        // Not rendered yet.
        assert_eq!(parse_items("null", "github.com"), None);
        assert_eq!(parse_items("[]", "github.com"), Some(Vec::new()));
    }

    #[test]
    fn new_items_are_unseen_after_the_first_refresh() {
        let mut section = LiveFolder::new(&github::SPEC).sections.remove(0);
        section.update(vec![item("https://github.com/1", "a", false)]);
        assert!(section.items[0].seen);
        section.last = Some(Instant::now());
        section.update(vec![
            item("https://github.com/1", "a", false),
            item("https://github.com/2", "b", false),
        ]);
        assert!(section.items[0].seen && !section.items[1].seen);
    }

    #[test]
    fn closed_items_return_only_when_updated() {
        let mut section = LiveFolder::new(&github::SPEC).sections.remove(0);
        let (a, b) = (
            "https://github.com/a/b/pull/1",
            "https://github.com/a/b/pull/2",
        );
        section.update(vec![updated(a, "1"), updated(b, "1")]);
        section.last = Some(Instant::now());
        section.items[0].dismissed = Some("1".into());
        assert_eq!(section.shown_items().count(), 1);
        assert!(
            section
                .update(vec![updated(a, "1"), updated(b, "1")])
                .is_empty()
        );
        assert!(!section.items[0].shown(), "still closed without an update");
        section.update(vec![updated(a, "2"), updated(b, "1")]);
        assert!(section.items[0].shown() && !section.items[0].seen);
        // An update to an item still listed makes it new again; no time is no update.
        section.items[1].seen = true;
        section.update(vec![updated(a, "2"), updated(b, "3")]);
        assert!(!section.items[1].seen);
        section.items[1].seen = true;
        section.update(vec![updated(a, "2"), updated(b, "")]);
        assert!(section.items[1].seen);
        section.update(vec![updated(a, "2"), updated(b, "3")]);
        // A merged pull request leaves the list.
        assert_eq!(section.update(vec![updated(a, "2")]), vec![b.to_owned()]);
    }

    #[test]
    fn checks_sign_in_then_schedules_sections() {
        let mut folder = LiveFolder::new(&github::SPEC);
        assert!(folder.checking());
        assert_eq!(folder.due(), Some(0));
        folder.sign_out(Instant::now());
        assert!(!folder.checking() && folder.due().is_none());
        folder.signed_out = false;
        folder.refresh();
        assert!(folder.checking());
        folder.sections[2].items = vec![item("https://github.com/a/b/pull/1", "a", true)];
        assert!(!folder.checking(), "a refresh keeps showing what it had");
        assert_eq!(folder.due(), Some(0));
        folder.sections[0].last = Some(Instant::now());
        assert_eq!(folder.due(), Some(1));
        assert!(github::SPEC.is_sign_in("https://github.com/login?return_to=%2Fpulls"));
        assert!(!github::SPEC.is_sign_in("https://github.com/pulls/search?q=x"));
        assert!(
            !asana::SPEC.is_sign_in("https://github.com/login"),
            "another site's sign-in"
        );
    }

    #[test]
    fn asana_is_a_second_integration() {
        let text = "F\tasana\t1\nS\tmine\t1\t1\nI\t0\thttps://app.asana.com/1/2/task/3\t2026-01-01\t-\tShip\nI\t0\thttps://github.com/a/b/pull/1\t\t-\tPR\nF\tgithub\t1\n";
        let parsed = parse(text);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].integration, &asana::SPEC);
        assert_eq!(parsed[0].sections.len(), 2);
        assert_eq!(
            parsed[0].sections[1].items.len(),
            1,
            "only app.asana.com links"
        );
        let json = r#"[{"title":"Ship","url":"https://app.asana.com/1/2/task/3","updated":"2026-01-01T00:00:00Z"}]"#;
        assert_eq!(parse_items(json, asana::SPEC.host).unwrap().len(), 1);
        assert_eq!(parse_items(SIGNED_OUT, "app.asana.com"), None);
        assert!(asana::SPEC.is_sign_in("https://app.asana.com/-/login"));
        for source in asana::SPEC.sources {
            assert!(
                source.extractor.contains("signed-out")
                    && source.url.starts_with("https://app.asana.com/")
            );
        }
    }

    #[test]
    fn builders_make_one_expression_each() {
        for spec in ALL {
            for source in spec.sources {
                let script = source.extractor;
                assert!(
                    script.starts_with("(() => {") && script.ends_with("})()"),
                    "{script}"
                );
                assert!(!script.contains("r#"), "{script}");
                let balance =
                    |open, close| script.matches(open).count() == script.matches(close).count();
                assert!(
                    balance('(', ')') && balance('{', '}') && balance('[', ']'),
                    "{script}"
                );
            }
        }
        assert!(
            github::SPEC.sources[0]
                .extractor
                .contains("querySelectorAll('a[data-testid=\"listitem-title-link\"]")
        );
    }

    #[test]
    fn missing_timestamps_survive_refresh_and_persistence_without_reopening() {
        let url = "https://github.com/a/b/pull/1";
        let mut folder = LiveFolder::new(&github::SPEC);
        let now = Instant::now();
        let section = &mut folder.sections[0];
        section.finish_refresh(Ok(vec![updated(url, "1")]), now);
        section.items[0].dismissed = Some("1".into());
        section.finish_refresh(Ok(vec![updated(url, "")]), now);
        assert_eq!(section.shown_items().count(), 0);
        assert_eq!(section.items[0].updated, "1");
        let mut restored = parse(&serialize(&[folder]));
        let section = &mut restored[0].sections[0];
        section.finish_refresh(Ok(vec![updated(url, "1")]), now);
        assert_eq!(section.shown_items().count(), 0);
        section.finish_refresh(Ok(vec![updated(url, "2")]), now);
        assert!(section.items[0].shown() && !section.items[0].seen);
    }

    #[test]
    fn explicit_reopen_restores_sidebar_items_after_close() {
        let url = "https://github.com/a/b/pull/1";
        let mut folders = vec![LiveFolder::new(&github::SPEC)];
        for section in &mut folders[0].sections {
            section.items = vec![Item {
                dismissed: Some("1".into()),
                ..updated(url, "1")
            }];
            assert_eq!(section.shown_items().count(), 0);
        }
        restore_items(&mut folders, url);
        for section in &folders[0].sections {
            let visible: Vec<_> = section.shown_items().collect();
            assert_eq!(visible.len(), 1);
            assert_eq!(visible[0].1.url, url);
            assert!(visible[0].1.seen);
        }
        assert_eq!(folders[0].unseen(), 0);
        assert_eq!(parse(&serialize(&folders)), folders);
    }

    #[test]
    fn disabled_sections_are_not_checking_or_scheduled() {
        for integration in ALL {
            for mask in 0..(1 << integration.sources.len()) {
                let mut folder = LiveFolder::new(integration);
                for (index, section) in folder.sections.iter_mut().enumerate() {
                    section.shown = mask & (1 << index) != 0;
                }
                assert_eq!(folder.has_enabled_sections(), mask != 0);
                assert_eq!(folder.checking(), mask != 0);
                assert_eq!(folder.due(), folder.sections.iter().position(|s| s.shown));
                let restored = parse(&serialize(std::slice::from_ref(&folder)));
                assert_eq!(restored[0].checking(), mask != 0);
                folder.sign_out(Instant::now());
                assert!(!folder.checking());
                assert_eq!(folder.due(), None);
                folder.refresh();
                assert_eq!(folder.due(), folder.sections.iter().position(|s| s.shown));
                folder.signed_out = false;
                for section in &mut folder.sections {
                    section.last = Some(Instant::now());
                }
                assert!(!folder.checking());
                assert_eq!(folder.due(), None);
            }
        }
        let mut folder = LiveFolder::new(&github::SPEC);
        folder.sections.clear();
        assert!(!folder.has_enabled_sections() && !folder.checking());
        assert_eq!(folder.due(), None);
    }

    #[test]
    fn lives_beside_tabs() {
        if let (Some(live), Some(tabs)) = (default_path(), crate::session::default_path()) {
            assert_eq!(live.parent(), tabs.parent());
        }
    }
}
