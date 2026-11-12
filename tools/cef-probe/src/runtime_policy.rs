use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub const MARKER: &[u8] = b"felinaut-cef-runtime-v1\n";

pub fn target_url(value: &str) -> Result<url::Url, String> {
    if value.len() > 2048 {
        return Err("Loopback fixture URL exceeds the probe limit".into());
    }
    let url = url::Url::parse(value).map_err(|_| "Invalid loopback fixture URL")?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http" || !loopback || !url.username().is_empty() || url.password().is_some()
    {
        return Err("CEF probe requires an HTTP fixture at a literal loopback address".into());
    }
    Ok(url)
}

pub fn allows_resource(value: &str, target: &url::Url) -> bool {
    target_url(value).is_ok_and(|url| url.origin() == target.origin())
}

pub fn allows_navigation(value: &str, target: &url::Url) -> bool {
    value == "about:blank" || allows_resource(value, target)
}

#[derive(Default)]
pub struct CloseGate {
    requested: bool,
    created: bool,
    dispatched: bool,
}

impl CloseGate {
    pub fn request(&mut self) {
        self.requested = true;
    }

    pub fn created(&mut self) {
        self.created = true;
    }

    pub fn should_dispatch(&self) -> bool {
        self.requested && self.created && !self.dispatched
    }

    pub fn dispatched(&mut self) {
        self.dispatched = true;
    }
}

pub fn profile(requested: &Path, home: &Path, cocoa: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let error = || "CEF probe requires a fresh, marked private temporary home".to_owned();
    let root = requested.canonicalize().map_err(|_| error())?;
    let temporary = Path::new("/tmp").canonicalize().map_err(|_| error())?;
    if requested != root
        || root.parent() != Some(temporary.as_path())
        || !root
            .file_name()
            .and_then(|part| part.to_str())
            .is_some_and(|part| part.starts_with("felinaut-cef-probe-"))
        || home.canonicalize().map_err(|_| error())? != root
        || cocoa.canonicalize().map_err(|_| error())? != root
        || std::fs::metadata(&root)
            .map_err(|_| error())?
            .permissions()
            .mode()
            & 0o077
            != 0
    {
        return Err(error());
    }
    let marker = root.join(".felinaut-cef-probe");
    if !std::fs::symlink_metadata(&marker)
        .map_err(|_| error())?
        .is_file()
    {
        return Err(error());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(marker)
        .map_err(|_| error())?
        .take(MARKER.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error())?;
    if bytes != MARKER {
        return Err(error());
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_loopback_http_without_credentials_is_accepted() {
        for value in [
            "http://127.0.0.1:8765/",
            "http://[::1]:1234/",
            "http://127.0.0.2/",
        ] {
            assert!(target_url(value).is_ok(), "{value}");
        }
        for value in [
            "",
            "garbage",
            "https://127.0.0.1/",
            "file:///tmp/a",
            "http://localhost/",
            "http://example.com/",
            "http://127.0.0.1.example.com/",
            "http://u:p@127.0.0.1/",
            "data:text/html,hi",
        ] {
            assert!(target_url(value).is_err(), "{value}");
        }
        assert!(target_url(&format!("http://127.0.0.1/{}", "x".repeat(2048))).is_err());
    }

    #[test]
    fn navigation_stays_on_the_fixture_origin() {
        let target = target_url("http://127.0.0.1:8765/fixture").unwrap();
        assert!(allows_navigation("about:blank", &target));
        assert!(allows_navigation("http://127.0.0.1:8765/other", &target));
        for value in [
            "http://127.0.0.1:8766/",
            "http://[::1]:8765/",
            "http://example.com/",
            "javascript:alert(1)",
        ] {
            assert!(!allows_navigation(value, &target));
        }
    }

    #[test]
    fn resources_and_redirects_require_the_exact_http_origin() {
        let target = target_url("http://127.0.0.1:8765/").unwrap();
        assert!(allows_resource("http://127.0.0.1:8765/image.png", &target));
        for value in [
            "",
            "about:blank",
            "data:text/plain,test",
            "file:///tmp/test",
            "https://127.0.0.1:8765/",
            "http://127.0.0.1:8766/",
            "http://example.com/",
            "http://user:secret@127.0.0.1:8765/",
        ] {
            assert!(!allows_resource(value, &target), "{value}");
        }
    }

    #[test]
    fn quit_before_async_creation_dispatches_exactly_one_close_when_ready() {
        let mut gate = CloseGate::default();
        assert!(!gate.should_dispatch());
        gate.request();
        gate.request();
        assert!(!gate.should_dispatch());
        gate.created();
        assert!(gate.should_dispatch());
        // An unavailable host can be retried; only a real request marks dispatch.
        assert!(gate.should_dispatch());
        gate.dispatched();
        gate.created();
        gate.request();
        assert!(!gate.should_dispatch());
    }

    #[test]
    fn creation_alone_does_not_close_and_later_quit_does() {
        let mut gate = CloseGate::default();
        gate.created();
        assert!(!gate.should_dispatch());
        gate.request();
        assert!(gate.should_dispatch());
        gate.dispatched();
        assert!(!gate.should_dispatch());
    }

    #[test]
    fn profiles_require_matching_homes_marker_and_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let path = Path::new("/tmp")
            .canonicalize()
            .unwrap()
            .join(format!("felinaut-cef-probe-unit-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let marker = path.join(".felinaut-cef-probe");
        assert!(profile(&path, &path, &path).is_err());
        std::fs::write(&marker, MARKER).unwrap();
        assert!(profile(&path, &path, &path).is_ok());
        assert!(profile(&path, Path::new("/tmp"), &path).is_err());
        std::fs::write(&marker, b"wrong marker").unwrap();
        assert!(profile(&path, &path, &path).is_err());
        std::fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink("/dev/null", &marker).unwrap();
        assert!(profile(&path, &path, &path).is_err());
        std::fs::remove_file(&marker).unwrap();
        std::fs::write(&marker, MARKER).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(profile(&path, &path, &path).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }
}
