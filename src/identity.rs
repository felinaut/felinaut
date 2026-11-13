//! The user agent Felinaut's pages send: WebKit's own, plus the Safari version tokens.
//!
//! A bare `WKWebView` reports `… AppleWebKit/605.1.15 (KHTML, like Gecko)` with no `Version/…
//! Safari/…` suffix. Sites read that as an unknown embedded view: Google, for one, serves a
//! stripped-down legacy results page without its Sign in button. Felinaut renders with the
//! system WebKit that the installed Safari also uses, so it reports that Safari's version.

use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

/// WebKit's own user agent on macOS. Apple froze the OS and WebKit tokens for privacy, so they
/// no longer change between releases.
const WEBKIT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
/// Safari's own build token, frozen the same way.
const SAFARI_TOKEN: &str = "605.1.15";
const SAFARI_INFO: &str = "/Applications/Safari.app/Contents/Info.plist";
/// Safari's `Info.plist` is about 22 KB; a much larger file is not one to parse.
const MAX_INFO_BYTES: u64 = 256 * 1024;

/// The user agent for `version` (`major.minor`, plus `.patch` when Safari has one).
pub fn user_agent_for(version: &str) -> String {
    format!("{WEBKIT} Version/{version} Safari/{SAFARI_TOKEN}")
}

/// Safari's version from its XML `Info.plist` as Safari itself reports it: `major.minor`, plus
/// `.patch` when there is one (Safari 26.6.2 sends `Version/26.6.2`). `None` if it is missing or
/// malformed.
pub fn safari_version(info_plist: &str) -> Option<String> {
    let after_key = info_plist
        .split("<key>CFBundleShortVersionString</key>")
        .nth(1)?;
    let value = after_key
        .trim_start()
        .strip_prefix("<string>")?
        .split("</string>")
        .next()?
        .trim();
    let mut parts = value.split('.');
    let major = parts.next()?;
    let minor = parts.next().unwrap_or("0");
    let patch = parts.next();
    let number =
        |part: &str| (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
    let valid = number(major) && number(minor) && patch.is_none_or(number) && parts.all(number);
    valid.then(|| match patch {
        Some(patch) => format!("{major}.{minor}.{patch}"),
        None => format!("{major}.{minor}"),
    })
}

/// Safari's version for a macOS version, for when the installed Safari cannot be read. They
/// are the same from macOS 26 on, and Safari was three ahead from macOS 11 to 15. Either way it
/// is approximate: Safari can be updated apart from macOS.
pub fn version_for_macos(major: isize, minor: isize, patch: isize) -> String {
    if major >= 26 && patch > 0 {
        format!("{major}.{minor}.{patch}")
    } else if major >= 26 {
        format!("{major}.{minor}")
    } else {
        format!("{}.0", major + 3)
    }
}

/// The text of `path`, or `None` if it cannot be read, is not UTF-8 or is over `MAX_INFO_BYTES`.
fn read_info(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_INFO_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_INFO_BYTES).then_some(text)
}

/// The user agent every Felinaut page uses. Reads Safari's version once, on first use; `main`
/// calls this before the first window opens so no page creation waits on the disk.
pub fn user_agent() -> &'static str {
    static AGENT: OnceLock<String> = OnceLock::new();
    AGENT.get_or_init(|| {
        let version = read_info(Path::new(SAFARI_INFO))
            .and_then(|plist| safari_version(&plist))
            .unwrap_or_else(|| {
                let os = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
                version_for_macos(os.majorVersion, os.minorVersion, os.patchVersion)
            });
        user_agent_for(&version)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plist(version: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             \t<key>CFBundleName</key>\n\t<string>Safari</string>\n\
             \t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n\
             \t<key>CFBundleVersion</key>\n\t<string>21624.5.1.11.3</string>\n</dict>\n</plist>\n"
        )
    }

    #[test]
    fn matches_safaris_own_user_agent_shape() {
        assert_eq!(
            user_agent_for("26.6"),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/26.6 Safari/605.1.15"
        );
        // What sites look for: WebKit first, then Safari's version and build tokens last.
        let agent = user_agent_for("18.3");
        assert!(agent.starts_with(WEBKIT));
        assert!(agent.ends_with(" Version/18.3 Safari/605.1.15"));
        assert!(!agent.contains("Chrome") && !agent.contains("Felinaut"));
    }

    #[test]
    fn matches_the_user_agent_measured_in_safari() {
        // Recorded from Safari 27.0.1 on macOS 27.0.1, whose `Info.plist` has
        // `CFBundleShortVersionString` 27.0.1: the HTTP header and `navigator.userAgent` agree.
        let version = safari_version(&plist("27.0.1")).unwrap();
        assert_eq!(
            user_agent_for(&version),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/27.0.1 Safari/605.1.15"
        );
    }

    #[test]
    fn reads_the_version_safari_reports_from_its_info_plist() {
        // Safari sends the patch level when there is one: Safari 26.6.2 reports
        // `Version/26.6.2`, Safari 26.6 reports `Version/26.6`.
        for (version, expected) in [
            ("26.6.2", "26.6.2"),
            ("26.6", "26.6"),
            ("18.3.1", "18.3.1"),
            ("26.0.1", "26.0.1"),
            ("26", "26.0"),
            (" 26.1 ", "26.1"),
            // Anything after the patch is a build detail that Safari does not report.
            ("26.6.2.1", "26.6.2"),
        ] {
            assert_eq!(
                safari_version(&plist(version)).as_deref(),
                Some(expected),
                "{version}"
            );
        }
    }

    #[test]
    fn rejects_missing_or_malformed_versions() {
        for version in [
            "",
            "beta",
            "26.x",
            "26..1",
            "26.6.",
            "26.6.x",
            ".6",
            "26.6 beta",
            "26.6.2.1.9-rc",
            "1234.5",
        ] {
            assert_eq!(safari_version(&plist(version)), None, "{version:?}");
        }
        for text in [
            "",
            "not a plist",
            "<key>CFBundleShortVersionString</key>",
            "<key>CFBundleShortVersionString</key><integer>26</integer>",
            "bplist00\u{0}\u{1}binary",
        ] {
            assert_eq!(safari_version(text), None, "{text:?}");
        }
    }

    #[test]
    fn an_unreadable_safari_falls_back_to_the_version_for_this_macos() {
        for ((major, minor, patch), expected) in [
            ((26, 0, 0), "26.0"),
            ((26, 0, 1), "26.0.1"),
            ((26, 6, 0), "26.6"),
            ((26, 6, 2), "26.6.2"),
            ((27, 1, 0), "27.1"),
            ((15, 6, 1), "18.0"),
            ((11, 0, 0), "14.0"),
        ] {
            assert_eq!(version_for_macos(major, minor, patch), expected);
        }
    }

    #[test]
    fn reads_info_files_only_up_to_the_size_limit() {
        let dir = std::env::temp_dir().join(format!("felinaut-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.plist");
        std::fs::write(&small, plist("26.6.2")).unwrap();
        assert_eq!(read_info(&small), Some(plist("26.6.2")));
        let limit = dir.join("limit.plist");
        std::fs::write(&limit, "a".repeat(MAX_INFO_BYTES as usize)).unwrap();
        assert_eq!(
            read_info(&limit).map(|text| text.len() as u64),
            Some(MAX_INFO_BYTES)
        );
        let large = dir.join("large.plist");
        std::fs::write(&large, "a".repeat(MAX_INFO_BYTES as usize + 1)).unwrap();
        assert_eq!(read_info(&large), None);
        let binary = dir.join("binary.plist");
        std::fs::write(&binary, b"bplist00\xff\xfe").unwrap();
        assert_eq!(read_info(&binary), None);
        assert_eq!(read_info(&dir.join("missing.plist")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_shared_user_agent_is_always_well_formed() {
        // Whatever this machine has installed, the result has Safari's shape.
        let agent = user_agent();
        let version = agent
            .strip_prefix(&format!("{WEBKIT} Version/"))
            .and_then(|rest| rest.strip_suffix(&format!(" Safari/{SAFARI_TOKEN}")))
            .expect("Safari-shaped user agent");
        let parts: Vec<_> = version.split('.').collect();
        assert!(
            (2..=3).contains(&parts.len()) && parts.iter().all(|part| part.parse::<u16>().is_ok()),
            "{version}"
        );
        assert!(std::ptr::eq(agent, user_agent()), "read once and shared");
    }
}
