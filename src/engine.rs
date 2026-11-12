//! Address-bar URL and search resolution. WebKit owns page loading.

use url::Url;

const MAX_URL_CHARS: usize = 8_192;
const NEW_TAB: &str = "felinaut://newtab";

/// Resolve address-bar input to a web URL, the internal new tab, or a search.
pub fn resolve_input(input: &str) -> Result<String, String> {
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

    let mut search = Url::parse("https://duckduckgo.com/").map_err(|e| e.to_string())?;
    search.query_pairs_mut().append_pair("q", input);
    Ok(search.into())
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
            assert_eq!(resolve_input(input).unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn search_queries_are_encoded_without_extra_parameters() {
        for query in [
            "rust browser",
            "cats & dogs #cute + café",
            "why example.com fails",
        ] {
            let url = Url::parse(&resolve_input(query).unwrap()).unwrap();
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
            assert!(resolve_input(input).is_err(), "{input}");
        }
    }
}
