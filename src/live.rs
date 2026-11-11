//! Live folders: a sidebar folder that lists the links one CSS selector finds on a web page,
//! refreshed every few minutes in a hidden page that shares the user's WebKit logins. Kept in
//! `Felifox/live.tsv` under Application Support: an `F \t name \t url \t selector \t minutes \t
//! open` line per folder, then an `I \t seen \t url \t title` line per item it last showed.

use std::path::PathBuf;
use std::time::Instant;

const HEADER: &str = "# Felifox live folders: F\tname\turl\tselector\tminutes\topen (1 expanded); I\tseen\turl\ttitle";
pub const MAX_ITEMS: usize = 50;
const MAX_TITLE: usize = 200;

/// GitHub's pull request list, checked against the live page (2025): each row's title link.
/// The second part matches the older server-rendered list.
pub const GITHUB_SELECTOR: &str =
    "a[data-testid=\"listitem-title-link\"], a.js-navigation-open.Link--primary";
pub const GITHUB_MY_PRS: &str = "https://github.com/pulls?q=is:open+is:pr+author:@me";
pub const GITHUB_REVIEWS: &str = "https://github.com/pulls?q=is:open+is:pr+review-requested:@me";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    pub url: String,
    pub seen: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveFolder {
    pub name: String,
    pub url: String,
    pub selector: String,
    /// Minutes between refreshes.
    pub every: u32,
    pub items: Vec<Item>,
    pub open: bool,
    /// Why the last refresh failed; not saved.
    pub error: Option<String>,
    /// The last refresh asks the user to sign in to this host; not saved.
    pub sign_in: Option<String>,
    /// When the last refresh ended; `None` refreshes as soon as the window is active.
    pub last: Option<Instant>,
}

impl LiveFolder {
    pub fn new(name: &str, url: &str, selector: &str, every: u32) -> Self {
        Self {
            name: name.trim().to_owned(),
            url: url.trim().to_owned(),
            selector: selector.trim().to_owned(),
            every: every.clamp(1, 24 * 60),
            items: Vec::new(),
            open: true,
            error: None,
            sign_in: None,
            last: None,
        }
    }

    pub fn unseen(&self) -> usize {
        self.items.iter().filter(|item| !item.seen).count()
    }

    pub fn due(&self) -> bool {
        self.last
            .is_none_or(|last| last.elapsed().as_secs() >= u64::from(self.every) * 60)
    }

    /// Takes a refresh's links. Links already shown keep their seen state; on the first
    /// refresh everything counts as seen, so a new folder is not all dots.
    pub fn update(&mut self, mut items: Vec<Item>) {
        let first = self.items.is_empty() && self.last.is_none();
        for item in &mut items {
            item.seen = first || self.items.iter().any(|old| old.url == item.url && old.seen);
        }
        self.items = items;
        self.error = None;
        self.sign_in = None;
    }
}

/// Validates a folder from the form: a web URL with a host, a selector, an interval.
pub fn check(name: &str, url: &str, selector: &str, minutes: &str) -> Result<LiveFolder, String> {
    let host = web_host(url.trim()).ok_or("The address must start with http:// or https://")?;
    if selector.trim().is_empty() {
        return Err("Enter a CSS selector for the links".into());
    }
    let every: u32 = minutes
        .trim()
        .parse()
        .ok()
        .filter(|minutes| (1..=1440).contains(minutes))
        .ok_or("Refresh every 1 to 1440 minutes")?;
    let name = if name.trim().is_empty() {
        host.as_str()
    } else {
        name
    };
    Ok(LiveFolder::new(name, url, selector, every))
}

/// The host of an http(s) URL.
pub fn web_host(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    matches!(url.scheme(), "http" | "https")
        .then(|| url.host_str().map(str::to_owned))
        .flatten()
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

/// Turns what the extractor script returned (JSON `[{title, url}]`) into items: web links on
/// the folder's own host only, each once, at most 50, titles trimmed to 200 characters.
/// `None` when the page gave no list at all.
pub fn parse_items(json: &str, folder_url: &str) -> Option<Vec<Item>> {
    let host = web_host(folder_url)?;
    let values: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let mut items: Vec<Item> = Vec::new();
    for value in values {
        let (Some(url), title) = (value["url"].as_str(), value["title"].as_str()) else {
            continue;
        };
        let Ok(mut url) = url::Url::parse(url) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") || url.host_str() != Some(&host) {
            continue;
        }
        url.set_fragment(None);
        let url = url.to_string();
        if items.iter().any(|item| item.url == url) {
            continue;
        }
        let title: String = title
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
        });
        if items.len() == MAX_ITEMS {
            break;
        }
    }
    Some(items)
}

/// The script run in the hidden page. Virtualised rows have no layout, hence `textContent`.
pub fn extractor(selector: &str) -> String {
    let selector = serde_json::to_string(selector).unwrap_or_default();
    format!(
        "(() => {{ try {{ return [...document.querySelectorAll({selector})].slice(0, 200).map(a => ({{ title: (a.innerText || a.textContent || '').trim(), url: a.href || '' }})); }} catch (e) {{ return []; }} }})()"
    )
}

/// `~/Library/Application Support/Felifox/live.tsv`, beside `tabs.tsv`.
pub fn default_path() -> Option<PathBuf> {
    Some(crate::session::default_path()?.with_file_name("live.tsv"))
}

pub fn parse(text: &str) -> Vec<LiveFolder> {
    let mut folders: Vec<LiveFolder> = Vec::new();
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["F", name, url, selector, minutes, rest @ ..] => {
                if let Ok(mut folder) = check(name, url, selector, minutes) {
                    folder.open = rest.first() != Some(&"0");
                    folders.push(folder);
                }
            }
            ["I", seen, url, title @ ..] => {
                let Some(folder) = folders.last_mut() else {
                    continue;
                };
                if folder.items.len() < MAX_ITEMS
                    && web_host(url).is_some_and(|host| web_host(&folder.url) == Some(host))
                {
                    folder.items.push(Item {
                        title: title
                            .first()
                            .unwrap_or(&"")
                            .chars()
                            .take(MAX_TITLE)
                            .collect(),
                        url: (*url).to_owned(),
                        seen: *seen == "1",
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
            "\nF\t{}\t{}\t{}\t{}\t{}",
            clean(&folder.name),
            clean(&folder.url),
            clean(&folder.selector),
            folder.every,
            u8::from(folder.open)
        ));
        for item in &folder.items {
            out.push_str(&format!(
                "\nI\t{}\t{}\t{}",
                u8::from(item.seen),
                clean(&item.url),
                clean(&item.title)
            ));
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
        }
    }

    #[test]
    fn round_trips_and_sanitizes() {
        let mut prs = LiveFolder::new("My\tPRs", GITHUB_MY_PRS, GITHUB_SELECTOR, 5);
        prs.items = vec![
            item("https://github.com/a/b/pull/1", "Fix\tit", true),
            item("https://github.com/a/b/pull/2", "New", false),
        ];
        let mut feed = LiveFolder::new("Feed", "https://example.com/", "a.story", 30);
        feed.open = false;
        let parsed = parse(&serialize(&[prs.clone(), feed.clone()]));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "My PRs");
        assert_eq!(parsed[0].items[0].title, "Fix it");
        assert_eq!(parsed[0].items[1], prs.items[1]);
        assert_eq!(parsed[0].every, 5);
        assert_eq!(parsed[1], feed);
    }

    #[test]
    fn skips_bad_folders_and_foreign_items() {
        let text = "# x\nF\tBad\tfile:///etc\ta\t5\t1\nI\t1\thttps://github.com/x\tx\nF\tOk\thttps://a.b/\ta\t0\t1\nF\tOk\thttps://a.b/\ta\t5\t1\nI\t0\tjavascript:alert(1)\tx\nI\t0\thttps://evil.c/\tx\nI\t0\thttps://a.b/1\tOne\n";
        let parsed = parse(text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].items, vec![item("https://a.b/1", "One", false)]);
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
        let items = parse_items(&json, GITHUB_MY_PRS).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "Fix the bug");
        assert_eq!(items[0].url, "https://github.com/a/b/pull/1");
        assert_eq!(items[1].title, "https://github.com/a/b/pull/2");
        assert_eq!(items[2].title.chars().count(), 200);
        let many: Vec<_> = (0..80)
            .map(|n| serde_json::json!({"title": "t", "url": format!("https://github.com/{n}")}))
            .collect();
        let many = serde_json::to_string(&many).unwrap();
        assert_eq!(parse_items(&many, GITHUB_MY_PRS).unwrap().len(), 50);
        assert_eq!(parse_items("null", GITHUB_MY_PRS), None);
    }

    #[test]
    fn new_items_are_unseen_after_the_first_refresh() {
        let mut folder = LiveFolder::new("PRs", GITHUB_MY_PRS, GITHUB_SELECTOR, 5);
        folder.update(vec![item("https://github.com/1", "a", false)]);
        assert_eq!(folder.unseen(), 0);
        folder.last = Some(Instant::now());
        folder.update(vec![
            item("https://github.com/1", "a", false),
            item("https://github.com/2", "b", false),
        ]);
        assert_eq!(folder.unseen(), 1);
        assert!(!folder.items[1].seen);
    }

    #[test]
    fn form_validation_and_sign_in_pages() {
        assert!(
            check("", "https://github.com/pulls", "a", "5").is_ok_and(|f| f.name == "github.com")
        );
        assert!(check("x", "github.com", "a", "5").is_err());
        assert!(check("x", "https://a.b/", " ", "5").is_err());
        assert!(check("x", "https://a.b/", "a", "0").is_err());
        assert!(is_sign_in("https://github.com/login?return_to=%2Fpulls"));
        assert!(!is_sign_in("https://github.com/pulls/search?q=x"));
        assert!(extractor("a[x=\"1\"]").contains("\"a[x=\\\"1\\\"]\""));
    }

    #[test]
    fn lives_beside_tabs() {
        if let (Some(live), Some(tabs)) = (default_path(), crate::session::default_path()) {
            assert_eq!(live.parent(), tabs.parent());
        }
    }
}
