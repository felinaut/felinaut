//! Live folders: premade integrations that list pages from a site in the sidebar, refreshed in
//! a hidden page that shares the user's WebKit logins, so no token is needed. GitHub is the
//! only one: open pull requests that ask for your review, that you wrote, that mention you.
//!
//! Kept in `Felifox/live.tsv` under Application Support: `F \t integration \t open` per folder,
//! `S \t section \t open \t shown` per section, then `I \t seen \t url \t updated \t dismissed
//! \t title` per item it showed. `dismissed` is the `updated` time the user closed it at.

use std::path::PathBuf;
use std::time::{Duration, Instant};

const HEADER: &str = "# Felifox live folders: F\tintegration\topen; S\tsection\topen\tshown; I\tseen\turl\tupdated\tdismissed\ttitle";
pub const MAX_ITEMS: usize = 50;
const MAX_TITLE: usize = 200;
const EVERY: Duration = Duration::from_secs(5 * 60);
/// While signed out, check again this often in case the user signed in elsewhere.
const SIGNED_OUT_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integration {
    Github,
}

/// One list inside a folder: where it comes from and what it is called.
pub struct Source {
    pub key: &'static str,
    pub label: &'static str,
    pub url: &'static str,
}

impl Integration {
    pub const ALL: [Integration; 1] = [Integration::Github];

    pub fn key(self) -> &'static str {
        match self {
            Integration::Github => "github",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Integration::Github => "GitHub",
        }
    }
    pub fn summary(self) -> &'static str {
        match self {
            Integration::Github => "Review requests, your pull requests, mentions",
        }
    }
    pub fn host(self) -> &'static str {
        match self {
            Integration::Github => "github.com",
        }
    }
    /// Opens in a normal tab when the hidden page lands on a sign-in page.
    pub fn sign_in_url(self) -> &'static str {
        match self {
            Integration::Github => "https://github.com/login",
        }
    }
    /// Checked against the signed-in `/pulls` page (it redirects to `/pulls/search`).
    pub fn sources(self) -> &'static [Source] {
        match self {
            Integration::Github => &[
                Source {
                    key: "reviews",
                    label: "Review requests",
                    url: "https://github.com/pulls?q=is:open+is:pr+review-requested:@me",
                },
                Source {
                    key: "mine",
                    label: "My pull requests",
                    url: "https://github.com/pulls?q=is:open+is:pr+author:@me",
                },
                Source {
                    key: "mentions",
                    label: "Mentions",
                    url: "https://github.com/pulls?q=is:open+is:pr+mentions:@me",
                },
            ],
        }
    }
    /// The script run in the hidden page: `null` until the list has rendered, then
    /// `[{title, url, updated}]`. GitHub draws the list after the load and shows a Blankslate
    /// when nothing matches. Rows outside the viewport have no layout, hence `textContent`.
    /// A row's last `relative-time` is its "Updated" time. Merged and closed rows are skipped
    /// even if a query lets them through.
    pub fn extractor(self) -> &'static str {
        match self {
            Integration::Github => {
                "(() => { const links = [...document.querySelectorAll('a[data-testid=\"listitem-title-link\"], a.js-navigation-open.Link--primary')]; if (!links.length && !document.querySelector('[class*=\"Blankslate\"]')) return null; return links.slice(0, 200).filter(a => !a.closest('li')?.querySelector('.octicon-git-merge, .octicon-git-pull-request-closed')).map(a => { const times = a.closest('li')?.querySelectorAll('relative-time') || []; return { title: (a.innerText || a.textContent || '').trim(), url: a.href || '', updated: times.length ? times[times.length - 1].getAttribute('datetime') || '' : '' }; }); })()"
            }
        }
    }
    fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|integration| integration.key() == key)
    }
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
}

impl Item {
    pub fn shown(&self) -> bool {
        self.dismissed.is_none()
    }

    /// `owner/repo #12` for a GitHub pull request, else the URL's path.
    pub fn place(&self) -> String {
        let Ok(url) = url::Url::parse(&self.url) else {
            return self.url.clone();
        };
        let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
        match parts.as_slice() {
            [owner, repo, "pull" | "issues", number, ..] => format!("{owner}/{repo} #{number}"),
            _ => url.path().to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub key: &'static str,
    pub label: &'static str,
    pub url: &'static str,
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
    /// Known items keep their seen and dismissed state; a dismissed item that was updated
    /// since comes back as new. The first refresh counts as seen, so a new folder is not all
    /// dots.
    pub fn update(&mut self, mut items: Vec<Item>) -> Vec<String> {
        let first = self.last.is_none() && self.items.is_empty();
        for item in &mut items {
            let old = self.items.iter().find(|old| old.url == item.url);
            item.seen = first || old.is_some_and(|old| old.seen);
            item.dismissed = old.and_then(|old| old.dismissed.clone());
            if item
                .dismissed
                .as_ref()
                .is_some_and(|at| *at != item.updated)
            {
                item.dismissed = None;
                item.seen = false;
            }
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
                .sources()
                .iter()
                .map(|source| Section {
                    key: source.key,
                    label: source.label,
                    url: source.url,
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

    /// Just added: nothing to show until the first refresh has checked the sign-in.
    pub fn checking(&self) -> bool {
        !self.signed_out
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

/// Whether a page the folder landed on is a sign-in page, as when a session expired.
pub fn is_sign_in(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        let path = url.path().to_ascii_lowercase();
        ["/login", "/signin", "/sign_in", "/session"]
            .iter()
            .any(|prefix| path.starts_with(prefix))
    })
}

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
        let title: String = value["title"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_TITLE)
            .collect();
        items.push(Item {
            title: if title.is_empty() { url.clone() } else { title },
            url,
            seen: false,
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
                let Some(integration) = Integration::from_key(key) else {
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
            ["I", seen, url, updated, dismissed, title @ ..] => {
                let (Some(folder), Some(index)) = (folders.last_mut(), section) else {
                    continue;
                };
                let host = folder.integration.host();
                let items = &mut folder.sections[index].items;
                let on_host = url::Url::parse(url).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https") && url.host_str() == Some(host)
                });
                if items.len() < MAX_ITEMS && on_host {
                    items.push(Item {
                        title: title
                            .first()
                            .unwrap_or(&"")
                            .chars()
                            .take(MAX_TITLE)
                            .collect(),
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
            folder.integration.key(),
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
                    "\nI\t{}\t{}\t{}\t{}\t{}",
                    u8::from(item.seen),
                    clean(&item.url),
                    clean(&item.updated),
                    item.dismissed.as_deref().map_or("-".into(), clean),
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
        let mut github = LiveFolder::new(Integration::Github);
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
        assert_eq!(items[0].place(), "a/b #1");
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
        let mut section = LiveFolder::new(Integration::Github).sections.remove(0);
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
        let mut section = LiveFolder::new(Integration::Github).sections.remove(0);
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
        // A merged pull request leaves the list.
        assert_eq!(section.update(vec![updated(a, "2")]), vec![b.to_owned()]);
    }

    #[test]
    fn checks_sign_in_then_schedules_sections() {
        let mut folder = LiveFolder::new(Integration::Github);
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
        assert!(is_sign_in("https://github.com/login?return_to=%2Fpulls"));
        assert!(!is_sign_in("https://github.com/pulls/search?q=x"));
    }

    #[test]
    fn lives_beside_tabs() {
        if let (Some(live), Some(tabs)) = (default_path(), crate::session::default_path()) {
            assert_eq!(live.parent(), tabs.parent());
        }
    }
}
