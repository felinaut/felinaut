//! The cookie tree behind the Cookies window: site (registrable domain) → host → cookie.
//!
//! Pure model over plain structs. `cookie_store.rs` reads and deletes the real cookies in
//! WebKit, `cookie_window.rs` draws the rows this module produces, and `WindowState` makes
//! every decision of that window: selection, expansion, the filter and the delete question. Cookie names, values and
//! domains are untrusted page data: nothing here logs them, and `Debug` hides the value.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SameSite {
    /// No attribute: WebKit sends the cookie with cross-site requests too.
    #[default]
    None,
    Lax,
    Strict,
}

impl SameSite {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Lax => "Lax",
            Self::Strict => "Strict",
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// As WebKit stores it: a leading dot means that subdomains get the cookie too.
    pub domain: String,
    pub path: String,
    /// Unix seconds. `None` is a session cookie: WebKit drops it when Felinaut quits.
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
}

impl fmt::Debug for Cookie {
    /// Never the value: a stray `{:?}` must not put a session token in a log or a panic.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cookie")
            .field("name", &self.name)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

/// What makes two cookies the same cookie (RFC 6265): name, domain and path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub domain: String,
    pub path: String,
    pub name: String,
}

impl Cookie {
    pub fn key(&self) -> Key {
        Key {
            domain: self.domain.clone(),
            path: self.path.clone(),
            name: self.name.clone(),
        }
    }

    /// Whether subdomains of `host()` receive the cookie too.
    pub fn includes_subdomains(&self) -> bool {
        self.domain.trim().starts_with('.')
    }

    pub fn host(&self) -> String {
        host_of(&self.domain)
    }
}

/// A node of the tree, by identity rather than position, so that selection and expansion
/// survive a refresh.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    All,
    Site(String),
    Host(String),
    Cookie(Key),
}

/// The site and host shown for a cookie that has no usable domain.
pub const NO_DOMAIN: &str = "(no domain)";

/// The host a cookie domain stands for: lower case, without the leading dot of a domain
/// cookie, a trailing root dot or IPv6 brackets. Control characters never reach the UI.
pub fn host_of(domain: &str) -> String {
    let host: String = domain
        .trim()
        .trim_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .chars()
        .filter(|character| !character.is_control())
        .flat_map(char::to_lowercase)
        .collect();
    if host.is_empty() {
        NO_DOMAIN.into()
    } else {
        host
    }
}

/// Second-level labels that registries put under a two-letter country code: `co.uk`, `com.au`.
const COUNTRY_SECOND_LEVEL: [&str; 16] = [
    "ac", "co", "com", "edu", "go", "gob", "gouv", "gov", "ltd", "mil", "ne", "net", "nom", "or",
    "org", "sch",
];

/// The registrable domain ("site") of a host, for grouping only.
///
/// ponytail: a heuristic, not the Public Suffix List (no new dependency, and no list to keep
/// current). The last two labels are the site, or the last three when a two-letter country
/// code follows a common registry label (`bbc.co.uk`, `abc.net.au`). Ceiling: hosts under a
/// multi-tenant suffix (`a.github.io`, `b.appspot.com`, `c.s3.amazonaws.com`) share one site,
/// and unlisted country registries (`city.kyoto.jp`, `example.pvt.k12.ma.us`) group one label
/// short. This only affects what is shown together: deleting a site deletes exactly the
/// cookies listed under it, and the confirmation names their number. If that ever matters,
/// vendor the list's ICANN section behind this function.
pub fn site_of(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    let count = labels.len();
    // An address, `localhost`, or anything else that has no registry above it.
    if count <= 2 || host.parse::<std::net::IpAddr>().is_ok() || labels.contains(&"") {
        return host.into();
    }
    let country_registry =
        labels[count - 1].len() == 2 && COUNTRY_SECOND_LEVEL.contains(&labels[count - 2]);
    let keep = if country_registry { 3 } else { 2 };
    labels[count - keep..].join(".")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    /// Sorted by name, then path.
    pub cookies: Vec<Cookie>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub name: String,
    /// The site's own host first, then its subdomains in order.
    pub hosts: Vec<Host>,
}

impl Site {
    pub fn count(&self) -> usize {
        self.hosts.iter().map(|host| host.cookies.len()).sum()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    /// Sorted by name.
    pub sites: Vec<Site>,
}

/// One visible node of the tree, in top-to-bottom order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: Node,
    /// 0 for the root, 1 site, 2 host, 3 cookie.
    pub depth: usize,
    pub label: String,
    /// A cookie's path when it is not `/`, to tell same-named cookies apart.
    pub note: Option<String>,
    /// Cookies at or below this node.
    pub count: usize,
    /// `None` for a cookie, which has nothing to expand.
    pub expanded: Option<bool>,
    /// The index of the parent's row; `None` for the root. The graph draws an edge to it.
    pub parent: Option<usize>,
}

/// What the details panel says about a branch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub sites: usize,
    pub hosts: usize,
    pub cookies: usize,
    pub session: usize,
    pub secure: usize,
    pub http_only: usize,
}

impl Tree {
    /// Groups cookies into the tree. Two cookies with the same key cannot both be stored, so
    /// a repeated key keeps the later one and counts once.
    pub fn build(cookies: &[Cookie]) -> Self {
        let mut unique: BTreeMap<Key, &Cookie> = BTreeMap::new();
        for cookie in cookies {
            unique.insert(cookie.key(), cookie);
        }
        let mut sites: BTreeMap<String, BTreeMap<String, Vec<Cookie>>> = BTreeMap::new();
        for cookie in unique.into_values() {
            let host = cookie.host();
            sites
                .entry(site_of(&host))
                .or_default()
                .entry(host)
                .or_default()
                .push(cookie.clone());
        }
        let sites = sites
            .into_iter()
            .map(|(name, hosts)| {
                let mut hosts: Vec<Host> = hosts
                    .into_iter()
                    .map(|(name, mut cookies)| {
                        cookies.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
                        Host { name, cookies }
                    })
                    .collect();
                hosts.sort_by_key(|host| host.name != name);
                Site { name, hosts }
            })
            .collect();
        Self { sites }
    }

    pub fn count(&self) -> usize {
        self.sites.iter().map(Site::count).sum()
    }

    fn hosts(&self) -> impl Iterator<Item = &Host> {
        self.sites.iter().flat_map(|site| &site.hosts)
    }

    /// The sites and hosts whose name contains `query`, ignoring case and surrounding space.
    /// A matching site keeps all its hosts; otherwise only its matching hosts remain.
    pub fn filtered(&self, query: &str) -> Self {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.clone();
        }
        let sites = self
            .sites
            .iter()
            .filter_map(|site| {
                if site.name.contains(&query) {
                    return Some(site.clone());
                }
                let hosts: Vec<Host> = site
                    .hosts
                    .iter()
                    .filter(|host| host.name.contains(&query))
                    .cloned()
                    .collect();
                (!hosts.is_empty()).then(|| Site {
                    name: site.name.clone(),
                    hosts,
                })
            })
            .collect();
        Self { sites }
    }

    /// Every cookie at or below `node`; empty when the node is not in the tree.
    pub fn subtree(&self, node: &Node) -> Vec<&Cookie> {
        match node {
            Node::All => self.hosts().flat_map(|host| &host.cookies).collect(),
            Node::Site(name) => self
                .sites
                .iter()
                .filter(|site| site.name == *name)
                .flat_map(|site| &site.hosts)
                .flat_map(|host| &host.cookies)
                .collect(),
            Node::Host(name) => self
                .hosts()
                .filter(|host| host.name == *name)
                .flat_map(|host| &host.cookies)
                .collect(),
            Node::Cookie(key) => self
                .hosts()
                .flat_map(|host| &host.cookies)
                .filter(|cookie| cookie.key() == *key)
                .collect(),
        }
    }

    /// The root is always there; any other node only while it holds a cookie.
    pub fn contains(&self, node: &Node) -> bool {
        *node == Node::All || !self.subtree(node).is_empty()
    }

    /// The node itself, or its closest ancestor that still exists: where the selection goes
    /// after a delete or a refresh.
    pub fn nearest(&self, node: &Node) -> Node {
        let mut node = node.clone();
        while !self.contains(&node) {
            node = match node {
                Node::Cookie(key) => Node::Host(host_of(&key.domain)),
                Node::Host(host) => Node::Site(site_of(&host)),
                Node::Site(_) | Node::All => Node::All,
            };
        }
        node
    }

    /// Takes the cookies with these keys out of the tree and returns them. Hosts and sites
    /// left without cookies go too.
    pub fn remove_keys(&mut self, doomed: &HashSet<Key>) -> Vec<Cookie> {
        let mut removed = Vec::with_capacity(doomed.len());
        for site in &mut self.sites {
            for host in &mut site.hosts {
                let (gone, kept): (Vec<Cookie>, Vec<Cookie>) = std::mem::take(&mut host.cookies)
                    .into_iter()
                    .partition(|cookie| doomed.contains(&cookie.key()));
                host.cookies = kept;
                removed.extend(gone);
            }
            site.hosts.retain(|host| !host.cookies.is_empty());
        }
        self.sites.retain(|site| !site.hosts.is_empty());
        removed
    }

    pub fn totals(&self, node: &Node) -> Totals {
        let cookies = self.subtree(node);
        let hosts: HashSet<String> = cookies.iter().map(|cookie| cookie.host()).collect();
        let sites: HashSet<String> = hosts.iter().map(|host| site_of(host)).collect();
        Totals {
            sites: sites.len(),
            hosts: hosts.len(),
            cookies: cookies.len(),
            session: cookies.iter().filter(|c| c.expires.is_none()).count(),
            secure: cookies.iter().filter(|c| c.secure).count(),
            http_only: cookies.iter().filter(|c| c.http_only).count(),
        }
    }

    /// What to open at first. The root opens while there are at most `site_limit` sites to
    /// show; every site, and then every host, opens as long as the number of visible nodes
    /// stays within `limit`. A small store shows everything at once, a large one starts as
    /// its sites, and a huge one as the root alone.
    pub fn opened_within(&self, limit: usize, site_limit: usize) -> HashSet<Node> {
        let sites = self.sites.len();
        let hosts = self.hosts().count();
        let mut open = HashSet::new();
        if sites > site_limit {
            return open;
        }
        open.insert(Node::All);
        if 1 + sites + hosts > limit {
            return open;
        }
        open.extend(self.sites.iter().map(|site| Node::Site(site.name.clone())));
        if 1 + sites + hosts + self.count() <= limit {
            open.extend(self.hosts().map(|host| Node::Host(host.name.clone())));
        }
        open
    }

    /// The visible rows, top to bottom: a node's children follow it only while it is in
    /// `expanded`.
    pub fn rows(&self, expanded: &HashSet<Node>) -> Vec<Row> {
        let open = |node: &Node| expanded.contains(node);
        let mut rows = vec![Row {
            node: Node::All,
            depth: 0,
            label: "All cookies".into(),
            note: None,
            count: self.count(),
            expanded: Some(open(&Node::All)),
            parent: None,
        }];
        if !open(&Node::All) {
            return rows;
        }
        for site in &self.sites {
            let node = Node::Site(site.name.clone());
            let site_open = open(&node);
            let site_row = rows.len();
            rows.push(Row {
                node,
                depth: 1,
                label: site.name.clone(),
                note: None,
                count: site.count(),
                expanded: Some(site_open),
                parent: Some(0),
            });
            if !site_open {
                continue;
            }
            for host in &site.hosts {
                let node = Node::Host(host.name.clone());
                let host_open = open(&node);
                let host_row = rows.len();
                rows.push(Row {
                    node,
                    depth: 2,
                    label: host.name.clone(),
                    note: None,
                    count: host.cookies.len(),
                    expanded: Some(host_open),
                    parent: Some(site_row),
                });
                if !host_open {
                    continue;
                }
                for cookie in &host.cookies {
                    rows.push(Row {
                        node: Node::Cookie(cookie.key()),
                        depth: 3,
                        label: display(&cookie.name, 80),
                        note: (cookie.path != "/").then(|| display(&cookie.path, 60)),
                        count: 1,
                        expanded: None,
                        parent: Some(host_row),
                    });
                }
            }
        }
        rows
    }
}

/// The most nodes the Cookies window opens by itself when it first shows the cookies.
pub const FIRST_VIEW: usize = 60;
/// The most sites it shows at first; beyond that it starts as the root alone.
pub const FIRST_VIEW_SITES: usize = 200;

/// How long the window waits for WebKit's answer. A delete takes one request per cookie
/// and then reads the store again, so its wait grows with the cookies it takes, up to a bound.
pub fn answer_timeout(deleting: usize) -> Duration {
    const BASE: Duration = Duration::from_secs(10);
    const PER_COOKIE: Duration = Duration::from_millis(20);
    const MOST: Duration = Duration::from_secs(300);
    let per_cookie = u32::try_from(deleting).map_or(MOST, |count| PER_COOKIE * count);
    (BASE + per_cookie).min(MOST)
}

/// What the window has to do after WebKit's answer, or its absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    Nothing,
    /// Read the store again: WebKit did not confirm a delete, which may still have run.
    Fetch,
}

/// What Escape did in the Cookies window, which backs out one step at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackOut {
    /// The pending delete question went away.
    Cancelled,
    /// The filter was cleared; the window empties its field.
    ClearedFilter,
    /// Nothing was left to back out of: close the window.
    Close,
}

/// A delete that waits for the user's one confirmation: exactly the cookies it named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub node: Node,
    pub keys: HashSet<Key>,
    /// The filter hides some of the branch's cookies, which stay.
    pub partial: bool,
}

impl Pending {
    pub fn prompt(&self) -> String {
        delete_prompt(&self.node, self.keys.len(), self.partial)
    }
}

/// Everything the Cookies window decides, apart from GPUI and WebKit: the cookies, the
/// filter, what is open and selected, the delete question and what WebKit last said. The
/// window draws this state, and sends to WebKit the deletes that its methods return.
#[derive(Debug)]
pub struct WindowState {
    /// Everything WebKit reported last.
    tree: Tree,
    /// The domain filter, as typed.
    query: String,
    /// `tree` through the filter: what the rows, the selection and delete work on.
    shown: Tree,
    rows: Vec<Row>,
    expanded: HashSet<Node>,
    selection: Node,
    /// A branch whose deletion waits for the user's one confirmation. It is dropped as soon
    /// as the cookies shown under the branch are no longer the ones it named.
    confirming: Option<Pending>,
    revealed: bool,
    /// What the details panel and the summary show, kept with the state rather than
    /// recomputed on every frame: totals walk every cookie under the selection.
    totals: Totals,
    selected_cookie: Option<Cookie>,
    total: usize,
    shown_count: usize,
    loaded: bool,
    /// A delete is on its way; its answer is the next list of cookies. This is the tree from
    /// before it, which is still what WebKit stores if the request never reaches it.
    deleting: Option<Tree>,
    error: Option<String>,
}

impl Default for WindowState {
    fn default() -> Self {
        let mut state = Self {
            tree: Tree::default(),
            query: String::new(),
            shown: Tree::default(),
            rows: Vec::new(),
            expanded: HashSet::from([Node::All]),
            selection: Node::All,
            confirming: None,
            revealed: false,
            totals: Totals::default(),
            selected_cookie: None,
            total: 0,
            shown_count: 0,
            loaded: false,
            deleting: None,
            error: None,
        };
        state.rebuild();
        state
    }
}

impl WindowState {
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    #[cfg(test)]
    pub fn shown(&self) -> &Tree {
        &self.shown
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn selection(&self) -> &Node {
        &self.selection
    }

    pub fn confirming(&self) -> Option<&Pending> {
        self.confirming.as_ref()
    }

    pub fn revealed(&self) -> bool {
        self.revealed
    }

    /// The totals of the selected node, as shown.
    pub fn totals(&self) -> Totals {
        self.totals
    }

    /// The selected cookie, when a shown cookie is selected.
    pub fn selected_cookie(&self) -> Option<&Cookie> {
        self.selected_cookie.as_ref()
    }

    /// Every cookie WebKit reported, filter or not.
    pub fn total(&self) -> usize {
        self.total
    }

    /// The cookies the filter shows.
    pub fn shown_count(&self) -> usize {
        self.shown_count
    }

    /// Whether WebKit has answered at least once.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether a new request for the cookies may go out: not while a delete is on its way,
    /// whose answer is the list anyway.
    pub fn may_fetch(&self) -> bool {
        self.deleting.is_none()
    }

    /// WebKit's answer to a fetch or a delete: the cookies, or `None` when it did not answer
    /// in time.
    #[must_use]
    pub fn answered(&mut self, cookies: Option<&[Cookie]>) -> Follow {
        let deleted = self.deleting.take().is_some();
        match cookies {
            Some(cookies) => {
                self.tree = Tree::build(cookies);
                if !self.loaded {
                    self.expanded = self.tree.opened_within(FIRST_VIEW, FIRST_VIEW_SITES);
                }
                self.loaded = true;
                self.error = None;
                self.rebuild();
                Follow::Nothing
            }
            // Some of the cookies may be gone and others not: neither the tree from before
            // nor the one shown now can be trusted, so ask WebKit what it stores.
            None if deleted => {
                self.error = Some(
                    "WebKit has not confirmed the delete yet. Reading the cookies again.".into(),
                );
                Follow::Fetch
            }
            None => {
                self.error = Some("WebKit did not answer. Try Refresh.".into());
                Follow::Nothing
            }
        }
    }

    /// A request could not be sent to WebKit at all. A delete that never left has deleted
    /// nothing: the cookies come back.
    pub fn send_failed(&mut self, error: String) {
        if let Some(before) = self.deleting.take() {
            self.tree = before;
            self.rebuild();
        }
        self.error = Some(error);
    }

    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        self.rebuild();
    }

    /// Recomputes what is shown after the cookies, the filter or the expansion changed.
    fn rebuild(&mut self) {
        self.shown = self.tree.filtered(&self.query);
        let selection = self.shown.nearest(&self.selection);
        if selection != self.selection {
            self.select(selection);
        }
        // A filter edit or a refresh can change what is under the branch: the question then
        // no longer says what a confirmation would delete.
        if let Some(pending) = &self.confirming
            && self.keys_under(&pending.node) != pending.keys
        {
            self.confirming = None;
        }
        self.rows = self.shown.rows(&self.expanded);
        self.total = self.tree.count();
        self.shown_count = self.shown.count();
        self.describe_selection();
    }

    fn describe_selection(&mut self) {
        self.totals = self.shown.totals(&self.selection);
        self.selected_cookie = match self.selection {
            Node::Cookie(_) => self
                .shown
                .subtree(&self.selection)
                .first()
                .copied()
                .cloned(),
            _ => None,
        };
    }

    pub fn select(&mut self, node: Node) {
        if self.selection != node {
            self.selection = node;
            self.revealed = false;
            self.confirming = None;
            self.describe_selection();
        }
    }

    pub fn toggle_revealed(&mut self) {
        self.revealed = !self.revealed;
    }

    /// Opens a closed branch or closes an open one. A cookie has nothing to open.
    pub fn toggle(&mut self, node: &Node) {
        if matches!(node, Node::Cookie(_)) {
            return;
        }
        if !self.expanded.remove(node) {
            self.expanded.insert(node.clone());
        }
        self.rebuild();
    }

    fn selected_row(&self) -> Option<usize> {
        self.rows.iter().position(|row| row.node == self.selection)
    }

    fn select_row(&mut self, index: usize) {
        if let Some(row) = self.rows.get(index) {
            self.select(row.node.clone());
        }
    }

    /// ↑ and ↓: the row above or below, stopping at either end.
    pub fn step(&mut self, by: isize) {
        let current = self.selected_row().unwrap_or(0);
        let last = self.rows.len().saturating_sub(1);
        self.select_row(current.saturating_add_signed(by).min(last));
    }

    /// ← closes an open branch, or else moves to the parent.
    pub fn collapse(&mut self) {
        let Some(index) = self.selected_row() else {
            return;
        };
        let row = self.rows[index].clone();
        if row.expanded == Some(true) {
            self.toggle(&row.node);
        } else if let Some(parent) = self.rows[..index]
            .iter()
            .rposition(|above| above.depth < row.depth)
        {
            self.select_row(parent);
        }
    }

    /// → opens a closed branch, or else moves to its first child.
    pub fn expand(&mut self) {
        let Some(index) = self.selected_row() else {
            return;
        };
        match self.rows[index].expanded {
            Some(false) => self.toggle(&self.selection.clone()),
            Some(true) => self.select_row(index + 1),
            None => {}
        }
    }

    /// A click on a graph node selects it and opens a closed branch. An open branch closes
    /// on a second click, so that selecting it, to read its totals or delete it, leaves it
    /// open.
    pub fn click(&mut self, node: &Node) {
        if !self.shown.contains(node) {
            return;
        }
        let reselected = self.selection == *node;
        self.select(node.clone());
        if reselected || !self.expanded.contains(node) {
            self.toggle(node);
        }
    }

    /// Return confirms a pending delete, whose cookies it returns; otherwise it opens or
    /// closes the selected branch.
    pub fn activate(&mut self) -> Option<HashSet<Key>> {
        if self.confirming.is_some() {
            self.confirm()
        } else {
            self.toggle(&self.selection.clone());
            None
        }
    }

    /// Escape backs out one step at a time: the delete question, the filter, the window.
    pub fn back_out(&mut self) -> BackOut {
        if self.confirming.take().is_some() {
            BackOut::Cancelled
        } else if !self.query.is_empty() {
            self.set_query("");
            BackOut::ClearedFilter
        } else {
            BackOut::Close
        }
    }

    /// The cookies shown at or below `node`: what deleting it takes. With a filter on, a
    /// site's hidden hosts keep theirs.
    fn keys_under(&self, node: &Node) -> HashSet<Key> {
        self.shown
            .subtree(node)
            .into_iter()
            .map(Cookie::key)
            .collect()
    }

    /// Deletes the selected cookie at once, returning it to send to WebKit; a branch first
    /// asks, naming how many cookies go.
    pub fn request_delete(&mut self) -> Option<HashSet<Key>> {
        let keys = self.keys_under(&self.selection);
        if self.deleting.is_some() || keys.is_empty() {
            return None;
        }
        if matches!(self.selection, Node::Cookie(_)) {
            self.begin_delete(keys)
        } else {
            let partial = keys.len() < self.tree.subtree(&self.selection).len();
            self.confirming = Some(Pending {
                node: self.selection.clone(),
                keys,
                partial,
            });
            None
        }
    }

    /// The user confirmed the pending delete: exactly the cookies it named, to send to
    /// WebKit.
    pub fn confirm(&mut self) -> Option<HashSet<Key>> {
        let pending = self.confirming.take()?;
        self.begin_delete(pending.keys)
    }

    pub fn cancel(&mut self) {
        self.confirming = None;
    }

    /// Deletes exactly these cookies. The tree shows the result at once; WebKit's answer
    /// then replaces it with what is stored.
    fn begin_delete(&mut self, keys: HashSet<Key>) -> Option<HashSet<Key>> {
        if keys.is_empty() || self.deleting.is_some() {
            return None;
        }
        self.deleting = Some(self.tree.clone());
        self.tree.remove_keys(&keys);
        self.rebuild();
        Some(keys)
    }
}

/// Untrusted text made safe to draw on one line: no control characters, at most `limit`
/// characters, and a placeholder for nothing at all.
pub fn display(text: &str, limit: usize) -> String {
    let mut shown: String = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(limit)
        .collect();
    if text.chars().count() > limit {
        shown.push('…');
    }
    if shown.is_empty() {
        "(empty)".into()
    } else {
        shown
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// "12 cookies", "1 cookie".
pub fn cookie_count(count: usize) -> String {
    plural(count, "cookie")
}

/// "3 sites · 5 hosts", leaving out the levels a branch does not span.
pub fn span(totals: Totals, node: &Node) -> Option<String> {
    match node {
        Node::All => Some(format!(
            "{} · {}",
            plural(totals.sites, "site"),
            plural(totals.hosts, "host")
        )),
        Node::Site(_) => Some(plural(totals.hosts, "host")),
        Node::Host(_) | Node::Cookie(_) => None,
    }
}

/// The question asked once before a whole branch goes. It names how many cookies go, and
/// whether the filter hides some of the branch (`partial`), whose cookies then stay.
pub fn delete_prompt(node: &Node, count: usize, partial: bool) -> String {
    let cookies = cookie_count(count);
    match (node, partial) {
        (Node::All, false) => format!("Delete all {cookies}? This signs you out of every site."),
        (Node::All, true) => format!("Delete the {cookies} shown? Cookies the filter hides stay."),
        (Node::Site(name), false) => format!("Delete {cookies} for {name} and its subdomains?"),
        (Node::Site(name), true) => format!(
            "Delete the {cookies} shown for {name}? Its hosts that the filter hides keep theirs."
        ),
        (Node::Host(name), false) => format!("Delete {cookies} for {name}?"),
        (Node::Host(name), true) => format!("Delete the {cookies} shown for {name}?"),
        (Node::Cookie(key), _) => format!("Delete the cookie {}?", display(&key.name, 40)),
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day).
fn civil(days: i64) -> (i64, i64, i64) {
    // Howard Hinnant's `civil_from_days`: eras of 400 years that start on 0000-03-01.
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// When a cookie ends, for the details panel: the UTC date and how far away that is.
pub fn expiry(expires: Option<i64>, now: i64) -> String {
    let Some(expires) = expires else {
        return "Session: ends when Felinaut quits".into();
    };
    let (year, month, day) = civil(expires.div_euclid(86_400));
    let seconds = expires.rem_euclid(86_400);
    let date = format!(
        "{year}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds / 3_600,
        seconds % 3_600 / 60
    );
    let left = expires.saturating_sub(now);
    let distance = match left {
        ..=0 => "expired".into(),
        1..3_600 => "in under an hour".into(),
        3_600..172_800 => format!("in {}", plural((left / 3_600) as usize, "hour")),
        172_800..63_072_000 => format!("in {}", plural((left / 86_400) as usize, "day")),
        _ => format!("in {}", plural((left / 31_536_000) as usize, "year")),
    };
    format!("{date} ({distance})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(domain: &str, name: &str) -> Cookie {
        Cookie {
            name: name.into(),
            value: "secret".into(),
            domain: domain.into(),
            path: "/".into(),
            ..Default::default()
        }
    }

    fn site_names(tree: &Tree) -> Vec<&str> {
        tree.sites.iter().map(|site| site.name.as_str()).collect()
    }

    fn host_names(site: &Site) -> Vec<&str> {
        site.hosts.iter().map(|host| host.name.as_str()).collect()
    }

    /// Deletes a branch the way the window does: the keys under the node, then those keys.
    fn remove(tree: &mut Tree, node: &Node) -> Vec<Cookie> {
        let doomed = tree.subtree(node).into_iter().map(Cookie::key).collect();
        tree.remove_keys(&doomed)
    }

    fn all_open(tree: &Tree) -> HashSet<Node> {
        let mut open = HashSet::from([Node::All]);
        for site in &tree.sites {
            open.insert(Node::Site(site.name.clone()));
            for host in &site.hosts {
                open.insert(Node::Host(host.name.clone()));
            }
        }
        open
    }

    #[test]
    fn an_empty_store_is_only_the_root() {
        let tree = Tree::build(&[]);
        assert_eq!(tree.count(), 0);
        assert!(tree.sites.is_empty());
        let rows = tree.rows(&HashSet::from([Node::All]));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].node.clone(), rows[0].count, rows[0].expanded),
            (Node::All, 0, Some(true))
        );
        assert_eq!(tree.totals(&Node::All), Totals::default());
        assert!(tree.subtree(&Node::Site("example.com".into())).is_empty());
        assert!(tree.contains(&Node::All));
        assert_eq!(tree.filtered("anything"), tree);
        assert_eq!(remove(&mut Tree::default(), &Node::All), Vec::new());
    }

    #[test]
    fn one_host_is_one_site_with_its_cookies_in_order() {
        let tree = Tree::build(&[cookie("example.com", "sid"), cookie("example.com", "csrf")]);
        assert_eq!(site_names(&tree), ["example.com"]);
        assert_eq!(host_names(&tree.sites[0]), ["example.com"]);
        let names: Vec<_> = tree.sites[0].hosts[0]
            .cookies
            .iter()
            .map(|cookie| cookie.name.as_str())
            .collect();
        assert_eq!(names, ["csrf", "sid"]);
        assert_eq!(tree.count(), 2);
    }

    #[test]
    fn subdomains_group_under_their_site_with_the_apex_first() {
        let tree = Tree::build(&[
            cookie("mail.google.com", "a"),
            cookie("accounts.google.com", "b"),
            cookie("accounts.google.com", "c"),
            cookie("google.com", "d"),
            cookie("deep.docs.google.com", "e"),
            cookie("example.org", "f"),
        ]);
        assert_eq!(site_names(&tree), ["example.org", "google.com"]);
        let google = &tree.sites[1];
        assert_eq!(
            host_names(google),
            [
                "google.com",
                "accounts.google.com",
                "deep.docs.google.com",
                "mail.google.com"
            ]
        );
        assert_eq!(google.count(), 5);
        assert_eq!(
            tree.totals(&Node::Site("google.com".into())),
            Totals {
                sites: 1,
                hosts: 4,
                cookies: 5,
                session: 5,
                ..Default::default()
            }
        );
        assert_eq!(tree.totals(&Node::All).sites, 2);
    }

    #[test]
    fn a_leading_dot_domain_shares_the_host_of_the_bare_domain() {
        let dotted = cookie(".example.com", "wide");
        let bare = cookie("example.com", "narrow");
        assert!(dotted.includes_subdomains());
        assert!(!bare.includes_subdomains());
        let tree = Tree::build(&[dotted.clone(), bare.clone()]);
        assert_eq!(site_names(&tree), ["example.com"]);
        assert_eq!(host_names(&tree.sites[0]), ["example.com"]);
        assert_eq!(tree.sites[0].hosts[0].cookies.len(), 2);
        // The same name on both spellings is two cookies, and each is found by its own key.
        let tree = Tree::build(&[cookie(".example.com", "x"), cookie("example.com", "x")]);
        assert_eq!(tree.count(), 2);
        assert_eq!(
            tree.subtree(&Node::Cookie(cookie(".example.com", "x").key()))
                .len(),
            1
        );
    }

    #[test]
    fn addresses_and_single_labels_are_their_own_site() {
        for (domain, host) in [
            ("localhost", "localhost"),
            ("127.0.0.1", "127.0.0.1"),
            ("192.168.1.20", "192.168.1.20"),
            ("::1", "::1"),
            ("[2001:db8::1]", "2001:db8::1"),
            ("printer", "printer"),
        ] {
            assert_eq!(host_of(domain), host);
            assert_eq!(site_of(host), host, "{domain}");
        }
        let tree = Tree::build(&[
            cookie("127.0.0.1", "a"),
            cookie("127.0.0.2", "b"),
            cookie("localhost", "c"),
        ]);
        assert_eq!(site_names(&tree), ["127.0.0.1", "127.0.0.2", "localhost"]);
    }

    #[test]
    fn sites_follow_the_documented_registry_heuristic() {
        for (host, site) in [
            ("example.com", "example.com"),
            ("www.example.com", "example.com"),
            ("a.b.c.example.com", "example.com"),
            ("bbc.co.uk", "bbc.co.uk"),
            ("news.bbc.co.uk", "bbc.co.uk"),
            ("shop.abc.net.au", "abc.net.au"),
            ("www.example.de", "example.de"),
            // A generic top level domain after `co` is not a country registry.
            ("x.co.com", "co.com"),
            ("co.uk", "co.uk"),
            // The documented ceiling: no Public Suffix List.
            ("user.github.io", "github.io"),
            ("xn--bcher-kva.example", "xn--bcher-kva.example"),
        ] {
            assert_eq!(site_of(host), site, "{host}");
        }
    }

    #[test]
    fn the_same_name_on_different_paths_is_two_cookies() {
        let mut admin = cookie("example.com", "sid");
        admin.path = "/admin".into();
        let root = cookie("example.com", "sid");
        let tree = Tree::build(&[admin.clone(), root.clone(), root.clone()]);
        assert_eq!(tree.count(), 2, "an identical key counts once");
        let rows = tree.rows(&all_open(&tree));
        let cookies: Vec<_> = rows.iter().filter(|row| row.depth == 3).collect();
        assert_eq!(cookies.len(), 2);
        assert_eq!(
            (cookies[0].label.as_str(), cookies[0].note.as_deref()),
            ("sid", None)
        );
        assert_eq!(
            (cookies[1].label.as_str(), cookies[1].note.as_deref()),
            ("sid", Some("/admin"))
        );
        assert_ne!(cookies[0].node, cookies[1].node);
        assert_eq!(tree.subtree(&Node::Cookie(admin.key())), [&admin]);
    }

    #[test]
    fn a_repeated_key_keeps_the_later_cookie() {
        let mut later = cookie("example.com", "sid");
        later.value = "new".into();
        let tree = Tree::build(&[cookie("example.com", "sid"), later]);
        assert_eq!(tree.sites[0].hosts[0].cookies[0].value, "new");
    }

    #[test]
    fn malformed_domains_and_names_stay_harmless() {
        for domain in ["", "   ", ".", "...", "\u{0}", "[]"] {
            assert_eq!(host_of(domain), NO_DOMAIN, "{domain:?}");
        }
        assert_eq!(host_of("  .Example.COM.  "), "example.com");
        assert_eq!(host_of("exa\nmple.com"), "example.com");
        assert_eq!(site_of("a..b.example.com"), "a..b.example.com");
        assert_eq!(site_of(NO_DOMAIN), NO_DOMAIN);

        let mut odd = cookie("", "na\u{7}me\nwith\tcontrols");
        odd.path = String::new();
        let long = cookie("EXAMPLE.com.", &"n".repeat(500));
        let tree = Tree::build(&[odd.clone(), long, cookie("exa mple", "")]);
        assert_eq!(site_names(&tree), [NO_DOMAIN, "exa mple", "example.com"]);
        let rows = tree.rows(&all_open(&tree));
        for row in &rows {
            assert!(!row.label.chars().any(char::is_control), "{row:?}");
            assert!(row.label.chars().count() <= 81);
            assert!(!row.label.is_empty());
        }
        assert!(rows.iter().any(|row| row.label == "(empty)"));
        // An empty path is shown as a note, and the cookie is still found for deletion.
        assert_eq!(tree.subtree(&Node::Cookie(odd.key())), [&odd]);
    }

    #[test]
    fn debug_output_never_contains_the_value() {
        let mut secret = cookie("example.com", "sid");
        secret.value = "hunter2-session-token".into();
        let tree = Tree::build(std::slice::from_ref(&secret));
        for text in [format!("{secret:?}"), format!("{tree:?}")] {
            assert!(!text.contains("hunter2"), "{text}");
            assert!(text.contains("sid"));
        }
    }

    #[test]
    fn rows_follow_expansion_and_point_at_their_parent() {
        let tree = Tree::build(&[
            cookie("a.example.com", "one"),
            cookie("a.example.com", "two"),
            cookie("example.com", "root"),
            cookie("other.org", "x"),
        ]);
        let shape = |rows: &[Row]| -> Vec<(usize, String, usize, Option<usize>)> {
            rows.iter()
                .map(|row| (row.depth, row.label.clone(), row.count, row.parent))
                .collect()
        };
        let row = |depth, label: &str, count, parent| (depth, label.to_string(), count, parent);

        assert_eq!(
            shape(&tree.rows(&HashSet::new())),
            [row(0, "All cookies", 4, None)]
        );
        assert_eq!(
            shape(&tree.rows(&HashSet::from([Node::All]))),
            [
                row(0, "All cookies", 4, None),
                row(1, "example.com", 3, Some(0)),
                row(1, "other.org", 1, Some(0)),
            ]
        );
        let rows = tree.rows(&all_open(&tree));
        assert_eq!(
            shape(&rows),
            [
                row(0, "All cookies", 4, None),
                row(1, "example.com", 3, Some(0)),
                row(2, "example.com", 1, Some(1)),
                row(3, "root", 1, Some(2)),
                row(2, "a.example.com", 2, Some(1)),
                row(3, "one", 1, Some(4)),
                row(3, "two", 1, Some(4)),
                row(1, "other.org", 1, Some(0)),
                row(2, "other.org", 1, Some(7)),
                row(3, "x", 1, Some(8)),
            ]
        );
        // A parent always comes before its children, one level up.
        for row in &rows[1..] {
            let parent = &rows[row.parent.unwrap()];
            assert_eq!(parent.depth + 1, row.depth);
        }
        assert_eq!(rows[1].expanded, Some(true));
        assert_eq!(rows[3].expanded, None);
        // A host stays hidden while its site is closed, even if the host itself is open.
        let open = HashSet::from([Node::All, Node::Host("a.example.com".into())]);
        assert_eq!(tree.rows(&open).len(), 3);
    }

    #[test]
    fn the_first_view_opens_as_much_as_fits() {
        let tree = Tree::build(&[
            cookie("a.example.com", "one"),
            cookie("a.example.com", "two"),
            cookie("example.com", "root"),
            cookie("other.org", "x"),
        ]);
        // 1 root + 2 sites + 3 hosts + 4 cookies.
        assert_eq!(tree.opened_within(10, 2), all_open(&tree));
        assert_eq!(tree.rows(&tree.opened_within(10, 2)).len(), 10);
        let sites_open = tree.opened_within(9, 2);
        assert_eq!(tree.rows(&sites_open).len(), 6, "hosts shown, cookies not");
        assert!(!sites_open.contains(&Node::Host("other.org".into())));
        // Too many nodes for the hosts: the sites show, closed.
        assert_eq!(tree.opened_within(5, 2), HashSet::from([Node::All]));
        assert_eq!(tree.opened_within(0, 2), HashSet::from([Node::All]));
        // Too many sites: the root alone, closed.
        assert_eq!(tree.opened_within(10, 1), HashSet::new());
        assert_eq!(tree.rows(&tree.opened_within(10, 1)).len(), 1);
        assert_eq!(
            Tree::default().opened_within(60, 0),
            HashSet::from([Node::All])
        );
    }

    #[test]
    fn filtering_matches_sites_and_hosts_by_substring() {
        let tree = Tree::build(&[
            cookie("accounts.google.com", "a"),
            cookie("mail.google.com", "b"),
            cookie("example.org", "c"),
            cookie("mail.example.org", "d"),
        ]);
        assert_eq!(tree.filtered(""), tree);
        assert_eq!(tree.filtered("   "), tree);
        // A site match keeps the whole site.
        let google = tree.filtered(" GOOGLE ");
        assert_eq!(site_names(&google), ["google.com"]);
        assert_eq!(google.count(), 2);
        // A host match keeps only the matching hosts, across sites.
        let mail = tree.filtered("mail");
        assert_eq!(site_names(&mail), ["example.org", "google.com"]);
        assert_eq!(host_names(&mail.sites[0]), ["mail.example.org"]);
        assert_eq!(host_names(&mail.sites[1]), ["mail.google.com"]);
        assert_eq!(mail.count(), 2);
        // Cookie names are not searched: the field filters by domain.
        assert_eq!(tree.filtered("zzz").count(), 0);
        assert_eq!(tree.filtered("a").count(), 4);
        assert!(tree.filtered("b").sites.is_empty());
    }

    #[test]
    fn deleting_a_subtree_removes_exactly_what_was_under_it() {
        let cookies = [
            cookie("accounts.google.com", "a"),
            cookie("accounts.google.com", "b"),
            cookie("mail.google.com", "c"),
            cookie("google.com", "d"),
            cookie("example.org", "e"),
        ];
        let names = |cookies: &[Cookie]| -> Vec<String> {
            let mut names: Vec<_> = cookies.iter().map(|cookie| cookie.name.clone()).collect();
            names.sort();
            names
        };

        // One cookie: its host and site stay.
        let mut tree = Tree::build(&cookies);
        let removed = remove(&mut tree, &Node::Cookie(cookies[0].key()));
        assert_eq!(names(&removed), ["a"]);
        assert_eq!(tree.count(), 4);
        assert!(tree.contains(&Node::Host("accounts.google.com".into())));

        // A host: the last cookie takes the host with it, not the site.
        let removed = remove(&mut tree, &Node::Host("accounts.google.com".into()));
        assert_eq!(names(&removed), ["b"]);
        assert!(!tree.contains(&Node::Host("accounts.google.com".into())));
        assert_eq!(
            host_names(&tree.sites[1]),
            ["google.com", "mail.google.com"]
        );

        // A site: every host under it, and nothing of another site.
        let mut tree = Tree::build(&cookies);
        let removed = remove(&mut tree, &Node::Site("google.com".into()));
        assert_eq!(names(&removed), ["a", "b", "c", "d"]);
        assert_eq!(site_names(&tree), ["example.org"]);
        assert_eq!(tree.count(), 1);

        // Something that is not there removes nothing.
        assert!(remove(&mut tree, &Node::Site("google.com".into())).is_empty());
        assert!(remove(&mut tree, &Node::Host("nope".into())).is_empty());
        assert_eq!(tree.count(), 1);

        // Everything.
        let mut tree = Tree::build(&cookies);
        assert_eq!(remove(&mut tree, &Node::All).len(), 5);
        assert_eq!(tree, Tree::default());
    }

    #[test]
    fn deleting_within_a_filter_spares_the_hidden_hosts_of_the_site() {
        let cookies = [
            cookie("mail.google.com", "shown"),
            cookie("accounts.google.com", "hidden"),
        ];
        let shown = Tree::build(&cookies).filtered("mail");
        let doomed = shown.subtree(&Node::Site("google.com".into()));
        assert_eq!(doomed, [&cookies[0]]);
    }

    #[test]
    fn selection_falls_back_to_the_closest_surviving_ancestor() {
        let cookies = [
            cookie("mail.google.com", "a"),
            cookie("mail.google.com", "b"),
            cookie("google.com", "c"),
        ];
        let mut tree = Tree::build(&cookies);
        let selected = Node::Cookie(cookies[0].key());
        assert_eq!(tree.nearest(&selected), selected);
        remove(&mut tree, &selected);
        assert_eq!(
            tree.nearest(&selected),
            Node::Host("mail.google.com".into())
        );
        remove(&mut tree, &Node::Host("mail.google.com".into()));
        assert_eq!(tree.nearest(&selected), Node::Site("google.com".into()));
        remove(&mut tree, &Node::All);
        assert_eq!(tree.nearest(&selected), Node::All);
        assert_eq!(tree.nearest(&Node::Site("gone.example".into())), Node::All);
    }

    #[test]
    fn totals_count_session_secure_and_http_only_cookies() {
        let mut login = cookie("example.com", "login");
        login.expires = Some(2_000_000_000);
        login.secure = true;
        login.http_only = true;
        let mut prefs = cookie("www.example.com", "prefs");
        prefs.secure = true;
        let tree = Tree::build(&[login.clone(), prefs]);
        assert_eq!(
            tree.totals(&Node::Site("example.com".into())),
            Totals {
                sites: 1,
                hosts: 2,
                cookies: 2,
                session: 1,
                secure: 2,
                http_only: 1,
            }
        );
        assert_eq!(
            tree.totals(&Node::Cookie(login.key())),
            Totals {
                sites: 1,
                hosts: 1,
                cookies: 1,
                session: 0,
                secure: 1,
                http_only: 1,
            }
        );
        assert_eq!(
            tree.totals(&Node::Host("missing".into())),
            Totals::default()
        );
    }

    #[test]
    fn the_delete_question_names_the_branch_and_how_many_cookies_go() {
        assert_eq!(
            delete_prompt(&Node::Site("google.com".into()), 12, false),
            "Delete 12 cookies for google.com and its subdomains?"
        );
        assert_eq!(
            delete_prompt(&Node::Host("mail.google.com".into()), 1, false),
            "Delete 1 cookie for mail.google.com?"
        );
        assert_eq!(
            delete_prompt(&Node::All, 340, false),
            "Delete all 340 cookies? This signs you out of every site."
        );
        assert_eq!(
            delete_prompt(&Node::Cookie(cookie("example.com", "sid").key()), 1, false),
            "Delete the cookie sid?"
        );
        assert_eq!(cookie_count(0), "0 cookies");
        let totals = Totals {
            sites: 1,
            hosts: 2,
            ..Default::default()
        };
        assert_eq!(span(totals, &Node::All).unwrap(), "1 site · 2 hosts");
        assert_eq!(
            span(totals, &Node::Site("example.com".into())).unwrap(),
            "2 hosts"
        );
        assert_eq!(span(totals, &Node::Host("example.com".into())), None);
    }

    #[test]
    fn expiry_shows_the_utc_date_and_the_distance() {
        let now = 1_700_000_000; // 2023-11-14 22:13:20 UTC
        assert_eq!(expiry(None, now), "Session: ends when Felinaut quits");
        assert_eq!(expiry(Some(0), now), "1970-01-01 00:00 UTC (expired)");
        assert_eq!(expiry(Some(now), now), "2023-11-14 22:13 UTC (expired)");
        assert_eq!(
            expiry(Some(now + 600), now),
            "2023-11-14 22:23 UTC (in under an hour)"
        );
        // The singular at exactly one hour, and the last second before two.
        assert_eq!(
            expiry(Some(now + 3_600), now),
            "2023-11-14 23:13 UTC (in 1 hour)"
        );
        assert_eq!(
            expiry(Some(now + 7_199), now),
            "2023-11-15 00:13 UTC (in 1 hour)"
        );
        assert_eq!(
            expiry(Some(now + 7_200), now),
            "2023-11-15 00:13 UTC (in 2 hours)"
        );
        assert_eq!(
            expiry(Some(now + 172_800), now),
            "2023-11-16 22:13 UTC (in 2 days)"
        );
        assert_eq!(
            expiry(Some(now + 30 * 86_400), now),
            "2023-12-14 22:13 UTC (in 30 days)"
        );
        // A leap day, and a far future date that some sites use for "never".
        assert_eq!(
            expiry(Some(1_709_164_800), now),
            "2024-02-29 00:00 UTC (in 106 days)"
        );
        assert_eq!(
            expiry(Some(253_402_300_799), now),
            "9999-12-31 23:59 UTC (in 7981 years)"
        );
        // Out-of-range input from a damaged store must not panic.
        assert!(expiry(Some(i64::MAX), now).ends_with("years)"));
        assert!(expiry(Some(i64::MIN), now).ends_with("(expired)"));
    }

    #[test]
    fn display_bounds_untrusted_text() {
        assert_eq!(display("plain", 10), "plain");
        assert_eq!(display("", 10), "(empty)");
        assert_eq!(display("a\u{0}b\r\nc", 10), "a b  c");
        assert_eq!(display("abcdef", 3), "abc…");
        assert_eq!(display("äöü", 3), "äöü");
    }

    fn store() -> Vec<Cookie> {
        vec![
            cookie("accounts.google.com", "a"),
            cookie("mail.google.com", "b"),
            cookie("mail.google.com", "c"),
            cookie("example.org", "d"),
        ]
    }

    fn loaded(cookies: &[Cookie]) -> WindowState {
        let mut state = WindowState::default();
        assert_eq!(state.answered(Some(cookies)), Follow::Nothing);
        state
    }

    fn keys(cookies: &[&Cookie]) -> HashSet<Key> {
        cookies.iter().map(|cookie| cookie.key()).collect()
    }

    #[test]
    fn the_window_starts_loading_with_only_the_root() {
        let state = WindowState::default();
        assert!(!state.loaded() && state.may_fetch());
        assert_eq!(state.rows().len(), 1);
        assert_eq!(*state.selection(), Node::All);
        assert_eq!(state.error(), None);
        // Before any answer there is nothing to delete.
        let mut state = state;
        assert_eq!(state.request_delete(), None);
        assert_eq!(state.confirming(), None);
    }

    #[test]
    fn the_first_answer_opens_what_fits_and_later_ones_keep_the_expansion() {
        let mut state = loaded(&store());
        assert!(state.loaded());
        // 1 root + 2 sites + 3 hosts + 4 cookies fit in the first view.
        assert_eq!(state.rows().len(), 10);
        state.toggle(&Node::Site("google.com".into()));
        assert_eq!(state.rows().len(), 5);
        assert_eq!(state.answered(Some(&store())), Follow::Nothing);
        assert_eq!(
            state.rows().len(),
            5,
            "a refresh keeps what the user closed"
        );
        // An empty answer is still an answer.
        assert_eq!(state.answered(Some(&[])), Follow::Nothing);
        assert_eq!(state.tree().count(), 0);
        assert_eq!(state.rows().len(), 1);
    }

    #[test]
    fn no_answer_keeps_the_cookies_and_says_so() {
        let mut state = loaded(&store());
        assert_eq!(state.answered(None), Follow::Nothing);
        assert_eq!(state.error(), Some("WebKit did not answer. Try Refresh."));
        assert_eq!(state.tree().count(), 4);
        state.send_failed("Cookies can only be read on the main thread".into());
        assert_eq!(
            state.error(),
            Some("Cookies can only be read on the main thread")
        );
        // The next answer clears the error.
        assert_eq!(state.answered(Some(&store())), Follow::Nothing);
        assert_eq!(state.error(), None);
    }

    #[test]
    fn selecting_moves_through_the_rows_and_hides_the_value_again() {
        let cookies = store();
        let mut state = loaded(&cookies);
        state.select(Node::Cookie(cookies[3].key()));
        state.toggle_revealed();
        assert!(state.revealed());
        state.step(-1);
        assert_eq!(*state.selection(), Node::Host("example.org".into()));
        assert!(!state.revealed(), "another selection hides the value");
        // The ends stop the selection.
        for _ in 0..20 {
            state.step(-1);
        }
        assert_eq!(*state.selection(), Node::All);
        for _ in 0..20 {
            state.step(1);
        }
        assert_eq!(state.selection(), &state.rows().last().unwrap().node);
        // The same node again keeps the value shown.
        state.toggle_revealed();
        state.select(state.selection().clone());
        assert!(state.revealed());
    }

    #[test]
    fn left_and_right_close_open_and_climb_the_tree() {
        let cookies = store();
        let mut state = loaded(&cookies);
        let host = Node::Host("example.org".into());
        state.select(Node::Cookie(cookies[3].key()));
        state.collapse();
        assert_eq!(*state.selection(), host, "← on a cookie goes to its host");
        state.collapse();
        assert_eq!(*state.selection(), host);
        assert!(
            !state
                .rows()
                .iter()
                .any(|row| row.depth == 3 && row.label == "d")
        );
        state.collapse();
        assert_eq!(*state.selection(), Node::Site("example.org".into()));
        state.select(host.clone());
        state.expand();
        assert!(state.rows().iter().any(|row| row.label == "d"), "→ opens");
        state.expand();
        assert_eq!(*state.selection(), Node::Cookie(cookies[3].key()));
        state.expand();
        assert_eq!(
            *state.selection(),
            Node::Cookie(cookies[3].key()),
            "a cookie has nothing to open"
        );
        let rows = state.rows().len();
        state.toggle(&Node::Cookie(cookies[3].key()));
        assert_eq!(state.rows().len(), rows);
    }

    #[test]
    fn a_click_selects_and_opens_and_a_second_click_closes() {
        let mut state = loaded(&store());
        let site = Node::Site("google.com".into());
        state.toggle(&site);
        state.click(&site);
        assert_eq!(*state.selection(), site);
        assert!(state.rows().iter().any(|row| row.depth == 2), "opened");
        let open = state.rows().len();
        state.click(&site);
        assert!(state.rows().len() < open, "the second click closes it");
        // A selected open branch stays open when another node was selected before.
        state.click(&site);
        state.select(Node::All);
        state.click(&site);
        assert_eq!(state.rows().len(), open);
        // A node that is not shown does nothing.
        state.click(&Node::Site("gone.example".into()));
        assert_eq!(*state.selection(), site);
    }

    #[test]
    fn the_filter_moves_the_selection_to_what_is_still_shown() {
        let cookies = store();
        let mut state = loaded(&cookies);
        state.select(Node::Cookie(cookies[0].key()));
        state.set_query("mail");
        assert_eq!(*state.selection(), Node::Site("google.com".into()));
        assert_eq!(state.shown().count(), 2);
        assert_eq!(state.tree().count(), 4);
        state.set_query("nothing matches");
        assert_eq!(*state.selection(), Node::All);
        assert_eq!(state.rows().len(), 1);
        assert_eq!(
            state.request_delete(),
            None,
            "nothing shown, nothing to delete"
        );
    }

    #[test]
    fn a_cookie_goes_at_once_and_a_branch_asks_first() {
        let cookies = store();
        let mut state = loaded(&cookies);
        state.select(Node::Cookie(cookies[3].key()));
        assert_eq!(state.request_delete(), Some(keys(&[&cookies[3]])));
        assert!(!state.may_fetch());
        assert_eq!(state.tree().count(), 3, "shown deleted at once");
        assert_eq!(state.request_delete(), None, "one delete at a time");
        assert_eq!(state.answered(Some(&cookies[..3])), Follow::Nothing);
        assert!(state.may_fetch());

        let site = Node::Site("google.com".into());
        state.select(site.clone());
        assert_eq!(state.request_delete(), None);
        assert_eq!(state.confirming().map(|pending| &pending.node), Some(&site));
        assert_eq!(
            state.tree().count(),
            3,
            "nothing goes before the confirmation"
        );
        state.cancel();
        assert_eq!(
            state.confirm(),
            None,
            "a cancelled question deletes nothing"
        );

        state.request_delete();
        assert_eq!(
            state.activate(),
            Some(keys(&[&cookies[0], &cookies[1], &cookies[2]])),
            "Return confirms"
        );
        assert_eq!(state.confirming(), None);
        assert_eq!(state.tree().count(), 0);
        assert_eq!(*state.selection(), Node::All);
    }

    #[test]
    fn another_selection_drops_the_question() {
        let mut state = loaded(&store());
        state.select(Node::Site("google.com".into()));
        state.request_delete();
        state.select(Node::Site("example.org".into()));
        assert_eq!(state.confirming(), None);
        assert_eq!(state.confirm(), None);
        // Return without a question opens or closes the selection.
        let rows = state.rows().len();
        assert_eq!(state.activate(), None);
        assert_ne!(state.rows().len(), rows);
    }

    #[test]
    fn escape_backs_out_one_step_at_a_time() {
        let mut state = loaded(&store());
        state.set_query("google");
        state.select(Node::Site("google.com".into()));
        state.request_delete();
        assert_eq!(state.back_out(), BackOut::Cancelled);
        assert_eq!(state.confirming(), None);
        assert_eq!(state.query(), "google");
        assert_eq!(state.back_out(), BackOut::ClearedFilter);
        assert_eq!(state.query(), "");
        assert_eq!(state.shown().count(), 4);
        assert_eq!(state.back_out(), BackOut::Close);
    }

    #[test]
    fn a_failed_send_ends_the_delete() {
        let cookies = store();
        let mut state = loaded(&cookies);
        state.select(Node::Cookie(cookies[3].key()));
        state.request_delete();
        state.send_failed("no store".into());
        assert!(state.may_fetch());
        assert_eq!(state.error(), Some("no store"));
    }

    #[test]
    fn a_pending_delete_never_takes_more_than_it_named() {
        let cookies = store();
        let google = [&cookies[0], &cookies[1], &cookies[2]];
        let mut state = loaded(&cookies);
        // Filter, ask to delete everything shown, then clear the filter by editing it.
        state.set_query("google");
        state.select(Node::All);
        state.request_delete();
        assert!(state.confirming().is_some());
        state.set_query("");
        assert!(
            state.confirming().is_none(),
            "the question named 3 cookies, not 4"
        );
        assert_eq!(state.confirm(), None);
        assert_eq!(state.tree().count(), 4);

        // A filter edit that shows the same cookies keeps the question.
        state.set_query("google");
        state.request_delete();
        state.set_query("googl");
        assert!(state.confirming().is_some());
        state.cancel();

        // A refresh that adds a cookie under the branch drops it.
        let site = Node::Site("google.com".into());
        state.set_query("");
        state.select(site.clone());
        state.request_delete();
        let mut more = cookies.clone();
        more.push(cookie("docs.google.com", "e"));
        assert_eq!(state.answered(Some(&more)), Follow::Nothing);
        assert!(state.confirming().is_none());

        // So does one that removes a cookie under it.
        state.request_delete();
        assert_eq!(state.answered(Some(&cookies)), Follow::Nothing);
        assert!(state.confirming().is_none());

        // A refresh elsewhere keeps it, and the delete is exactly what was named.
        state.request_delete();
        let mut elsewhere = cookies.clone();
        elsewhere.push(cookie("other.example", "f"));
        assert_eq!(state.answered(Some(&elsewhere)), Follow::Nothing);
        assert!(state.confirming().is_some());
        assert_eq!(state.confirm(), Some(keys(&google)));
        assert_eq!(state.tree().count(), 2);
    }

    #[test]
    fn the_delete_question_says_when_the_filter_hides_part_of_the_branch() {
        assert_eq!(
            delete_prompt(&Node::All, 7, true),
            "Delete the 7 cookies shown? Cookies the filter hides stay."
        );
        assert_eq!(
            delete_prompt(&Node::Site("example.com".into()), 1, true),
            "Delete the 1 cookie shown for example.com? Its hosts that the filter hides keep \
             theirs."
        );
        assert_eq!(
            delete_prompt(&Node::Host("mail.example.com".into()), 2, true),
            "Delete the 2 cookies shown for mail.example.com?"
        );

        let mut state = loaded(&store());
        let prompt = |state: &WindowState| state.confirming().map(Pending::prompt);
        // The filter matches a host: the site shows part of its cookies, All too.
        state.set_query("mail");
        state.select(Node::Site("google.com".into()));
        state.request_delete();
        assert_eq!(
            prompt(&state).as_deref(),
            Some(
                "Delete the 2 cookies shown for google.com? Its hosts that the filter hides \
                 keep theirs."
            )
        );
        state.select(Node::All);
        state.request_delete();
        assert_eq!(
            prompt(&state).as_deref(),
            Some("Delete the 2 cookies shown? Cookies the filter hides stay.")
        );
        // A host always shows all of its cookies.
        state.select(Node::Host("mail.google.com".into()));
        state.request_delete();
        assert_eq!(
            prompt(&state).as_deref(),
            Some("Delete 2 cookies for mail.google.com?")
        );
        // The filter matches the whole site: nothing of it is hidden.
        state.set_query("google");
        state.select(Node::Site("google.com".into()));
        state.request_delete();
        assert_eq!(
            prompt(&state).as_deref(),
            Some("Delete 3 cookies for google.com and its subdomains?")
        );
        // A filter that hides nothing is no filter for the question.
        state.set_query("o");
        state.select(Node::All);
        state.request_delete();
        assert_eq!(
            prompt(&state).as_deref(),
            Some("Delete all 4 cookies? This signs you out of every site.")
        );
    }

    #[test]
    fn a_delete_that_cannot_be_sent_puts_the_cookies_back() {
        let cookies = store();
        let mut state = loaded(&cookies);
        state.select(Node::Site("google.com".into()));
        state.request_delete();
        assert!(state.confirm().is_some());
        assert_eq!(state.tree().count(), 1, "shown deleted at once");
        // Nothing reached WebKit, so every cookie is still stored.
        state.send_failed("Cookies can only be read on the main thread".into());
        assert_eq!(state.tree(), &Tree::build(&cookies));
        assert!(state.rows().iter().any(|row| row.label == "google.com"));
        assert!(state.may_fetch());
        assert_eq!(
            state.error(),
            Some("Cookies can only be read on the main thread")
        );
        // A failed fetch has nothing to put back.
        state.send_failed("again".into());
        assert_eq!(state.tree(), &Tree::build(&cookies));
    }

    #[test]
    fn a_delete_that_webkit_does_not_confirm_reads_the_store_again() {
        let cookies = store();
        let mut state = loaded(&cookies);
        state.select(Node::Site("google.com".into()));
        state.request_delete();
        state.confirm();
        // WebKit may have deleted some or all of them: ask it, rather than guess.
        assert_eq!(state.answered(None), Follow::Fetch);
        assert!(state.may_fetch());
        assert_eq!(
            state.error(),
            Some("WebKit has not confirmed the delete yet. Reading the cookies again.")
        );
        assert_eq!(
            state.tree().count(),
            1,
            "the deleted cookies do not come back"
        );
        // That fetch times out too: say so, and leave the next step to the user.
        assert_eq!(state.answered(None), Follow::Nothing);
        assert_eq!(state.error(), Some("WebKit did not answer. Try Refresh."));
        // The answer then shows what is stored.
        assert_eq!(state.answered(Some(&cookies[1..])), Follow::Nothing);
        assert_eq!(state.tree().count(), 3);
        assert_eq!(state.error(), None);
    }

    #[test]
    fn the_wait_for_webkit_grows_with_the_cookies_a_delete_takes() {
        use std::time::Duration;
        assert_eq!(answer_timeout(0), Duration::from_secs(10));
        assert!(answer_timeout(1) > answer_timeout(0));
        assert_eq!(answer_timeout(5_000), Duration::from_secs(110));
        // A huge store still has a bounded wait.
        assert_eq!(answer_timeout(usize::MAX), Duration::from_secs(300));
    }

    #[test]
    fn what_the_details_show_is_kept_up_to_date_between_changes() {
        let cookies = store();
        let mut state = WindowState::default();
        let fresh = |state: &WindowState| {
            assert_eq!(state.totals(), state.shown().totals(state.selection()));
            assert_eq!(
                state.selected_cookie(),
                match state.selection() {
                    Node::Cookie(_) => state.shown().subtree(state.selection()).first().copied(),
                    _ => None,
                }
            );
            assert_eq!(state.total(), state.tree().count());
            assert_eq!(state.shown_count(), state.shown().count());
        };
        fresh(&state);
        assert_eq!(state.answered(Some(&cookies)), Follow::Nothing);
        fresh(&state);
        assert_eq!(state.totals().cookies, 4);
        state.select(Node::Cookie(cookies[1].key()));
        fresh(&state);
        assert_eq!(state.selected_cookie(), Some(&cookies[1]));
        state.set_query("example");
        fresh(&state);
        assert_eq!(state.selected_cookie(), None, "the filter hid it");
        state.select(Node::Cookie(cookies[3].key()));
        state.request_delete();
        fresh(&state);
        assert_eq!(state.totals().cookies, 0);
        state.send_failed("no store".into());
        fresh(&state);
        state.step(-1);
        fresh(&state);
        state.set_query("");
        state.toggle(&Node::All);
        fresh(&state);
        assert_eq!(state.answered(Some(&cookies[..1])), Follow::Nothing);
        fresh(&state);
        assert_eq!(state.total(), 1);
    }
}
