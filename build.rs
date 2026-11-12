use std::{path::PathBuf, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
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
}
