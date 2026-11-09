use std::{
    path::Path,
    process::{Command, ExitCode},
};

fn release_command(root: &Path, host: &str) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(root)
        .args([
            "build",
            "--release",
            "--locked",
            "--bin",
            "felifox",
            "--target",
            host,
        ])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .env("MACOSX_DEPLOYMENT_TARGET", "12.0")
        // Strip only the final packaged executable, not build-time macro metadata.
        .env("CARGO_PROFILE_RELEASE_STRIP", "none");
    command
}

fn dist() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        println!(
            "cargo dist [--app-only]\n\nBuild the current release, then package dist/Felifox.app and the illustrated DMG.\nPrevious artifacts are replaced. Ad-hoc signed unless FELIFOX_SIGN_IDENTITY (Developer ID)\nand FELIFOX_NOTARY_PROFILE (notarytool keychain profile) are set; see README."
        );
        return Ok(());
    }
    if args.iter().any(|arg| arg != "--app-only") {
        return Err(
            "Usage: cargo dist [--app-only]. Distribution always builds release first.".into(),
        );
    }
    if !cfg!(target_os = "macos") {
        return Err("macOS distribution requires macOS.".into());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let rustc = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("Could not run rustc: {e}"))?;
    if !rustc.status.success() {
        return Err("rustc -vV failed.".into());
    }
    let version = String::from_utf8_lossy(&rustc.stdout);
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .filter(|host| matches!(*host, "aarch64-apple-darwin" | "x86_64-apple-darwin"))
        .ok_or("A native macOS Rust toolchain is required.")?;

    println!("1/2 Building Felifox release ({host})…");
    let status = release_command(&root, host)
        .status()
        .map_err(|e| format!("Could not start release build: {e}"))?;
    if !status.success() {
        return Err(format!(
            "Release build failed ({status}); packaging was not started."
        ));
    }

    println!("2/2 Packaging the freshly built release…");
    let status = Command::new("python3")
        .current_dir(&root)
        .arg(root.join("scripts/package_macos.py"))
        // This is safe only after the successful build above; don't compile twice.
        .args(["--skip-build", "--overwrite"])
        .args(args)
        .status()
        .map_err(|e| format!("Could not start packager (Python 3.11+ required): {e}"))?;
    if !status.success() {
        return Err(format!("Packaging failed ({status})."));
    }
    Ok(())
}

fn main() -> ExitCode {
    match dist() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cargo dist: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn release_build_is_locked_and_matches_the_packager_target() {
        let command = release_command(Path::new("/project"), "aarch64-apple-darwin");
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[..7],
            &[
                "build",
                "--release",
                "--locked",
                "--bin",
                "felifox",
                "--target",
                "aarch64-apple-darwin"
            ]
        );
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "CARGO_PROFILE_RELEASE_STRIP"
                    && value == Some(std::ffi::OsStr::new("none")))
        );
    }
}
