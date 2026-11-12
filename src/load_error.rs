//! WebKit navigation errors, classified and worded for the error page.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Offline,
    HostNotFound,
    CannotConnect,
    TimedOut,
    ConnectionLost,
    Secure,
    Redirects,
    BlockedPort,
    Other,
}

impl Kind {
    /// The error page's title and advice.
    pub fn text(self) -> (&'static str, &'static str) {
        match self {
            Kind::Offline => (
                "You're offline",
                "This Mac isn't connected to the internet. Check Wi-Fi or your network cable, then try again.",
            ),
            Kind::HostNotFound => (
                "Can't find this site",
                "Check the address for typos. The site may not exist, or your DNS may not be answering.",
            ),
            Kind::CannotConnect => (
                "Can't connect to this site",
                "The site isn't accepting connections. It may be down, or the address may use the wrong port.",
            ),
            Kind::TimedOut => (
                "The site took too long to respond",
                "The site may be busy, or your connection may be slow. Try again in a moment.",
            ),
            Kind::ConnectionLost => (
                "The connection was lost",
                "The network connection dropped while the page was loading.",
            ),
            Kind::Secure => (
                "Can't open this page securely",
                "A secure connection couldn't be set up. The site's certificate may be invalid or expired, or something on the network may be intercepting traffic.",
            ),
            Kind::Redirects => (
                "Too many redirects",
                "The site keeps redirecting to itself. Clearing its cookies may help.",
            ),
            Kind::BlockedPort => (
                "This port is blocked",
                "WebKit never connects to ports used by other services, such as mail or file sharing, so web pages can't misuse them. Check the port number in the address.",
            ),
            Kind::Other => ("Can't open the page", "WebKit couldn't load the page."),
        }
    }
}

/// A top-level page load that failed before anything was shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadFailure {
    /// The page that failed, or empty when WebKit didn't say.
    pub url: String,
    pub kind: Kind,
    /// WebKit's description with its error domain and code.
    pub detail: String,
}

/// Classifies a WebKit navigation error. `None` means it is not a failure to show: the load was
/// cancelled (another navigation replaced it) or became a download or policy decision.
pub fn classify(domain: &str, code: i64) -> Option<Kind> {
    match domain {
        "NSURLErrorDomain" => Some(match code {
            -999 => return None,
            -1009 | -1018 | -1019 | -1020 => Kind::Offline,
            -1003 | -1006 => Kind::HostNotFound,
            -1004 => Kind::CannotConnect,
            -1001 => Kind::TimedOut,
            -1005 => Kind::ConnectionLost,
            -1206..=-1200 | -1022 => Kind::Secure,
            -1007 | -1010 => Kind::Redirects,
            _ => Kind::Other,
        }),
        // 100/101: the content or URL can't be shown; 103: a restricted port. Others (102: a
        // policy change interrupted the load, 204: a plug-in took it) are not failures.
        "WebKitErrorDomain" => match code {
            100 | 101 => Some(Kind::Other),
            103 => Some(Kind::BlockedPort),
            _ => None,
        },
        _ => Some(Kind::Other),
    }
}

/// The failure to show for a WebKit error, or `None` for errors that aren't failures.
pub fn failure(domain: &str, code: i64, url: &str, description: &str) -> Option<LoadFailure> {
    let description: String = description
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();
    Some(LoadFailure {
        url: url.to_owned(),
        kind: classify(domain, code)?,
        detail: format!("{} ({domain} {code})", description.trim()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_errors_and_ignores_cancellations() {
        use Kind::*;
        for (domain, code, kind) in [
            ("NSURLErrorDomain", -1009, Some(Offline)),
            ("NSURLErrorDomain", -1003, Some(HostNotFound)),
            ("NSURLErrorDomain", -1004, Some(CannotConnect)),
            ("NSURLErrorDomain", -1001, Some(TimedOut)),
            ("NSURLErrorDomain", -1005, Some(ConnectionLost)),
            ("NSURLErrorDomain", -1202, Some(Secure)),
            ("NSURLErrorDomain", -1007, Some(Redirects)),
            ("NSURLErrorDomain", -1011, Some(Other)),
            ("NSURLErrorDomain", -999, None),
            ("WebKitErrorDomain", 101, Some(Other)),
            ("WebKitErrorDomain", 103, Some(BlockedPort)),
            ("WebKitErrorDomain", 102, None),
            ("WebKitErrorDomain", 204, None),
            ("NSPOSIXErrorDomain", 61, Some(Other)),
        ] {
            assert_eq!(classify(domain, code), kind, "{domain} {code}");
        }
        assert_eq!(
            failure("NSURLErrorDomain", -999, "https://a.example/", "x"),
            None
        );
    }

    #[test]
    fn describes_the_error_in_one_bounded_line() {
        let failure = failure(
            "NSURLErrorDomain",
            -1003,
            "https://a.invalid/",
            " Not\nfound. ",
        )
        .unwrap();
        assert_eq!(failure.url, "https://a.invalid/");
        assert_eq!(failure.detail, "Notfound. (NSURLErrorDomain -1003)");
        let long = super::failure("X", 1, "", &"c".repeat(1000)).unwrap();
        assert!(long.detail.len() < 320);
    }
}
