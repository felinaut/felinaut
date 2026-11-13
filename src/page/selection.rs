//! Pure per-tab backend selection. Native creation, confirmation of lost work, document
//! scopes and page ownership remain with the host; switching only reloads a URL.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Engine {
    #[default]
    WebKit,
    Cef,
}

impl Engine {
    pub fn id(self) -> &'static str {
        match self {
            Self::WebKit => "webkit",
            Self::Cef => "cef",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::WebKit => "WebKit",
            Self::Cef => "Chromium (CEF)",
        }
    }

    /// Exact stable IDs only: unknown saved values require explicit recovery.
    pub fn parse(id: &str) -> Result<Self, String> {
        match id {
            "webkit" => Ok(Self::WebKit),
            "cef" => Ok(Self::Cef),
            _ => Err("Unknown tab backend; choose an available engine to recover".into()),
        }
    }

    /// `runtime_ready` describes the requested engine, not the other engine's runtime.
    /// Build/runtime flags are supplied by the host, never inferred from the OS.
    pub fn check_available(
        self,
        compiled_webkit: bool,
        compiled_cef: bool,
        runtime_ready: bool,
    ) -> Result<(), String> {
        let compiled = match self {
            Self::WebKit => compiled_webkit,
            Self::Cef => compiled_cef,
        };
        if !compiled {
            Err(format!("{} is not included in this build", self.label()))
        } else if !runtime_ready {
            Err(format!("{} runtime is not ready", self.label()))
        } else {
            Ok(())
        }
    }
}

/// A validated reload, not a migrated document. Creating this plan changes no selection.
/// The host must retain the old page until replacement creation and readiness succeed,
/// and discard the plan if its tab/document changes while initialization is pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchPlan {
    original: String,
    engine: Engine,
    url: String,
}

impl SwitchPlan {
    pub fn engine(&self) -> Engine {
        self.engine
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Commit only after the host reports replacement readiness. Failure and stale
    /// selections leave the original raw ID (including unknown IDs) untouched.
    pub fn complete(
        self,
        current: &mut String,
        replacement_ready: Result<(), String>,
    ) -> Result<(), String> {
        if *current != self.original {
            return Err("Tab backend changed before replacement was ready".into());
        }
        replacement_ready?;
        *current = self.engine.id().into();
        Ok(())
    }
}

/// `None` is an unchanged selection: no reload or replacement is needed. This only
/// accepts credential-free HTTP(S); the host's local New Tab is a separate policy,
/// and must never be passed to a backend as an arbitrary local/file URL.
pub fn validate_switch(
    current: &str,
    requested: Engine,
    url: &str,
    compiled_webkit: bool,
    compiled_cef: bool,
    runtime_ready: bool,
) -> Result<Option<SwitchPlan>, String> {
    if current == requested.id() {
        return Ok(None);
    }
    requested.check_available(compiled_webkit, compiled_cef, runtime_ready)?;
    validate_reload_url(url)?;
    Ok(Some(SwitchPlan {
        original: current.into(),
        engine: requested,
        url: url.into(),
    }))
}

fn validate_reload_url(value: &str) -> Result<(), String> {
    let invalid = || "Switching engines requires a valid credential-free HTTP(S) URL".to_owned();
    // WHATWG parsing repairs missing slashes, backslashes and embedded whitespace.
    // A reload must not reinterpret such malformed input into a different destination.
    if value.is_empty()
        || value.len() > 64 * 1024
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
        || value.contains('\\')
    {
        return Err(invalid());
    }
    let (scheme, rest) = value.split_once("://").ok_or_else(invalid)?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Err(invalid());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid());
    }
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !(bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return Err(invalid());
        }
    }
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

/// Only the exact trusted local Home identifier is eligible for the host's New Tab
/// selection path. This does not broaden the URL reload policy above.
pub fn is_local_home(value: &str) -> bool {
    value == "felinaut://newtab"
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEB: &str = "https://example.test/path?q=1#section";

    #[test]
    fn stable_ids_labels_and_default() {
        assert_eq!(Engine::default(), Engine::WebKit);
        for (engine, id, label) in [
            (Engine::WebKit, "webkit", "WebKit"),
            (Engine::Cef, "cef", "Chromium (CEF)"),
        ] {
            assert_eq!(engine.id(), id);
            assert_eq!(engine.label(), label);
            assert_eq!(Engine::parse(id), Ok(engine));
        }
        for raw in ["", "WebKit", "CEF", " cef", "cef\n", "gecko", "null"] {
            assert!(Engine::parse(raw).is_err(), "{raw:?}");
        }
    }

    #[test]
    fn inclusion_and_runtime_are_independent_and_never_fall_back() {
        for webkit in [false, true] {
            for cef in [false, true] {
                for ready in [false, true] {
                    assert_eq!(
                        Engine::WebKit.check_available(webkit, cef, ready).is_ok(),
                        webkit && ready
                    );
                    assert_eq!(
                        Engine::Cef.check_available(webkit, cef, ready).is_ok(),
                        cef && ready
                    );
                }
            }
        }
        assert!(
            Engine::Cef
                .check_available(true, false, true)
                .unwrap_err()
                .contains("build")
        );
        assert!(
            Engine::Cef
                .check_available(true, true, false)
                .unwrap_err()
                .contains("runtime")
        );
    }

    #[test]
    fn successful_switch_waits_for_replacement_readiness() {
        for (original, target) in [("webkit", Engine::Cef), ("cef", Engine::WebKit)] {
            let mut current = original.to_owned();
            let plan = validate_switch(&current, target, WEB, true, true, true)
                .unwrap()
                .unwrap();
            assert_eq!(current, original);
            assert_eq!(plan.engine(), target);
            assert_eq!(plan.url(), WEB);
            plan.complete(&mut current, Ok(())).unwrap();
            assert_eq!(current, target.id());
        }
    }

    #[test]
    fn failed_replacement_preserves_original_including_unknown_id() {
        for original in ["webkit", "future-engine"] {
            let mut current = original.to_owned();
            let plan = validate_switch(&current, Engine::Cef, WEB, true, true, true)
                .unwrap()
                .unwrap();
            assert_eq!(
                plan.complete(&mut current, Err("creation failed".into())),
                Err("creation failed".into())
            );
            assert_eq!(current, original);
        }
    }

    #[test]
    fn stale_selection_cannot_be_overwritten() {
        let plan = validate_switch("webkit", Engine::Cef, WEB, true, true, true)
            .unwrap()
            .unwrap();
        let mut current = "other-selection".into();
        assert!(plan.complete(&mut current, Ok(())).is_err());
        assert_eq!(current, "other-selection");
    }

    #[test]
    fn same_engine_does_not_reload_even_if_runtime_is_now_unavailable() {
        for engine in [Engine::WebKit, Engine::Cef] {
            assert_eq!(
                validate_switch(engine.id(), engine, "", false, false, false),
                Ok(None)
            );
        }
    }

    #[test]
    fn unavailable_targets_are_errors_and_do_not_change_selection() {
        let current = "webkit";
        for (compiled, ready) in [(false, true), (true, false), (false, false)] {
            assert!(validate_switch(current, Engine::Cef, WEB, true, compiled, ready).is_err());
            assert_eq!(current, "webkit");
        }
        // An unknown saved ID is not defaulted; explicit recovery can select WebKit.
        assert!(Engine::parse("future-engine").is_err());
        let mut current = "future-engine".to_owned();
        validate_switch(&current, Engine::WebKit, WEB, true, false, true)
            .unwrap()
            .unwrap()
            .complete(&mut current, Ok(()))
            .unwrap();
        assert_eq!(current, "webkit");
    }

    #[test]
    fn valid_web_reload_urls_are_preserved() {
        for url in [
            WEB,
            "http://localhost:3000/",
            "http://[::1]:8000/",
            "HTTPS://example.test/",
            "https://example.test/a%20b",
            "https://example.test/日本語",
            "https://example.test/?email=a@b",
        ] {
            let plan = validate_switch("webkit", Engine::Cef, url, true, true, true)
                .unwrap()
                .unwrap();
            assert_eq!(plan.url(), url);
        }
    }

    #[test]
    fn unsafe_or_malformed_reload_urls_are_rejected() {
        for url in [
            "",
            "not a url",
            "example.test",
            "//example.test/",
            "file:///tmp/a",
            "javascript:alert(1)",
            "data:text/html,test",
            "about:blank",
            "felinaut://newtab",
            "https://user:secret@example.test/",
            "https://user@example.test/",
            "https://@example.test/",
            "https://example.test@evil.test/",
            "https:///example.test/",
            "https:example.test",
            "https://",
            "https://?q=1",
            "https://example.test:bad/",
            "https://[invalid]/",
            " https://example.test/",
            "https://exa\nmple.test/",
            "https://example.test/ ",
            "https://example.test/\\evil",
            "https://example.test/%",
            "https://example.test/%z0",
            "https://example.test/\0",
        ] {
            assert!(
                validate_switch("webkit", Engine::Cef, url, true, true, true).is_err(),
                "{url:?}"
            );
        }
        assert!(
            validate_reload_url(&format!("https://example.test/{}", "a".repeat(64 * 1024)))
                .is_err()
        );
    }

    #[test]
    fn local_home_policy_is_exact_and_separate_from_reload() {
        assert!(is_local_home("felinaut://newtab"));
        assert!(validate_reload_url("felinaut://newtab").is_err());
        for value in [
            "felinaut://newtab/",
            "felinaut://newtab?url=file:///tmp/a",
            "felinaut://newtab#x",
            "FELINAUT://newtab",
            "file:///tmp/newtab",
            "https://newtab/",
            "",
        ] {
            assert!(!is_local_home(value));
        }
    }
}
