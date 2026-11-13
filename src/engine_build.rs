//! Build-time backend selection, shared by build.rs and the browser.
//! A feature request is not proof that a native adapter/runtime exists.

#[derive(Clone, Copy, Debug)]
pub struct Backend {
    pub id: &'static str,
    pub feature: &'static str,
    pub family: &'static str,
    pub adapter_implemented: bool,
    pub requires_isolated_launcher: bool,
    pub limitation: &'static str,
}

pub const BACKENDS: &[Backend] = &[
    Backend {
        id: "system-webkit",
        feature: "engine-webkit-system",
        family: "webkit",
        adapter_implemented: true,
        requires_isolated_launcher: false,
        limitation: "macOS WKWebView; supplied by the operating system",
    },
    Backend {
        id: "upstream-webkit",
        feature: "engine-webkit-upstream",
        family: "webkit",
        adapter_implemented: true,
        requires_isolated_launcher: true,
        limitation: "Experimental macOS Cocoa runtime; trusted local products and isolated comparison launcher required, not per-tab coexistence or a distribution bundle",
    },
    Backend {
        id: "cef",
        feature: "engine-cef",
        family: "chromium",
        adapter_implemented: false,
        requires_isolated_launcher: false,
        limitation: "CEF adapter not implemented; standalone same-window coexistence passed, but browser page factory, lifecycle, profiles and security integration are not ready",
    },
    Backend {
        id: "gecko",
        feature: "engine-gecko",
        family: "gecko",
        adapter_implemented: false,
        requires_isolated_launcher: false,
        limitation: "Gecko adapter not implemented; maintained desktop embedding API not established",
    },
    Backend {
        id: "servo",
        feature: "engine-servo",
        family: "servo",
        adapter_implemented: false,
        requires_isolated_launcher: false,
        limitation: "Servo adapter not implemented; presentation/input integration and arm64 sandboxing unresolved",
    },
    Backend {
        id: "ladybird",
        feature: "engine-ladybird",
        family: "ladybird",
        adapter_implemented: false,
        requires_isolated_launcher: false,
        limitation: "Ladybird adapter not implemented; maintained standalone embedding boundary not established",
    },
    Backend {
        id: "ultralight",
        feature: "engine-ultralight",
        family: "webkit",
        adapter_implemented: false,
        requires_isolated_launcher: false,
        limitation: "Ultralight adapter not implemented; proprietary licensing, single-process isolation and accessibility require review",
    },
    Backend {
        id: "wpe",
        feature: "engine-wpe",
        family: "webkit",
        adapter_implemented: false,
        requires_isolated_launcher: false,
        limitation: "WPE adapter not implemented; Linux investigation, no validated native macOS or Windows port",
    },
];

pub fn compiled_ids() -> Vec<&'static str> {
    [
        (cfg!(feature = "engine-webkit-system"), "system-webkit"),
        (cfg!(feature = "engine-webkit-upstream"), "upstream-webkit"),
        (cfg!(feature = "engine-cef"), "cef"),
        (cfg!(feature = "engine-gecko"), "gecko"),
        (cfg!(feature = "engine-servo"), "servo"),
        (cfg!(feature = "engine-ladybird"), "ladybird"),
        (cfg!(feature = "engine-ultralight"), "ultralight"),
        (cfg!(feature = "engine-wpe"), "wpe"),
    ]
    .into_iter()
    .filter_map(|(enabled, id)| enabled.then_some(id))
    .collect()
}

pub fn require_backend(selected: &[&str], id: &str) -> Result<&'static Backend, String> {
    let backend = BACKENDS
        .iter()
        .find(|backend| backend.id == id)
        .ok_or_else(|| format!("Unknown backend {id:?}"))?;
    if !backend.adapter_implemented {
        return Err(format!(
            "{} ({}): {}",
            backend.id, backend.family, backend.limitation
        ));
    }
    if !selected.contains(&id) {
        return Err(format!(
            "Backend {} was excluded from this build; rebuild with --features {}",
            backend.id, backend.feature
        ));
    }
    Ok(backend)
}

pub fn validate(target_os: &str, selected: &[&str]) -> Result<(), String> {
    if selected.is_empty() {
        return Err("Select at least one implemented engine: engine-webkit-system or engine-webkit-upstream".into());
    }
    for (index, id) in selected.iter().enumerate() {
        if selected[..index].contains(id) {
            return Err(format!("Duplicate backend {id:?}"));
        }
        require_backend(selected, id)?;
    }
    if target_os != "macos" {
        return Err(format!(
            "No native Felinaut adapter implemented for target OS {target_os:?}; Cargo features do not establish platform support"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_working_webkit_combination_is_valid() {
        for selected in [
            vec!["system-webkit"],
            vec!["upstream-webkit"],
            vec!["system-webkit", "upstream-webkit"],
        ] {
            assert!(validate("macos", &selected).is_ok());
            for id in &selected {
                assert!(require_backend(&selected, id).is_ok());
            }
        }
        assert!(require_backend(&["system-webkit"], "upstream-webkit").is_err());
        assert!(require_backend(&["upstream-webkit"], "system-webkit").is_err());
    }

    #[test]
    fn rejects_empty_unknown_duplicate_and_unsupported_platforms() {
        for selected in [
            vec![],
            vec!["unknown"],
            vec!["system-webkit", "system-webkit"],
        ] {
            assert!(validate("macos", &selected).is_err());
        }
        for os in ["linux", "windows", "", "MacOS"] {
            assert!(validate(os, &["system-webkit"]).is_err());
            assert!(validate(os, &["upstream-webkit"]).is_err());
        }
    }

    #[test]
    fn unimplemented_candidates_fail_even_with_a_working_fallback() {
        for backend in BACKENDS
            .iter()
            .filter(|backend| !backend.adapter_implemented)
        {
            for selected in [vec![backend.id], vec!["system-webkit", backend.id]] {
                let error = validate("macos", &selected).unwrap_err();
                assert!(error.contains(backend.limitation));
                assert!(error.contains("not implemented"));
            }
        }
    }

    #[test]
    fn catalog_ids_and_features_are_unique_and_isolation_is_explicit() {
        for (index, backend) in BACKENDS.iter().enumerate() {
            assert!(!BACKENDS[..index].iter().any(|other| other.id == backend.id));
            assert!(
                !BACKENDS[..index]
                    .iter()
                    .any(|other| other.feature == backend.feature)
            );
            assert!(backend.feature.starts_with("engine-"));
            assert!(!backend.family.is_empty());
            assert!(!backend.limitation.is_empty());
            assert_eq!(
                backend.requires_isolated_launcher,
                backend.id == "upstream-webkit"
            );
        }
        assert!(validate("macos", &compiled_ids()).is_ok());
    }
}
