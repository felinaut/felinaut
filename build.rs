use std::{path::PathBuf, process::Command};

#[path = "src/engine_build.rs"]
pub mod engine_build;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn parse_version(version: &str) -> Result<[u64; 3], &'static str> {
    let mut components = version.split('.');
    let mut parsed = [0; 3];
    for value in &mut parsed {
        let component = components.next().ok_or("expected canonical ASCII M.N.P")?;
        if component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
        {
            return Err("expected canonical ASCII M.N.P without leading zeros or whitespace");
        }
        *value = component
            .parse()
            .map_err(|_| "version components must be in 0..u64::MAX")?;
    }
    if components.next().is_some() {
        return Err("expected exactly three version components (M.N.P)");
    }
    Ok(parsed)
}

fn build_version() -> Result<String, String> {
    let base = std::env::var("CARGO_PKG_VERSION")
        .map_err(|error| format!("Cannot read CARGO_PKG_VERSION: {error}"))?;
    let source =
        parse_version(&base).map_err(|error| format!("Invalid CARGO_PKG_VERSION: {error}"))?;
    let version = match std::env::var("FELINAUT_RELEASE_VERSION") {
        Ok(version) => version,
        Err(std::env::VarError::NotPresent) => return Ok(base),
        Err(error) => return Err(format!("Invalid FELINAUT_RELEASE_VERSION: {error}")),
    };
    let release = parse_version(&version)
        .map_err(|error| format!("Invalid FELINAUT_RELEASE_VERSION: {error}"))?;
    if release[..2] != source[..2] || release[2] < source[2] {
        return Err(format!(
            "FELINAUT_RELEASE_VERSION must have the same major/minor as CARGO_PKG_VERSION ({base}) and a patch at least as large as its source patch"
        ));
    }
    Ok(version)
}

fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/engine_build.rs");
    engine_build::validate(
        &std::env::var("CARGO_CFG_TARGET_OS").map_err(|error| error.to_string())?,
        &engine_build::compiled_ids(),
    )?;
    if cfg!(feature = "engine-cef")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64")
    {
        return Err("The optional CEF runtime currently requires native macOS arm64; other architectures are not validated".into());
    }
    println!("cargo:rerun-if-env-changed=FELINAUT_RELEASE_VERSION");
    println!(
        "cargo:rustc-env=FELINAUT_BUILD_VERSION={}",
        build_version()?
    );
    // Resolve Git paths rather than assuming .git is a directory (worktrees use a file).
    let mut paths = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        paths.push(reference);
    }
    for path in paths {
        if let Some(path) = git(&["rev-parse", "--git-path", &path]) {
            println!("cargo:rerun-if-changed={}", PathBuf::from(path).display());
        }
    }
    let revision = git(&["rev-parse", "--verify", "HEAD"])
        .filter(|sha| matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|sha| sha[..12].to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=FELINAUT_GIT_COMMIT={revision}");
    Ok(())
}
