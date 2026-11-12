fn main() {
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("macos"),
        "The CEF application probe requires native macOS"
    );
    println!("cargo:rerun-if-changed=native/application.mm");
    println!("cargo:rerun-if-changed=native/pump.mm");
    cc::Build::new()
        .cpp(true)
        .file("native/application.mm")
        .file("native/pump.mm")
        .flag("-std=c++17")
        .flag("-fblocks")
        .flag("-fno-objc-arc")
        .warnings_into_errors(true)
        .compile("felinaut_cef_application_probe");
    println!("cargo:rustc-link-lib=framework=AppKit");
}
