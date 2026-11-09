//! Persistent browsing history and the address bar's fuzzy suggestions.
//!
//! One tab-separated line per URL in `history/visits.tsv` under Application Support:
//! `last visit (unix seconds) \t visits \t url \t title`. Deleting the folder clears history.

use std::{
    collections::HashMap,
    fs, io,
    ops::Range,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

// ponytail: fixed cap, oldest visits dropped; add expiry if the file ever matters.
const MAX_ENTRIES: usize = 10_000;
const HEADER: &str = "# Felifox history: last visit (unix seconds)\tvisits\turl\ttitle";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    pub visits: u32,
    pub last_visit: u64,
}

pub struct History {
    entries: Vec<Entry>,
    path: Option<PathBuf>,
    dirty: bool,
}

/// A history entry matched against the typed text, with the byte ranges to highlight.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub url: String,
    /// The URL without scheme, `www.` or a lone trailing slash, as the dropdown shows it.
    pub display_url: String,
    pub title: String,
    pub url_ranges: Vec<Range<usize>>,
    pub title_ranges: Vec<Range<usize>>,
    pub score: f32,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `~/Library/Application Support/Felifox/history/visits.tsv`, or none without a home.
pub fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Felifox/history/visits.tsv"))
}

/// Writes the whole file through a temporary sibling so a crash never leaves it truncated.
pub fn write(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tsv.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

impl History {
    /// A missing file is an empty history; an unreadable one is reported and left untouched
    /// until the next write.
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries = match path.as_deref().map(fs::read_to_string) {
            Some(Ok(text)) => parse(&text),
            Some(Err(error)) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Some(Err(error)) => {
                eprintln!("Could not read browsing history: {error}");
                Vec::new()
            }
            None => Vec::new(),
        };
        Self {
            entries,
            path,
            dirty: false,
        }
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Records a completed top-level navigation; only HTTP(S) pages count.
    pub fn record(&mut self, url: &str, title: &str, now: u64) -> bool {
        if !recordable(url) {
            return false;
        }
        let title = clean_title(title, url);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.url == url) {
            entry.visits = entry.visits.saturating_add(1);
            entry.last_visit = now;
            entry.title = title;
        } else {
            self.entries.push(Entry {
                url: url.into(),
                title,
                visits: 1,
                last_visit: now,
            });
            if self.entries.len() > MAX_ENTRIES {
                self.entries
                    .sort_by_key(|entry| std::cmp::Reverse(entry.last_visit));
                self.entries.truncate(MAX_ENTRIES);
            }
        }
        self.dirty = true;
        true
    }

    /// Titles often arrive after the load finishes, or change later in single-page apps.
    pub fn set_title(&mut self, url: &str, title: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.url == url) {
            let title = clean_title(title, url);
            if entry.title != title {
                entry.title = title;
                self.dirty = true;
            }
        }
    }

    /// Forgets a page, as the dropdown's × button does.
    pub fn remove(&mut self, url: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.url != url);
        let removed = self.entries.len() != before;
        self.dirty |= removed;
        removed
    }

    /// The path and contents to write if anything changed since the last snapshot.
    /// Serializing is in-memory work; the caller writes off the UI thread.
    pub fn take_snapshot(&mut self) -> Option<(PathBuf, String)> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        Some((self.path.clone()?, serialize(&self.entries)))
    }

    pub fn suggest(&self, query: &str, now: u64, limit: usize) -> Vec<Suggestion> {
        suggest(&self.entries, query, now, limit)
    }

    /// The most frecent pages, for an empty address bar.
    pub fn recent(&self, now: u64, limit: usize) -> Vec<Suggestion> {
        let mut entries: Vec<&Entry> = self.entries.iter().collect();
        entries.sort_by(|a, b| {
            frecency(b, now)
                .total_cmp(&frecency(a, now))
                .then_with(|| b.last_visit.cmp(&a.last_visit))
        });
        entries
            .into_iter()
            .take(limit)
            .map(|entry| Suggestion {
                url: entry.url.clone(),
                display_url: display_url(&entry.url).to_owned(),
                title: entry.title.clone(),
                url_ranges: Vec::new(),
                title_ranges: Vec::new(),
                score: frecency(entry, now),
            })
            .collect()
    }
}

fn recordable(url: &str) -> bool {
    url::Url::parse(url)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

fn clean_title(title: &str, url: &str) -> String {
    let title: String = title
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if !title.is_empty() {
        return title;
    }
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

fn parse(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.splitn(4, '\t');
        let (Some(last_visit), Some(visits), Some(url)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(last_visit), Ok(visits)) = (last_visit.parse::<u64>(), visits.parse::<u32>())
        else {
            continue;
        };
        if !recordable(url) {
            continue;
        }
        let title = clean_title(fields.next().unwrap_or(""), url);
        match index.get(url) {
            Some(&at) => {
                let entry = &mut entries[at];
                entry.visits = entry.visits.saturating_add(visits);
                if last_visit >= entry.last_visit {
                    entry.last_visit = last_visit;
                    entry.title = title;
                }
            }
            None => {
                index.insert(url, entries.len());
                entries.push(Entry {
                    url: url.into(),
                    title,
                    visits,
                    last_visit,
                });
            }
        }
    }
    entries
}

fn serialize(entries: &[Entry]) -> String {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.last_visit));
    let mut out = String::from(HEADER);
    out.push('\n');
    for entry in sorted {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            entry.last_visit, entry.visits, entry.url, entry.title
        ));
    }
    out
}

/// The URL as the dropdown shows and matches it: no scheme, no `www.`, no lone trailing slash.
pub fn display_url(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    match rest.strip_suffix('/') {
        Some(host) if !host.contains('/') => host,
        _ => rest,
    }
}

/// Lowercase a character without changing the character count.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Case-insensitive match of one query word: a substring scores far above a scattered
/// subsequence, and starting the text (the host) or a word adds to that.
fn match_text(needle: &[char], hay: &str) -> Option<(f32, Vec<Range<usize>>)> {
    let hay: Vec<(usize, usize, char)> = hay
        .char_indices()
        .map(|(start, c)| (start, start + c.len_utf8(), fold(c)))
        .collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let word_start = |i: usize| i == 0 || !hay[i - 1].2.is_alphanumeric();
    if let Some(start) = (0..=hay.len() - needle.len()).find(|&i| {
        hay[i..i + needle.len()]
            .iter()
            .zip(needle)
            .all(|(h, n)| h.2 == *n)
    }) {
        let bonus = if start == 0 {
            60.
        } else if word_start(start) {
            30.
        } else {
            0.
        };
        let end = hay[start + needle.len() - 1].1;
        return Some((100. + bonus, std::iter::once(hay[start].0..end).collect()));
    }
    // ponytail: greedy left-to-right subsequence; upgrade to an optimal alignment if ranking feels off.
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut score = 0.;
    let mut from = 0;
    let mut previous: Option<usize> = None;
    for &n in needle {
        let i = (from..hay.len()).find(|&i| hay[i].2 == n)?;
        let contiguous = previous.is_some_and(|p| p + 1 == i);
        score += if contiguous {
            8.
        } else if word_start(i) {
            10.
        } else {
            2.
        };
        match ranges.last_mut() {
            Some(last) if contiguous => last.end = hay[i].1,
            _ => ranges.push(hay[i].0..hay[i].1),
        }
        previous = Some(i);
        from = i + 1;
    }
    Some((score, ranges))
}

fn frecency(entry: &Entry, now: u64) -> f32 {
    let age_days = now.saturating_sub(entry.last_visit) / 86_400;
    let recency = match age_days {
        0..=1 => 2.0,
        2..=7 => 1.5,
        8..=30 => 1.2,
        31..=90 => 1.0,
        _ => 0.7,
    };
    recency * (1.0 + (entry.visits.max(1) as f32).ln())
}

fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Every whitespace-separated word must match the URL or the title. Ranked by match quality
/// times frecency (visit count and how recently the page was seen).
pub fn suggest(entries: &[Entry], query: &str, now: u64, limit: usize) -> Vec<Suggestion> {
    let words: Vec<Vec<char>> = query
        .split_whitespace()
        .map(|word| {
            word.strip_prefix("https://")
                .or_else(|| word.strip_prefix("http://"))
                .unwrap_or(word)
                .chars()
                .map(fold)
                .collect()
        })
        .filter(|word: &Vec<char>| !word.is_empty())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    'entries: for entry in entries {
        let display = display_url(&entry.url);
        let mut total = 0.;
        let mut url_ranges = Vec::new();
        let mut title_ranges = Vec::new();
        for word in &words {
            let in_url = match_text(word, display);
            let in_title = match_text(word, &entry.title);
            if in_url.is_none() && in_title.is_none() {
                continue 'entries;
            }
            total += in_url
                .as_ref()
                .map_or(0., |m| m.0)
                .max(in_title.as_ref().map_or(0., |m| m.0));
            url_ranges.extend(in_url.into_iter().flat_map(|m| m.1));
            title_ranges.extend(in_title.into_iter().flat_map(|m| m.1));
        }
        out.push(Suggestion {
            url: entry.url.clone(),
            display_url: display.to_owned(),
            title: entry.title.clone(),
            url_ranges: merge(url_ranges),
            title_ranges: merge(title_ranges),
            score: total * frecency(entry, now),
        });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.url.cmp(&b.url)));
    out.truncate(limit);
    out
}

/// The first row whose shown URL contains the typed text, for inline autocomplete: `le`
/// completes to `goog·le·.com` and `lang.org` to `rust-·lang.org·`. Returns the row and the
/// byte offset of the typed text in its URL. ASCII case-insensitive, and never when the typed
/// text already is the whole URL.
pub fn completion(typed: &str, suggestions: &[Suggestion]) -> Option<(usize, usize)> {
    if typed.is_empty() || typed.contains(char::is_whitespace) {
        return None;
    }
    let needle = typed.to_ascii_lowercase();
    suggestions.iter().enumerate().find_map(|(index, row)| {
        if row.display_url.len() == typed.len() {
            return None;
        }
        row.display_url
            .to_ascii_lowercase()
            .find(&needle)
            .map(|start| (index, start))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const NOW: u64 = 1_800_000_000;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("felifox-history-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let _ = fs::remove_file(&path);
        path
    }

    fn entry(url: &str, title: &str, visits: u32, last_visit: u64) -> Entry {
        Entry {
            url: url.into(),
            title: title.into(),
            visits,
            last_visit,
        }
    }

    fn urls(suggestions: &[Suggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.url.as_str()).collect()
    }

    #[test]
    fn records_dedupes_and_round_trips_through_disk() {
        let path = temp_file("visits.tsv");
        let mut history = History::load(Some(path.clone()));
        assert!(history.entries().is_empty());
        assert!(history.take_snapshot().is_none(), "nothing to save yet");

        assert!(history.record("https://example.com/", "Example\tDomain\n", NOW));
        assert!(history.record("https://www.rust-lang.org/", "", NOW + 1));
        assert!(history.record("https://example.com/", "Example Domain", NOW + 2));
        for ignored in [
            "about:blank",
            "felifox://newtab",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(!history.record(ignored, "x", NOW), "{ignored}");
        }
        assert_eq!(history.entries().len(), 2);
        let example = &history.entries()[0];
        assert_eq!((example.visits, example.last_visit), (2, NOW + 2));
        assert_eq!(example.title, "Example Domain");
        assert_eq!(
            history.entries()[1].title,
            "www.rust-lang.org",
            "host stands in for a title"
        );

        history.set_title("https://www.rust-lang.org/", "Rust");
        let (snapshot_path, contents) = history.take_snapshot().unwrap();
        assert_eq!(snapshot_path, path);
        assert!(
            history.take_snapshot().is_none(),
            "snapshot clears the dirty flag"
        );
        write(&snapshot_path, &contents).unwrap();
        assert!(contents.starts_with("# Felifox history"));
        assert!(contents.contains("\t2\thttps://example.com/\tExample Domain\n"));

        let reloaded = History::load(Some(path.clone()));
        let mut expected = history.entries().to_vec();
        expected.sort_by_key(|entry| std::cmp::Reverse(entry.last_visit));
        assert_eq!(reloaded.entries(), expected.as_slice());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn removing_a_page_forgets_it_on_disk_too() {
        let path = temp_file("remove.tsv");
        let mut history = History::load(Some(path.clone()));
        history.record("https://a.example/", "A", NOW);
        history.record("https://b.example/", "B", NOW);
        history.take_snapshot();

        assert!(!history.remove("https://missing.example/"));
        assert!(history.take_snapshot().is_none(), "nothing changed");
        assert!(history.remove("https://a.example/"));
        let (_, contents) = history.take_snapshot().expect("removal is saved");
        write(&path, &contents).unwrap();
        assert_eq!(
            urls(&History::load(Some(path.clone())).recent(NOW, 8)),
            ["https://b.example/"]
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn completes_the_first_row_whose_url_contains_the_typed_text() {
        let entries = [
            entry("https://github.com/rust-lang/rust", "The Rust repo", 9, NOW),
            entry("https://www.rust-lang.org/", "Rust", 1, NOW),
            entry("https://www.google.com/", "Google", 1, NOW),
        ];
        let rows = suggest(&entries, "ru", NOW, 8);
        assert_eq!(
            urls(&rows),
            [
                "https://github.com/rust-lang/rust",
                "https://www.rust-lang.org/"
            ]
        );
        assert_eq!(completion("ru", &rows), Some((0, 11)), "first row wins");
        let rows = suggest(&entries, "lang.org", NOW, 8);
        assert_eq!(completion("lang.org", &rows), Some((0, 5)), "mid-URL match");
        assert_eq!(
            completion("LANG.ORG", &rows),
            Some((0, 5)),
            "case-insensitive"
        );
        let rows = suggest(&entries, "le", NOW, 8);
        assert_eq!(urls(&rows), ["https://www.google.com/"]);
        assert_eq!(completion("le", &rows), Some((0, 4)));
        let rows = suggest(&entries, "rust-lang.org", NOW, 8);
        assert_eq!(
            completion("rust-lang.org", &rows),
            None,
            "nothing left to add"
        );
        assert_eq!(
            completion("rust lang", &rows),
            None,
            "words are not completed"
        );
        assert_eq!(completion("", &rows), None);
        let rows = suggest(&entries, "repo", NOW, 8);
        assert_eq!(
            completion("repo", &rows),
            None,
            "title-only matches are not"
        );
    }

    #[test]
    fn tolerates_hand_edited_files() {
        let text = "# comment\n\nbroken line\nnope\t1\thttps://a.example/\tA\n\
                    10\t2\thttps://a.example/\tOld A\n20\t3\thttps://a.example/\tNew A\n\
                    30\t1\tabout:blank\tNo\n40\t1\thttps://b.example/\n";
        let entries = parse(text);
        assert_eq!(
            entries,
            vec![
                entry("https://a.example/", "New A", 5, 20),
                entry("https://b.example/", "b.example", 1, 40),
            ]
        );
    }

    #[test]
    fn caps_the_number_of_entries_by_dropping_the_oldest() {
        let mut history = History::load(None);
        for i in 0..=MAX_ENTRIES as u64 {
            history.record(&format!("https://example.com/{i}"), "", NOW + i);
        }
        assert_eq!(history.entries().len(), MAX_ENTRIES);
        assert!(!history.entries().iter().any(|e| e.url.ends_with("/0")));
        assert!(
            history.take_snapshot().is_none(),
            "no path means nothing to write"
        );
    }

    #[test]
    fn strips_scheme_www_and_lone_slash_for_display() {
        assert_eq!(display_url("https://www.example.com/"), "example.com");
        assert_eq!(display_url("http://example.com/a/"), "example.com/a/");
        assert_eq!(
            display_url("https://example.com/a?b=c"),
            "example.com/a?b=c"
        );
    }

    #[test]
    fn substring_and_host_prefix_matches_outrank_scattered_ones() {
        let entries = [
            entry(
                "https://rust-lang.org/",
                "Rust Programming Language",
                1,
                NOW,
            ),
            entry("https://example.com/trust", "Trust me", 1, NOW),
            entry("https://news.ycombinator.com/", "Hacker News", 1, NOW),
            entry("https://docs.rs/", "Docs.rs: Rust crates", 1, NOW),
        ];
        let found = suggest(&entries, "rust", NOW, 10);
        assert_eq!(
            urls(&found),
            [
                "https://rust-lang.org/",
                "https://docs.rs/",
                "https://example.com/trust",
            ],
            "host prefix, then title word start, then mid-word substring"
        );
        assert_eq!(found[0].url_ranges, vec![0..4]);
        assert_eq!(found[0].title_ranges, vec![0..4]);
        assert_eq!(found[1].title_ranges, vec![9..13]);
        assert!(found[1].url_ranges.is_empty());

        let scattered = suggest(&entries, "hkrn", NOW, 10);
        assert_eq!(urls(&scattered), ["https://news.ycombinator.com/"]);
        assert_eq!(scattered[0].title_ranges, vec![0..1, 3..4, 5..6, 7..8]);
    }

    #[test]
    fn matching_is_case_insensitive_and_unicode_safe() {
        let entries = [entry(
            "https://café.example/Ünïcode",
            "Grüße aus Köln",
            1,
            NOW,
        )];
        let found = suggest(&entries, "ÜNÏ köln", NOW, 10);
        assert_eq!(found.len(), 1);
        let s = &found[0];
        assert_eq!(&s.display_url[s.url_ranges[0].clone()], "Ünï");
        assert_eq!(&s.title[s.title_ranges[0].clone()], "Köln");
    }

    #[test]
    fn every_word_must_match_and_schemes_in_the_query_are_ignored() {
        let entries = [
            entry(
                "https://github.com/zed-industries/zed",
                "GitHub - zed",
                1,
                NOW,
            ),
            entry("https://github.com/rust-lang/rust", "GitHub - rust", 1, NOW),
        ];
        assert_eq!(
            urls(&suggest(&entries, "github zed", NOW, 10)),
            ["https://github.com/zed-industries/zed"]
        );
        assert!(suggest(&entries, "github nothing", NOW, 10).is_empty());
        assert_eq!(suggest(&entries, "https://git", NOW, 10).len(), 2);
        assert!(suggest(&entries, "   ", NOW, 10).is_empty());
        assert_eq!(
            suggest(&entries, "github", NOW, 1).len(),
            1,
            "limit applies"
        );
    }

    #[test]
    fn frecency_prefers_frequent_and_recent_pages() {
        let entries = [
            entry("https://a.example/", "Page", 1, NOW - 100 * DAY),
            entry("https://b.example/", "Page", 1, NOW),
            entry("https://c.example/", "Page", 20, NOW - 100 * DAY),
        ];
        assert_eq!(
            urls(&suggest(&entries, "page", NOW, 10)),
            [
                "https://c.example/",
                "https://b.example/",
                "https://a.example/"
            ]
        );
    }

    #[test]
    fn recent_pages_are_ordered_by_frecency() {
        let mut history = History::load(None);
        history.record("https://old.example/", "Old", NOW - 200 * DAY);
        history.record("https://busy.example/", "Busy", NOW - 100 * DAY);
        history.record("https://busy.example/", "Busy", NOW - 100 * DAY);
        history.record("https://busy.example/", "Busy", NOW - 100 * DAY);
        history.record("https://fresh.example/", "Fresh", NOW);
        let recent = history.recent(NOW, 2);
        assert_eq!(
            urls(&recent),
            ["https://fresh.example/", "https://busy.example/"]
        );
        assert!(recent[0].url_ranges.is_empty() && recent[0].title_ranges.is_empty());
    }

    #[test]
    fn highlight_ranges_are_merged_and_sorted() {
        assert_eq!(merge(vec![5..7, 0..2, 1..3, 7..8]), vec![0..3, 5..8]);
    }
}
