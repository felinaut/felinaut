//! "Copy Clean Link" (⌘⇧C): the page address without tracking parameters.
//!
//! Only parameters that exist to track clicks and campaigns are removed; everything that
//! selects content (a search query, a branch, a video time) stays. Kept parameters keep their
//! original order and percent-encoding, so the link opens exactly the same page.

use url::{Url, form_urlencoded};

/// Parameters removed on every site, compared case-insensitively.
const TRACKING: &[&str] = &[
    // Ad click identifiers.
    "fbclid",
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    "gad_source",
    "gad_campaignid",
    "msclkid",
    "twclid",
    "ttclid",
    "li_fat_id",
    "yclid",
    "ysclid",
    "rb_clickid",
    "srsltid",
    "epik",
    // Analytics and email campaigns.
    "_ga",
    "_gl",
    "mc_cid",
    "mc_eid",
    "_hsenc",
    "_hsmi",
    "__hstc",
    "__hssc",
    "__hsfp",
    "hsctatracking",
    "mkt_tok",
    "_kx",
    "vero_id",
    "vero_conv",
    "oly_anon_id",
    "oly_enc_id",
    "s_cid",
    "wickedid",
    // Social share identifiers.
    "igshid",
    "igsh",
];

/// Parameter name prefixes removed on every site. `mtm_` is Matomo's own; its older `pk_`
/// names are listed exactly in `MATOMO`, because sites use `pk_` for their own parameters.
const TRACKING_PREFIXES: &[&str] = &["utm_", "mtm_", "hsa_"];

/// Matomo's (formerly Piwik's) campaign parameters.
const MATOMO: &[&str] = &[
    "pk_campaign",
    "pk_cid",
    "pk_content",
    "pk_group",
    "pk_keyword",
    "pk_kwd",
    "pk_medium",
    "pk_placement",
    "pk_source",
];

/// Parameters that only track on particular sites; elsewhere the same names can mean
/// something real, so they are kept.
struct SiteRule {
    /// Whether the (lowercase, dot-trimmed) host belongs to the site.
    site: fn(&str) -> bool,
    /// When set, the rule applies only on this path.
    path: Option<&'static str>,
    names: &'static [&'static str],
    prefixes: &'static [&'static str],
}

const SITE_RULES: &[SiteRule] = &[
    SiteRule {
        site: |host| on(host, "youtube.com") || on(host, "youtu.be"),
        path: None,
        names: &["si", "feature", "pp"],
        prefixes: &[],
    },
    SiteRule {
        site: |host| on(host, "spotify.com"),
        path: None,
        names: &["si"],
        prefixes: &[],
    },
    SiteRule {
        site: is_google,
        path: Some("/search"),
        names: &[
            "ved", "ei", "sei", "oq", "gs_lcrp", "gs_lp", "gs_ssp", "sclient", "sourceid", "uact",
            "sca_esv", "sca_upv", "sxsrf", "iflsig", "rlz", "biw", "bih", "aqs", "client", "sa",
            "source",
        ],
        prefixes: &[],
    },
    SiteRule {
        site: is_amazon,
        path: None,
        names: &[
            "ref",
            "ref_",
            "qid",
            "sr",
            "crid",
            "sprefix",
            "content-id",
            "_encoding",
        ],
        prefixes: &["pd_rd_", "pf_rd_"],
    },
];

/// Google's own search domains, after `google.` (from its list of supported domains).
const GOOGLE_ENDINGS: &[&str] = &[
    "com", "ad", "ae", "al", "am", "as", "at", "az", "ba", "be", "bf", "bg", "bi", "bj", "bs",
    "bt", "by", "ca", "cat", "cd", "cf", "cg", "ch", "ci", "cl", "cm", "cn", "cv", "cz", "de",
    "dj", "dk", "dm", "dz", "ee", "es", "fi", "fm", "fr", "ga", "ge", "gg", "gl", "gm", "gr", "gy",
    "hn", "hr", "ht", "hu", "ie", "im", "iq", "is", "it", "je", "jo", "kg", "ki", "kz", "la", "li",
    "lk", "lt", "lu", "lv", "md", "me", "mg", "mk", "ml", "mn", "mu", "mv", "mw", "ne", "nl", "no",
    "nr", "nu", "pl", "pn", "ps", "pt", "ro", "rs", "ru", "rw", "sc", "se", "sh", "si", "sk", "sm",
    "sn", "so", "sr", "st", "td", "tg", "tl", "tm", "tn", "to", "tt", "vg", "vu", "ws", "co.ao",
    "co.bw", "co.ck", "co.cr", "co.id", "co.il", "co.in", "co.jp", "co.ke", "co.kr", "co.ls",
    "co.ma", "co.mz", "co.nz", "co.th", "co.tz", "co.ug", "co.uk", "co.uz", "co.ve", "co.vi",
    "co.za", "co.zm", "co.zw", "com.af", "com.ag", "com.ar", "com.au", "com.bd", "com.bh",
    "com.bn", "com.bo", "com.br", "com.bz", "com.co", "com.cu", "com.cy", "com.do", "com.ec",
    "com.eg", "com.et", "com.fj", "com.gh", "com.gi", "com.gt", "com.hk", "com.jm", "com.kh",
    "com.kw", "com.lb", "com.ly", "com.mm", "com.mt", "com.mx", "com.my", "com.na", "com.ng",
    "com.ni", "com.np", "com.om", "com.pa", "com.pe", "com.pg", "com.ph", "com.pk", "com.pr",
    "com.py", "com.qa", "com.sa", "com.sb", "com.sg", "com.sl", "com.sv", "com.tj", "com.tr",
    "com.tw", "com.ua", "com.uy", "com.vc", "com.vn",
];

/// Amazon's storefronts, after `amazon.`.
const AMAZON_ENDINGS: &[&str] = &[
    "com", "ca", "cn", "de", "es", "fr", "ie", "in", "it", "nl", "pl", "se", "sg", "ae", "sa",
    "eg", "co.uk", "co.jp", "co.za", "com.au", "com.be", "com.br", "com.mx", "com.tr",
];

/// `host` is `domain` or one of its subdomains.
fn on(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain)
        .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
}

/// `host` is `<brand>.<ending>`, or a subdomain of it, for one of the brand's own `endings`.
/// The endings are listed rather than guessed, because `amazon.github.io` or `google.evil.io`
/// belong to someone else.
fn is_brand_site(host: &str, brand: &str, endings: &[&str]) -> bool {
    endings.iter().any(|ending| {
        host.strip_suffix(ending)
            .and_then(|rest| rest.strip_suffix('.'))
            .is_some_and(|rest| on(rest, brand))
    })
}

fn is_google(host: &str) -> bool {
    is_brand_site(host, "google", GOOGLE_ENDINGS)
}

fn is_amazon(host: &str) -> bool {
    is_brand_site(host, "amazon", AMAZON_ENDINGS)
}

fn is_tracking(host: &str, path: &str, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let listed = |names: &[&str], prefixes: &[&str]| {
        names.contains(&name.as_str()) || prefixes.iter().any(|prefix| name.starts_with(prefix))
    };
    listed(TRACKING, TRACKING_PREFIXES)
        || MATOMO.contains(&name.as_str())
        || SITE_RULES.iter().any(|rule| {
            (rule.site)(host)
                && rule.path.is_none_or(|only| only == path)
                && listed(rule.names, rule.prefixes)
        })
}

/// A copied link, and whether anything was removed to make it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanLink {
    pub link: String,
    /// Whether tracking was found and removed. When it is `false`, `link` is the address
    /// exactly as it was given.
    pub removed: bool,
}

/// The cleaned link for a web page address, or `None` for anything that is not an
/// http(s) URL (the New Tab page, an empty tab, malformed input).
pub fn clean(address: &str) -> Option<CleanLink> {
    let mut url = Url::parse(address).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    // `www.amazon.com.` is the same site as `www.amazon.com`.
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let mut removed = false;

    // Amazon also puts its tracking in a trailing `/ref=…` path segment.
    if is_amazon(&host)
        && let Some(index) = url.path().rfind("/ref=")
        && !url.path()[index + 1..].contains('/')
    {
        let path = url.path()[..index].to_owned();
        url.set_path(if path.is_empty() { "/" } else { &path });
        removed = true;
    }

    let path = url.path().to_owned();
    if let Some(query) = url.query() {
        let parts: Vec<&str> = query.split('&').filter(|part| !part.is_empty()).collect();
        let kept: Vec<&str> = parts
            .iter()
            .copied()
            .filter(|part| {
                // Decode only to compare the name; the original text is what is kept.
                let name = form_urlencoded::parse(part.as_bytes())
                    .next()
                    .map(|(name, _)| name.into_owned())
                    .unwrap_or_default();
                !is_tracking(&host, &path, &name)
            })
            .collect();
        if kept.len() < parts.len() {
            removed = true;
            let kept = kept.join("&");
            url.set_query((!kept.is_empty()).then_some(kept.as_str()));
        }
    }

    // Text fragments (`#:~:text=…`) record what was highlighted, not where the page is.
    if let Some(fragment) = url.fragment()
        && let Some(index) = fragment.find(":~:")
    {
        let anchor = fragment[..index].to_owned();
        url.set_fragment((!anchor.is_empty()).then_some(anchor.as_str()));
        removed = true;
    }
    let link = if removed {
        url.into()
    } else {
        address.to_owned()
    };
    Some(CleanLink { link, removed })
}

#[cfg(test)]
mod tests {
    use super::{CleanLink, clean};

    /// `input` loses its tracking and becomes `expected`.
    fn check(input: &str, expected: &str) {
        let link = CleanLink {
            link: expected.to_owned(),
            removed: true,
        };
        assert_eq!(clean(input), Some(link), "{input}");
    }

    /// `input` has no tracking, so it is copied exactly as given.
    fn unchanged(input: &str) {
        let link = CleanLink {
            link: input.to_owned(),
            removed: false,
        };
        assert_eq!(clean(input), Some(link), "{input}");
    }

    #[test]
    fn removes_campaign_and_click_parameters_everywhere() {
        check(
            "https://example.com/post?utm_source=x&utm_medium=email&utm_campaign=fall",
            "https://example.com/post",
        );
        check(
            "https://example.com/?fbclid=abc&gclid=1&msclkid=2&_ga=3&mc_eid=4",
            "https://example.com/",
        );
        check(
            "https://shop.example/item?id=7&utm_source=newsletter&color=red",
            "https://shop.example/item?id=7&color=red",
        );
        // Names are matched case-insensitively and after percent-decoding.
        check(
            "https://example.com/a?UTM_Source=x&utm%5Fmedium=y&FBCLID=z&b=1",
            "https://example.com/a?b=1",
        );
        check(
            "https://example.com/a?pk_campaign=x&mtm_kwd=y&pk_kwd=z",
            "https://example.com/a",
        );
    }

    #[test]
    fn removes_valueless_and_repeated_tracking_parameters() {
        check(
            "https://example.com/a?fbclid&b=1",
            "https://example.com/a?b=1",
        );
        check(
            "https://example.com/a?fbclid=1&b=1&fbclid=2&utm_source&FBCLID=",
            "https://example.com/a?b=1",
        );
    }

    #[test]
    fn keeps_everything_that_selects_content_exactly_as_written() {
        for url in [
            "https://github.com/felinaut/felinaut/blob/main/README.md?ref=main",
            "https://example.com/search?q=caf%C3%A9+au+lait&page=2",
            "https://example.com/a?x=1+2&y=%20&z",
            "https://example.com/watch?v=abc&t=42",
            "https://example.com/?si=keep",
            "https://example.com/?feature=keep",
            "https://example.com/doc#section-2",
            "http://localhost:3000/dev?debug=1",
            "https://example.com/",
            // Matomo's names are listed exactly; a site's own `pk_` parameters stay.
            "https://example.com/item?pk_id=5&pk=3",
            // Tracking-looking text inside a value is part of the value.
            "https://example.com/go?q=utm_source%3Dx&next=https%3A%2F%2Fa.example%2F%3Ffbclid%3D1",
            "https://example.com/go?note=fbclid",
        ] {
            unchanged(url);
        }
    }

    #[test]
    fn keeps_the_port_and_user_information() {
        check(
            "https://user:secret@example.com:8443/a?utm_source=x&b=1",
            "https://user:secret@example.com:8443/a?b=1",
        );
        check("http://127.0.0.1:8080/?gclid=1", "http://127.0.0.1:8080/");
    }

    #[test]
    fn keeps_parameter_order_and_drops_an_empty_query() {
        check(
            "https://example.com/?b=2&utm_source=x&a=1&gclid=y&c=3",
            "https://example.com/?b=2&a=1&c=3",
        );
        check(
            "https://example.com/path?utm_source=x",
            "https://example.com/path",
        );
        check(
            "https://example.com/path?&&utm_source=x&&",
            "https://example.com/path",
        );
    }

    #[test]
    fn says_nothing_was_removed_when_there_was_no_tracking() {
        // Empty query parts and a bare `?` are not tracking: the address is copied as it is,
        // and the pop-up must not claim that tracking was removed.
        for url in [
            "https://example.com/path?",
            "https://example.com/path?a=1&&b=2",
            "https://example.com/path?a=1&",
            "https://example.com/path?#top",
            "https://EXAMPLE.com/Path",
            "https://www.amazon.com/dp/B000123?th=1",
            "https://www.google.com/search?q=tea",
        ] {
            unchanged(url);
        }
    }

    #[test]
    fn removes_site_specific_trackers_only_on_their_sites() {
        check(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&si=AbC123&feature=shared&t=10",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
        );
        check(
            "https://youtu.be/dQw4w9WgXcQ?si=AbC123",
            "https://youtu.be/dQw4w9WgXcQ",
        );
        check(
            "https://m.youtube.com/watch?v=abc&pp=x",
            "https://m.youtube.com/watch?v=abc",
        );
        check(
            "https://music.youtube.com/watch?v=abc&si=x&list=RD",
            "https://music.youtube.com/watch?v=abc&list=RD",
        );
        check(
            "https://open.spotify.com/track/123?si=abcdef",
            "https://open.spotify.com/track/123",
        );
        check(
            "https://www.google.com/search?q=what&hl=en&sei=x&ved=y&oq=wha&gs_lcrp=z&sourceid=chrome&ie=UTF-8",
            "https://www.google.com/search?q=what&hl=en&ie=UTF-8",
        );
        check(
            "https://www.google.co.uk/search?q=tea&ei=x",
            "https://www.google.co.uk/search?q=tea",
        );
        check(
            "https://www.google.de/search?q=tee&ved=x",
            "https://www.google.de/search?q=tee",
        );
        check(
            "https://google.com.au/search?q=tea&ei=x",
            "https://google.com.au/search?q=tea",
        );
        // The same names elsewhere, or off Google's search page, are left alone.
        for url in [
            "https://example.com/search?q=x&ved=y",
            "https://www.google.com/maps?q=x&ei=y",
            "https://notgoogle.com/search?ei=y",
            "https://youtube.com.example/watch?si=1",
            "https://notyoutube.com/watch?si=1",
        ] {
            unchanged(url);
        }
    }

    #[test]
    fn brand_rules_need_the_brands_own_domain() {
        // Each of these is someone else's site that merely starts with the brand's name.
        for url in [
            "https://google.evil.example/search?ei=y",
            "https://google.evil.io/search?ei=y",
            "https://www.google.co.io/search?ei=y",
            "https://google.com.evil.io/search?ei=y",
            "https://xgoogle.com/search?ei=y",
            "https://amazon.github.io/dp/1/ref=sr_1?qid=1&sr=8",
            "https://amazon.myshop.de/item/ref=sr_1?ref=x&qid=1",
            "https://www.amazon.com.evil.io/dp/1/ref=sr_1?qid=1",
            "https://notamazon.com/dp/1?ref=x",
            "https://example.com/dp/1/ref=sr_1_1?ref=x",
        ] {
            unchanged(url);
        }
    }

    #[test]
    fn removes_amazon_referral_paths_and_parameters() {
        check(
            "https://www.amazon.com/dp/B000123/ref=sr_1_1?crid=X&keywords=kettle&qid=1&sr=8-1&th=1",
            "https://www.amazon.com/dp/B000123?keywords=kettle&th=1",
        );
        check(
            "https://www.amazon.de/gp/product/B0001?pd_rd_w=a&pf_rd_p=b&psc=1",
            "https://www.amazon.de/gp/product/B0001?psc=1",
        );
        // Search result pages keep the search.
        check(
            "https://www.amazon.fr/s?k=bouilloire&crid=2X&sprefix=bouil%2Caps%2C90&ref=nb_sb_noss",
            "https://www.amazon.fr/s?k=bouilloire",
        );
        // Other Amazon hosts and two-label storefronts.
        check(
            "https://smile.amazon.co.uk/dp/B1/ref=pd_1?qid=2",
            "https://smile.amazon.co.uk/dp/B1",
        );
        check(
            "https://m.amazon.com.br/dp/B1?sr=1-1&th=1",
            "https://m.amazon.com.br/dp/B1?th=1",
        );
        // Only the path segment may be tracking.
        check(
            "https://www.amazon.com/dp/B1/ref=sr_1",
            "https://www.amazon.com/dp/B1",
        );
        // A `ref=` that is not the last path segment is part of the address.
        unchanged("https://www.amazon.com/ref=nav_logo/deals");
    }

    #[test]
    fn treats_a_trailing_dot_host_like_the_same_site() {
        check(
            "https://www.amazon.com./dp/X/ref=sr_1?qid=1",
            "https://www.amazon.com./dp/X",
        );
        check(
            "https://www.google.com./search?q=a&ei=1",
            "https://www.google.com./search?q=a",
        );
        check(
            "https://www.youtube.com./watch?v=a&si=1",
            "https://www.youtube.com./watch?v=a",
        );
    }

    #[test]
    fn removes_text_fragments_but_keeps_anchors() {
        check(
            "https://example.com/article#:~:text=hello%20world",
            "https://example.com/article",
        );
        check(
            "https://example.com/article#intro:~:text=hello",
            "https://example.com/article#intro",
        );
        unchanged("https://example.com/article#intro");
    }

    #[test]
    fn only_web_pages_have_a_link_to_copy() {
        for input in [
            "",
            "felinaut://newtab",
            "about:blank",
            "file:///Users/me/secret.txt",
            "javascript:alert(1)",
            "data:text/plain,hi",
            "not a url",
            "https://",
        ] {
            assert_eq!(clean(input), None, "{input:?}");
        }
    }
}
