fn main() {
    assert_eq!(
        std::env::var("TARGET").as_deref(),
        Ok("aarch64-apple-darwin"),
        "The production CEF helper currently requires native macOS arm64"
    );
    println!("cargo:rerun-if-env-changed=CEF_PATH");
    let sdk = std::path::PathBuf::from(
        std::env::var_os("CEF_PATH").expect("Set CEF_PATH to the already prepared pinned CEF SDK"),
    );
    assert!(sdk.is_absolute(), "CEF_PATH must be absolute");
    let header = sdk.join("include/cef_version.h");
    let version = std::fs::read_to_string(&header).expect("Prepared CEF version header missing");
    assert!(
        version.lines().any(|line| line.trim()
            == "#define CEF_VERSION \"156.0.26+g8ea0c73+chromium-156.0.8078.88\""),
        "CEF header does not match the root's pinned SDK/runtime pair"
    );
    println!("cargo:rerun-if-changed={}", header.display());
}
