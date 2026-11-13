use crate::load_error::{Kind, LoadFailure};

/// CEF can report a terminal load callback after failure/cancellation. Do not
/// turn that callback into a success event that erases the browser's error UI.
#[derive(Clone, Copy, Default)]
pub struct LoadCycle {
    failed: bool,
}
impl LoadCycle {
    pub fn started(&mut self) {
        self.failed = false;
    }
    pub fn failed(&mut self) {
        self.failed = true;
    }
    pub fn successful_end(self) -> bool {
        !self.failed
    }
}

#[derive(Clone, Copy)]
pub struct RequestPolicy {
    pub navigation: bool,
    pub download: bool,
}
impl RequestPolicy {
    pub fn allows(self, url: &str) -> bool {
        !self.download
            && if self.navigation {
                navigation(url)
            } else {
                resource(url)
            }
    }
}

pub fn navigation(value: &str) -> bool {
    super::super::valid_navigation(value)
}

/// Web subresources may use embedded data and origin-scoped blobs, but never
/// filesystem, browser UI, extension or external-application protocols.
pub fn resource(value: &str) -> bool {
    if value.len() > 2 * 1024 * 1024 || value.chars().any(char::is_control) {
        return false;
    }
    if navigation(value) {
        return true;
    }
    if let Some(inner) = value.strip_prefix("blob:") {
        return (navigation(inner) && inner != "about:blank")
            || inner.strip_prefix("null/").is_some_and(opaque_blob_id);
    }
    value.starts_with("data:") && url::Url::parse(value).is_ok()
}

fn opaque_blob_id(id: &str) -> bool {
    // Sandboxed web frames can have an opaque origin. Their blob URLs use a
    // UUID, not a privileged inner URL; CEF still enforces the blob's ownership.
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

pub fn failure(code: i32, url: &str) -> Option<LoadFailure> {
    if code == -3 {
        return None;
    } // ERR_ABORTED: replacement/policy/download.
    let kind = match code {
        -106 => Kind::Offline,
        -105 | -137 => Kind::HostNotFound,
        -102 | -104 | -108 | -109 => Kind::CannotConnect,
        -7 | -118 => Kind::TimedOut,
        -21 | -100 | -101 => Kind::ConnectionLost,
        -299..=-200 | -107 | -113 | -117 | -150 => Kind::Secure,
        -310 => Kind::Redirects,
        -312 => Kind::BlockedPort,
        _ => Kind::Other,
    };
    Some(LoadFailure {
        url: if navigation(url) {
            url.into()
        } else {
            String::new()
        },
        kind,
        detail: format!("CEF main-frame load failed ({code})"),
        certificate: None,
    })
}

/// Startup-only disk work. Reject redirected engine subdirectories *before*
/// creating anything below them; a post-mkdir canonical check would be too late.
pub fn cache_root(profile: &std::path::Path) -> Result<std::path::PathBuf, String> {
    use std::fs;
    fs::create_dir_all(profile)
        .map_err(|e| format!("Could not prepare CEF startup profile: {e}"))?;
    let mut parent = profile.canonicalize().map_err(|e| e.to_string())?;
    for name in ["engines", "cef", "runtime-v156"] {
        let child = parent.join(name);
        match fs::create_dir(&child) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("Could not prepare CEF profile directory: {error}")),
        }
        let metadata = fs::symlink_metadata(&child).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(
                "CEF profile subdirectory must be a directory, not a symlink or file".into(),
            );
        }
        let canonical = child.canonicalize().map_err(|e| e.to_string())?;
        if canonical.parent() != Some(parent.as_path()) {
            return Err("CEF profile subdirectory escaped the startup profile".into());
        }
        parent = canonical;
    }
    Ok(parent)
}

pub fn disabled_features(existing: &str) -> String {
    let mut values: Vec<_> = existing.split(',').filter(|s| !s.is_empty()).collect();
    if !values.contains(&"SystemNotifications") {
        values.push("SystemNotifications");
    }
    values.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_allows_web_and_embedded_resources_but_not_privileged_navigation() {
        for value in [
            "https://example.test/",
            "http://127.0.0.1:1234/",
            "about:blank",
        ] {
            assert!(navigation(value));
            assert!(resource(value));
        }
        for value in [
            "data:image/png;base64,AAAA",
            "blob:https://example.test/uuid",
            "blob:http://127.0.0.1/id",
            "blob:null/123e4567-e89b-12d3-a456-426614174000",
        ] {
            assert!(resource(value), "{value}");
            assert!(!navigation(value));
        }
        for value in [
            "",
            "relative",
            "file:///tmp/a",
            "javascript:alert(1)",
            "chrome://settings",
            "devtools://x",
            "https://u:p@example.test/",
            "blob:file:///tmp/a",
            "blob:null/x",
            "blob:null/123e4567-e89b-12d3-a456-426614174000/extra",
            "blob:chrome://settings/id",
            "https://example.test/\n",
            "mailto:a@example.test",
        ] {
            assert!(!resource(value), "{value}");
            assert!(!navigation(value));
        }
        assert!(!resource(&format!(
            "data:image/png,{}",
            "a".repeat(2 * 1024 * 1024)
        )));
    }
    #[test]
    fn navigation_and_download_resource_flags_cannot_weaken_url_policy() {
        for navigation in [false, true] {
            for download in [false, true] {
                let policy = RequestPolicy {
                    navigation,
                    download,
                };
                assert_eq!(policy.allows("https://fixture.test/"), !download);
                assert_eq!(
                    policy.allows("data:image/png,AAAA"),
                    !navigation && !download
                );
                for url in [
                    "file:///tmp/private",
                    "chrome://settings",
                    "javascript:1",
                    "https://user:secret@fixture.test/",
                ] {
                    assert!(!policy.allows(url));
                }
            }
        }
    }
    #[test]
    fn failed_cancelled_or_crashed_loads_cannot_emit_late_success_but_reload_recovers() {
        let mut cycle = LoadCycle::default();
        cycle.started();
        assert!(cycle.successful_end());
        cycle.failed();
        assert!(!cycle.successful_end());
        assert!(!cycle.successful_end());
        cycle.failed();
        assert!(!cycle.successful_end());
        cycle.started();
        assert!(cycle.successful_end());
    }
    #[test]
    fn load_failures_are_normalized_without_native_page_text_or_tls_exceptions() {
        assert!(failure(-3, "https://example.test/").is_none());
        for (code, kind) in [
            (-106, Kind::Offline),
            (-105, Kind::HostNotFound),
            (-102, Kind::CannotConnect),
            (-118, Kind::TimedOut),
            (-101, Kind::ConnectionLost),
            (-202, Kind::Secure),
            (-310, Kind::Redirects),
            (-312, Kind::BlockedPort),
            (-999, Kind::Other),
        ] {
            let failure = failure(code, "https://example.test/").unwrap();
            assert_eq!(failure.kind, kind);
            assert!(failure.certificate.is_none());
        }
        assert!(failure(-1, "file:///private/test").unwrap().url.is_empty());
    }
    #[test]
    fn engine_cache_is_separate_reusable_and_rejects_redirects_before_writing_through_them() {
        let root =
            std::env::temp_dir().join(format!("felinaut-cef-cache-unit-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let profile = root.join("profile");
        let outside = root.join("outside");
        std::fs::create_dir(&profile).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, profile.join("engines")).unwrap();
        assert!(cache_root(&profile).is_err());
        assert!(!outside.join("cef").exists());
        std::fs::remove_file(profile.join("engines")).unwrap();
        let cache = cache_root(&profile).unwrap();
        assert_eq!(
            cache,
            profile
                .canonicalize()
                .unwrap()
                .join("engines/cef/runtime-v156")
        );
        assert_eq!(cache_root(&profile).unwrap(), cache);
        std::fs::remove_dir(&cache).unwrap();
        std::fs::write(&cache, b"not a directory").unwrap();
        assert!(cache_root(&profile).is_err());
        std::fs::remove_file(&cache).unwrap();
        std::os::unix::fs::symlink(&outside, &cache).unwrap();
        assert!(cache_root(&profile).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn notification_disable_merges_instead_of_replacing_runtime_defaults() {
        assert_eq!(
            disabled_features("Existing<Trial,,Other"),
            "Existing<Trial,Other,SystemNotifications"
        );
        assert_eq!(
            disabled_features(&disabled_features("Existing")),
            disabled_features("Existing")
        );
    }
}
