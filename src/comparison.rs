//! Opt-in, isolated Cocoa WebKit comparison launches. Not a general profile/private mode.

use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use crate::page::{BackendInfo, RuntimeSource};

static POLICY: OnceLock<Policy> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub struct Policy {
    mode: Option<Mode>,
    expected_uuid: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    System,
    Upstream,
}

impl Policy {
    fn parse(mode: Option<&str>, expected_uuid: Option<&str>) -> Result<Self, String> {
        let mode = match mode {
            None => None,
            Some("system") => Some(Mode::System),
            Some("upstream") => Some(Mode::Upstream),
            Some(_) => return Err("FELINAUT_COMPARISON must be system or upstream".into()),
        };
        let expected_uuid = expected_uuid.map(canonical_uuid).transpose()?;
        if mode.is_none() && expected_uuid.is_some() {
            return Err("An expected WebKit UUID requires comparison mode".into());
        }
        if mode == Some(Mode::Upstream) && expected_uuid.is_none() {
            return Err("Upstream comparison requires FELINAUT_EXPECTED_WEBKIT_UUID".into());
        }
        Ok(Self {
            mode,
            expected_uuid,
        })
    }

    pub fn enabled(&self) -> bool {
        self.mode.is_some()
    }

    pub fn label(&self) -> Option<&'static str> {
        self.mode.map(|mode| match mode {
            Mode::System => "System WebKit (Comparison)",
            Mode::Upstream => "Upstream Cocoa WebKit (Experimental)",
        })
    }

    fn verify_build(&self, selected: &[&str]) -> Result<(), String> {
        if self.enabled() && selected.contains(&"cef") {
            return Err("Cocoa runtime comparison requires a build without CEF; system/upstream WebKit comparison remains a separate-process experiment".into());
        }
        if self.mode.is_none()
            && !selected.contains(&"system-webkit")
            && !selected.contains(&"upstream-webkit")
            && selected.contains(&"cef")
        {
            return crate::engine_build::require_backend(selected, "cef").map(|_| ());
        }
        let id = match self.mode {
            None | Some(Mode::System) => "system-webkit",
            Some(Mode::Upstream) => "upstream-webkit",
        };
        crate::engine_build::require_backend(selected, id).map(|_| ())
    }

    pub fn verify_runtime(&self, info: &BackendInfo) -> Result<(), String> {
        if self.mode.is_none()
            && info.id == "cef"
            && info.engine_family == "chromium"
            && crate::engine_build::compiled_ids() == ["cef"]
        {
            return Ok(());
        }
        // An unidentified image must not downgrade an external runtime into normal
        // browsing with a persistent profile. Missing provenance fails before UI/profile load.
        let mode = self.mode.unwrap_or(Mode::System);
        let required = match mode {
            Mode::System => RuntimeSource::SystemSharedCache,
            Mode::Upstream => RuntimeSource::ExternalFramework,
        };
        if info.id != "native-webkit"
            || info.engine_family != "webkit"
            || info.runtime_identity.source != required
        {
            return Err(
                "Loaded WebKit runtime does not match the requested comparison mode".into(),
            );
        }
        let uuid = info
            .runtime_identity
            .image_uuid
            .as_deref()
            .ok_or("Loaded WebKit image UUID is unverified")?;
        let uuid = canonical_uuid(uuid)?;
        if self
            .expected_uuid
            .as_ref()
            .is_some_and(|expected| *expected != uuid)
        {
            return Err(
                "Loaded WebKit UUID differs from the requested runtime; refusing fallback".into(),
            );
        }
        Ok(())
    }

    /// Restrict comparison top-level navigation to explicit loopback addresses. This is not
    /// a subresource firewall; the comparison harness serves trusted, self-contained fixtures.
    pub fn allows_url(&self, value: &str) -> bool {
        if !self.enabled() {
            return true;
        }
        let Ok(url) = url::Url::parse(value) else {
            return false;
        };
        if url.as_str() == "about:blank" {
            return true;
        }
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        match url.host() {
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            _ => false,
        }
    }
}

fn canonical_uuid(value: &str) -> Result<String, String> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err("WebKit image UUID must be a canonical 36-character UUID".into());
    }
    Ok(value.to_ascii_uppercase())
}

pub fn policy() -> &'static Policy {
    POLICY.get_or_init(Policy::default)
}

/// Called before creating the application/windows, so profile checks do not block UI work.
pub fn initialize() -> Result<(), String> {
    let read = |name| match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(format!("{name} must contain valid text")),
    };
    let mode = read("FELINAUT_COMPARISON")?;
    let uuid = read("FELINAUT_EXPECTED_WEBKIT_UUID")?;
    let policy = Policy::parse(mode.as_deref(), uuid.as_deref())?;
    policy.verify_build(&crate::engine_build::compiled_ids())?;
    if policy.enabled() {
        validate_profile()?;
    }
    POLICY
        .set(policy)
        .map_err(|_| "Comparison policy was initialized before startup validation".to_owned())
}

fn validate_profile() -> Result<(), String> {
    let error = || {
        "Comparison requires a fresh private profile created by the comparison launcher".to_owned()
    };
    let path = |name| std::env::var_os(name).map(PathBuf::from).ok_or_else(error);
    validate_profile_paths(
        &path("FELINAUT_COMPARISON_HOME")?,
        &path("HOME")?,
        &path("CFFIXED_USER_HOME")?,
    )
}

fn validate_profile_paths(requested: &Path, home: &Path, cocoa_home: &Path) -> Result<(), String> {
    let error = || {
        "Comparison requires a fresh private profile created by the comparison launcher".to_owned()
    };
    let canonical = requested.canonicalize().map_err(|_| error())?;
    // Match the launcher's canonical path policy; never accept a symlink as the
    // comparison root, even if the ordinary HOME aliases resolve to that root.
    if requested != canonical
        || home.canonicalize().map_err(|_| error())? != canonical
        || cocoa_home.canonicalize().map_err(|_| error())? != canonical
    {
        return Err(error());
    }
    let home = canonical;
    let temporary = PathBuf::from("/tmp").canonicalize().map_err(|_| error())?;
    let marker = home.join(".felinaut-comparison");
    let marker_metadata = std::fs::symlink_metadata(&marker).map_err(|_| error())?;
    let expected_marker = b"felinaut-comparison-v1\n";
    let mut contents = Vec::new();
    if !marker_metadata.is_file() {
        return Err(error());
    }
    std::fs::File::open(marker)
        .map_err(|_| error())?
        .take(expected_marker.len() as u64 + 1)
        .read_to_end(&mut contents)
        .map_err(|_| error())?;
    if home.parent() != Some(temporary.as_path())
        || !home
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("felinaut-compare-"))
        || contents != expected_marker
    {
        return Err(error());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&home)
            .map_err(|_| error())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::{Capabilities, RuntimeIdentity};
    const UUID: &str = "AC293B1A-9EAF-34D8-86B2-914D2ABB84F3";

    fn info(source: RuntimeSource, uuid: Option<&str>) -> BackendInfo {
        BackendInfo {
            id: "native-webkit",
            engine_family: "webkit",
            runtime_version: None,
            runtime_identity: RuntimeIdentity {
                source,
                image_uuid: uuid.map(str::to_owned),
            },
            capabilities: Capabilities::default(),
        }
    }

    struct TestHome(PathBuf);

    impl TestHome {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = PathBuf::from("/tmp")
                .canonicalize()
                .unwrap()
                .join(format!("felinaut-compare-test-{}-{id}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(
                path.join(".felinaut-comparison"),
                b"felinaut-comparison-v1\n",
            )
            .unwrap();
            Self(path)
        }

        fn validate(&self) -> Result<(), String> {
            validate_profile_paths(&self.0, &self.0, &self.0)
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn marked_private_profile_is_accepted_without_changing_environment() {
        let home = TestHome::new();
        assert!(home.validate().is_ok());
        let other = TestHome::new();
        assert!(validate_profile_paths(&home.0, &other.0, &home.0).is_err());
        assert!(validate_profile_paths(&home.0, &home.0, &other.0).is_err());
        assert!(validate_profile_paths(&home.0.join("missing"), &home.0, &home.0).is_err());
    }

    #[test]
    fn marker_must_be_present_regular_and_exact() {
        let home = TestHome::new();
        let marker = home.0.join(".felinaut-comparison");
        std::fs::remove_file(&marker).unwrap();
        assert!(home.validate().is_err());
        for contents in [
            b"".as_slice(),
            b"felinaut-comparison-v1",
            b"felinaut-comparison-v1\nextra",
        ] {
            std::fs::write(&marker, contents).unwrap();
            assert!(home.validate().is_err());
        }
        std::fs::remove_file(&marker).unwrap();
        let other = TestHome::new();
        std::os::unix::fs::symlink(other.0.join(".felinaut-comparison"), &marker).unwrap();
        assert!(home.validate().is_err());
    }

    #[test]
    fn public_permissions_or_noncanonical_profile_roots_are_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let home = TestHome::new();
        for mode in [0o701, 0o710, 0o755, 0o777] {
            std::fs::set_permissions(&home.0, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(home.validate().is_err());
        }
        std::fs::set_permissions(&home.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = home.0.join("alias");
        std::os::unix::fs::symlink(&home.0, &alias).unwrap();
        assert!(validate_profile_paths(&alias, &alias, &alias).is_err());
        let nested = home.0.join("felinaut-compare-nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            nested.join(".felinaut-comparison"),
            b"felinaut-comparison-v1\n",
        )
        .unwrap();
        assert!(validate_profile_paths(&nested, &nested, &nested).is_err());
    }

    #[test]
    fn default_keeps_normal_navigation_but_requires_verified_system_identity() {
        let policy = Policy::parse(None, None).unwrap();
        assert!(!policy.enabled());
        assert!(policy.label().is_none());
        assert!(policy.allows_url("https://example.com/"));
        assert!(
            policy
                .verify_runtime(&info(RuntimeSource::SystemSharedCache, Some(UUID)))
                .is_ok()
        );
    }

    #[test]
    fn unidentified_or_external_runtime_cannot_enter_normal_browsing() {
        let policy = Policy::default();
        for source in [RuntimeSource::Unverified, RuntimeSource::ExternalFramework] {
            for uuid in [None, Some(UUID)] {
                assert!(policy.verify_runtime(&info(source, uuid)).is_err());
            }
        }
        assert!(
            policy
                .verify_runtime(&info(RuntimeSource::SystemSharedCache, None))
                .is_err()
        );
        assert!(
            policy
                .verify_runtime(&info(RuntimeSource::SystemSharedCache, Some("invalid")))
                .is_err()
        );
        let mut wrong = info(RuntimeSource::SystemSharedCache, Some(UUID));
        wrong.engine_family = "chromium";
        assert!(policy.verify_runtime(&wrong).is_err());
        wrong.engine_family = "webkit";
        wrong.id = "different-backend";
        assert!(policy.verify_runtime(&wrong).is_err());
    }

    #[test]
    fn cef_only_normal_startup_is_distinct_from_cocoa_runtime_comparison() {
        assert!(Policy::default().verify_build(&["cef"]).is_ok());
        assert!(
            Policy::default()
                .verify_build(&["system-webkit", "cef"])
                .is_ok()
        );
        for policy in [
            Policy::parse(Some("system"), None).unwrap(),
            Policy::parse(Some("upstream"), Some(UUID)).unwrap(),
        ] {
            assert!(
                policy
                    .verify_build(&["system-webkit", "upstream-webkit", "cef"])
                    .is_err()
            );
        }
    }

    #[test]
    fn comparison_selection_cannot_enable_a_backend_excluded_at_build_time() {
        let system = Policy::parse(Some("system"), None).unwrap();
        let upstream = Policy::parse(Some("upstream"), Some(UUID)).unwrap();
        let both = ["system-webkit", "upstream-webkit"];
        assert!(system.verify_build(&both).is_ok());
        assert!(upstream.verify_build(&both).is_ok());
        assert!(system.verify_build(&["system-webkit"]).is_ok());
        assert!(upstream.verify_build(&["upstream-webkit"]).is_ok());
        assert!(system.verify_build(&["upstream-webkit"]).is_err());
        assert!(upstream.verify_build(&["system-webkit"]).is_err());
        assert!(
            Policy::default()
                .verify_build(&["upstream-webkit"])
                .is_err()
        );
        assert!(Policy::default().verify_build(&["system-webkit"]).is_ok());
        assert!(Policy::default().verify_build(&[]).is_err());
        assert!(
            Policy::default()
                .verify_runtime(&info(RuntimeSource::ExternalFramework, Some(UUID)))
                .is_err()
        );
    }

    #[test]
    fn invalid_comparison_requests_fail_instead_of_starting_normally() {
        for mode in ["", "1", "wpe", "Upstream", "upstream "] {
            assert!(Policy::parse(Some(mode), Some(UUID)).is_err());
        }
        assert!(Policy::parse(None, Some(UUID)).is_err());
        assert!(Policy::parse(Some("upstream"), None).is_err());
        for uuid in [
            "",
            "abc",
            "../runtime",
            "AC293B1A_9EAF-34D8-86B2-914D2ABB84F3",
            " AC293B1A-9EAF-34D8-86B2-914D2ABB84F3",
        ] {
            assert!(Policy::parse(Some("upstream"), Some(uuid)).is_err());
        }
    }

    #[test]
    fn actual_image_source_and_uuid_must_match_the_requested_runtime() {
        let policy = Policy::parse(Some("upstream"), Some(&UUID.to_lowercase())).unwrap();
        assert!(
            policy
                .verify_runtime(&info(RuntimeSource::ExternalFramework, Some(UUID)))
                .is_ok()
        );
        for source in [RuntimeSource::SystemSharedCache, RuntimeSource::Unverified] {
            assert!(policy.verify_runtime(&info(source, Some(UUID))).is_err());
        }
        assert!(
            policy
                .verify_runtime(&info(RuntimeSource::ExternalFramework, None))
                .is_err()
        );
        assert!(
            policy
                .verify_runtime(&info(
                    RuntimeSource::ExternalFramework,
                    Some("00000000-0000-0000-0000-000000000000")
                ))
                .is_err()
        );
        let system = Policy::parse(Some("system"), None).unwrap();
        assert!(
            system
                .verify_runtime(&info(RuntimeSource::SystemSharedCache, Some(UUID)))
                .is_ok()
        );
        assert!(
            system
                .verify_runtime(&info(RuntimeSource::ExternalFramework, Some(UUID)))
                .is_err()
        );
    }

    #[test]
    fn comparison_navigation_is_explicitly_loopback_only() {
        let policy = Policy::parse(Some("system"), None).unwrap();
        for url in [
            "about:blank",
            "http://127.0.0.1:8765/",
            "https://[::1]/",
            "http://127.0.0.2/",
        ] {
            assert!(policy.allows_url(url), "{url}");
        }
        for url in [
            "https://example.com/",
            "http://localhost/",
            "http://127.0.0.1.example.com/",
            "about:config",
            "file:///tmp/test",
            "javascript:alert(1)",
            "data:text/html,hi",
            "invalid",
            "http://192.168.1.1/",
        ] {
            assert!(!policy.allows_url(url), "{url}");
        }
    }
}
