//! Address-bar URL and search resolution. WebKit owns page loading.

use url::Url;

use crate::language::WebLanguage;

const MAX_URL_CHARS: usize = 8_192;
const NEW_TAB: &str = "felinaut://newtab";

/// Where address-bar searches go. A fixed list: the query is the only thing
/// that varies, so a saved setting can never point searches at an arbitrary site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchEngine {
    #[default]
    DuckDuckGo,
    Google,
    Bing,
    Brave,
    Ecosia,
    Startpage,
}

impl SearchEngine {
    pub const ALL: [Self; 6] = [
        Self::DuckDuckGo,
        Self::Google,
        Self::Bing,
        Self::Brave,
        Self::Ecosia,
        Self::Startpage,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Google => "Google",
            Self::Bing => "Bing",
            Self::Brave => "Brave Search",
            Self::Ecosia => "Ecosia",
            Self::Startpage => "Startpage",
        }
    }

    /// The persisted identifier, independent of the display name.
    pub fn key(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Google => "google",
            Self::Bing => "bing",
            Self::Brave => "brave",
            Self::Ecosia => "ecosia",
            Self::Startpage => "startpage",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|engine| engine.key() == key)
    }

    /// The results page and the name of its query parameter.
    fn endpoint(self) -> (&'static str, &'static str) {
        match self {
            Self::DuckDuckGo => ("https://duckduckgo.com/", "q"),
            Self::Google => ("https://www.google.com/search", "q"),
            Self::Bing => ("https://www.bing.com/search", "q"),
            Self::Brave => ("https://search.brave.com/search", "q"),
            Self::Ecosia => ("https://www.ecosia.org/search", "q"),
            Self::Startpage => ("https://www.startpage.com/sp/search", "query"),
        }
    }

    /// The engine's own parameter for its interface and results language, when it has one
    /// that overrides the language it would guess from the visitor's location. Engines
    /// without one get the language from the browser's preferred languages instead.
    fn language_hint(self, language: WebLanguage) -> Option<(&'static str, &'static str)> {
        let code = language.code()?;
        match (self, code) {
            (Self::Google, code) => Some(("hl", code)),
            (Self::Bing, code) => Some(("setlang", code)),
            (Self::Brave, code) => Some(("lang", code)),
            (Self::DuckDuckGo, "en") => Some(("kad", "en_US")),
            (Self::DuckDuckGo, "fr") => Some(("kad", "fr_FR")),
            (Self::DuckDuckGo, _) | (Self::Ecosia | Self::Startpage, _) => None,
        }
    }

    /// The results page for `query`: the encoded query, plus the engine's language
    /// parameter when a website language is chosen.
    pub fn search_url(self, query: &str, language: WebLanguage) -> Result<String, String> {
        let (base, parameter) = self.endpoint();
        let mut url = Url::parse(base).map_err(|e| e.to_string())?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair(parameter, query);
            if let Some((name, value)) = self.language_hint(language) {
                pairs.append_pair(name, value);
            }
        }
        Ok(url.into())
    }
}

/// Resolve address-bar input to a web URL, the internal new tab, or a search with `engine`
/// in `language`.
pub fn resolve_input(
    input: &str,
    engine: SearchEngine,
    language: WebLanguage,
) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter an address or search query.".into());
    }
    // Accept the old internal address without sending it to a search engine.
    if matches!(input, NEW_TAB | "felifox://newtab") {
        return Ok(NEW_TAB.into());
    }

    // A host:port must be recognized before treating its colon as a scheme.
    if !input.chars().any(char::is_whitespace) && looks_like_host(input) {
        return web_url(&format!("https://{input}")).map(|url| url.to_string());
    }
    if let Some((scheme, _)) = input.split_once(':')
        && is_scheme(scheme)
    {
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(format!("Unsupported URL scheme: {scheme}"));
        }
        if !input[scheme.len() + 1..].starts_with("//") {
            return Err("HTTP URLs must include // and a host.".into());
        }
        return web_url(input).map(|url| url.to_string());
    }

    engine.search_url(input, language)
}

fn web_url(input: &str) -> Result<Url, String> {
    if input.chars().count() > MAX_URL_CHARS {
        return Err("URL is too long.".into());
    }
    if input.chars().any(char::is_whitespace) {
        return Err("URLs cannot contain whitespace; encode spaces as %20.".into());
    }
    let url = Url::parse(input).map_err(|e| format!("Invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("Unsupported URL scheme: {}", url.scheme()));
    }
    if url.host_str().is_none() {
        return Err("URL must contain a host.".into());
    }
    Ok(url)
}

fn is_scheme(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn looks_like_host(input: &str) -> bool {
    let authority = input.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') || authority.is_empty() {
        return false;
    }
    let Ok(url) = Url::parse(&format!("https://{input}")) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => true,
        Some(url::Host::Domain(host)) => {
            host.eq_ignore_ascii_case("localhost")
                || (host.contains('.')
                    && host.trim_end_matches('.').split('.').all(|label| {
                        !label.is_empty()
                            && !label.starts_with('-')
                            && !label.ends_with('-')
                            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    }))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_addresses_and_internal_page() {
        for (input, expected) in [
            (" felinaut://newtab ", NEW_TAB),
            (" felifox://newtab ", NEW_TAB),
            ("https://example.com/path", "https://example.com/path"),
            ("HTTP://example.com", "http://example.com/"),
            ("example.com", "https://example.com/"),
            ("example.com:8080/path", "https://example.com:8080/path"),
            ("localhost:3000", "https://localhost:3000/"),
            ("localhost", "https://localhost/"),
            ("127.0.0.1:8080", "https://127.0.0.1:8080/"),
            ("[::1]:8080", "https://[::1]:8080/"),
        ] {
            for engine in SearchEngine::ALL {
                assert_eq!(
                    resolve_input(input, engine, WebLanguage::System).unwrap(),
                    expected,
                    "{input}"
                );
            }
        }
    }

    #[test]
    fn search_queries_are_encoded_without_extra_parameters() {
        for query in [
            "rust browser",
            "cats & dogs #cute + café",
            "why example.com fails",
        ] {
            let url = Url::parse(
                &resolve_input(query, SearchEngine::default(), WebLanguage::default()).unwrap(),
            )
            .unwrap();
            assert_eq!(url.host_str(), Some("duckduckgo.com"));
            assert_eq!(
                url.query_pairs().collect::<Vec<_>>(),
                vec![("q".into(), query.into())]
            );
        }
    }

    #[test]
    fn rejects_unsupported_schemes_and_invalid_addresses() {
        for input in [
            "",
            "  ",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/plain,hi",
            "ftp://example.com",
            "mailto:a@example.com",
            "felifox://settings",
            "felinaut://settings",
            "felinaut://newtab/",
            "felinaut://newtab?private=1",
            "about:meow",
            "https://",
            "http:example.com",
            "https://example.com:bad",
            "https://example.com/a b",
        ] {
            for engine in SearchEngine::ALL {
                assert!(
                    resolve_input(input, engine, WebLanguage::System).is_err(),
                    "{input}"
                );
            }
        }
    }

    #[test]
    fn duckduckgo_stays_the_default_search_engine() {
        assert_eq!(SearchEngine::default(), SearchEngine::DuckDuckGo);
        assert_eq!(
            resolve_input("cats", SearchEngine::default(), WebLanguage::default()).unwrap(),
            "https://duckduckgo.com/?q=cats"
        );
    }

    #[test]
    fn every_search_engine_gets_only_the_encoded_query() {
        let expected = [
            (SearchEngine::DuckDuckGo, "duckduckgo.com", "/", "q"),
            (SearchEngine::Google, "www.google.com", "/search", "q"),
            (SearchEngine::Bing, "www.bing.com", "/search", "q"),
            (SearchEngine::Brave, "search.brave.com", "/search", "q"),
            (SearchEngine::Ecosia, "www.ecosia.org", "/search", "q"),
            (
                SearchEngine::Startpage,
                "www.startpage.com",
                "/sp/search",
                "query",
            ),
        ];
        assert_eq!(SearchEngine::ALL.len(), expected.len());
        for (engine, host, path, parameter) in expected {
            for query in [
                "rust browser",
                "cats & dogs #cute + café",
                "a=b&q=evil",
                "?",
            ] {
                let url = Url::parse(&resolve_input(query, engine, WebLanguage::System).unwrap())
                    .unwrap();
                assert_eq!(url.scheme(), "https", "{engine:?}");
                assert_eq!(url.host_str(), Some(host), "{engine:?}");
                assert_eq!(url.path(), path, "{engine:?}");
                assert_eq!(url.fragment(), None, "{engine:?}");
                assert_eq!(
                    url.query_pairs().collect::<Vec<_>>(),
                    vec![(parameter.into(), query.into())],
                    "{engine:?} {query}"
                );
            }
        }
    }

    #[test]
    fn search_engine_keys_round_trip_and_reject_unknown_values() {
        let mut keys = Vec::new();
        for engine in SearchEngine::ALL {
            assert_eq!(SearchEngine::from_key(engine.key()), Some(engine));
            keys.push(engine.key());
        }
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), SearchEngine::ALL.len(), "keys must be unique");
        for key in [
            "",
            "DuckDuckGo",
            "Google",
            " google",
            "yahoo",
            "https://evil.example/?q=",
        ] {
            assert_eq!(SearchEngine::from_key(key), None, "{key:?}");
        }
    }

    #[test]
    fn a_chosen_language_adds_only_each_engines_own_language_parameter() {
        let expected = [
            (SearchEngine::Google, Some(("hl", "en")), Some(("hl", "fr"))),
            (
                SearchEngine::Bing,
                Some(("setlang", "en")),
                Some(("setlang", "fr")),
            ),
            (
                SearchEngine::Brave,
                Some(("lang", "en")),
                Some(("lang", "fr")),
            ),
            (
                SearchEngine::DuckDuckGo,
                Some(("kad", "en_US")),
                Some(("kad", "fr_FR")),
            ),
            // These follow the browser's preferred languages instead.
            (SearchEngine::Ecosia, None, None),
            (SearchEngine::Startpage, None, None),
        ];
        assert_eq!(expected.len(), SearchEngine::ALL.len());
        for (engine, english, french) in expected {
            let query_name = engine.endpoint().1;
            for (language, hint) in [
                (WebLanguage::System, None),
                (WebLanguage::English, english),
                (WebLanguage::French, french),
            ] {
                // A query that tries to set the language itself stays part of the query.
                for query in ["météo", "x&hl=de&setlang=de&kad=de_CH"] {
                    let url = Url::parse(&resolve_input(query, engine, language).unwrap()).unwrap();
                    let mut want = vec![(query_name.to_owned(), query.to_owned())];
                    want.extend(hint.map(|(n, v)| (n.to_owned(), v.to_owned())));
                    assert_eq!(
                        url.query_pairs().into_owned().collect::<Vec<_>>(),
                        want,
                        "{engine:?} {language:?} {query}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_chosen_language_never_changes_typed_addresses() {
        for language in [WebLanguage::English, WebLanguage::French] {
            for (input, expected) in [
                ("example.com", "https://example.com/"),
                (
                    "https://www.google.com/search?q=x",
                    "https://www.google.com/search?q=x",
                ),
                ("felinaut://newtab", NEW_TAB),
            ] {
                assert_eq!(
                    resolve_input(input, SearchEngine::Google, language).unwrap(),
                    expected
                );
            }
        }
    }
}
