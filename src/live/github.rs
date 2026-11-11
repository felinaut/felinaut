//! GitHub: open pull requests that ask for your review, that you wrote, that mention you,
//! scraped from the signed-in `/pulls` page (it redirects to `/pulls/search`).

use super::{Source, Spec};

/// The list draws after the load and shows a Blankslate when nothing matches. A row's last
/// `relative-time` is its "Updated" time. Merged and closed rows are skipped even if a query
/// lets them through.
const PULLS: &str = dom!(
    links: r#"a[data-testid="listitem-title-link"], a.js-navigation-open.Link--primary"#,
    empty: r#"[class*="Blankslate"]"#,
    row: "li",
    skip: ".octicon-git-merge, .octicon-git-pull-request-closed",
    updated: "(t => t.length ? t[t.length - 1].getAttribute('datetime') : '')(row.querySelectorAll('relative-time'))",
    detail: r#"[row.querySelector('[data-testid="repo-filter-link"]')?.textContent.trim(), (a.href.match(/\/(?:pull|issues)\/(\d+)/) || [])[1]].filter(Boolean).join(' #')"#,
);

pub static SPEC: Spec = Spec {
    key: "github",
    name: "GitHub",
    noun: "pull requests",
    summary: "Review requests, your pull requests, mentions",
    icon: crate::brand::icon::GITHUB,
    host: "github.com",
    sign_in_url: "https://github.com/login",
    sign_in_paths: &["/login", "/session"],
    sources: &[
        Source {
            key: "reviews",
            label: "Review requests",
            url: "https://github.com/pulls?q=is:open+is:pr+review-requested:@me",
            extractor: PULLS,
        },
        Source {
            key: "mine",
            label: "My pull requests",
            url: "https://github.com/pulls?q=is:open+is:pr+author:@me",
            extractor: PULLS,
        },
        Source {
            key: "mentions",
            label: "Mentions",
            url: "https://github.com/pulls?q=is:open+is:pr+mentions:@me",
            extractor: PULLS,
        },
    ],
};
