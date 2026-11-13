fn main() {
    // The shared adapter has a root-browser runtime gate that this harness never enables.
    println!("cargo:rustc-check-cfg=cfg(feature, values(\"engine-cef\"))");
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("macos"),
        "The CEF application probe requires native macOS"
    );
    println!("cargo:rerun-if-changed=native/application.mm");
    println!("cargo:rerun-if-changed=native/pump.mm");
    println!("cargo:rerun-if-changed=native/protocols.m");
    println!("cargo:rerun-if-env-changed=CEF_PATH");
    let mut protocols = None;
    let mut native = cc::Build::new();
    native
        .cpp(true)
        .file("native/application.mm")
        .file("native/pump.mm")
        .flag("-std=c++17")
        .flag("-fblocks")
        .flag("-fno-objc-arc")
        .warnings_into_errors(true);
    if std::env::var_os("CARGO_FEATURE_RUNTIME").is_some() {
        let sdk = std::path::PathBuf::from(
            std::env::var_os("CEF_PATH")
                .expect("Run prepare_runtime.py and set its absolute CEF_PATH"),
        );
        assert!(sdk.is_absolute(), "CEF_PATH must be absolute");
        let header = sdk.join("include/cef_version.h");
        let version =
            std::fs::read_to_string(&header).expect("Prepared CEF version header missing");
        assert!(
            version.lines().any(|line| line.trim()
                == "#define CEF_VERSION \"156.0.26+g8ea0c73+chromium-156.0.8078.88\""),
            "CEF header does not match the pinned binding/runtime pair"
        );
        println!("cargo:rerun-if-changed={}", header.display());
        native.define("FELINAUT_CEF_RUNTIME", None);
        let mut bridge = cc::Build::new();
        bridge
            .file("native/protocols.m")
            .include(sdk)
            .define("OS_MAC", None)
            .flag("-fno-objc-arc")
            .warnings_into_errors(true);
        protocols = Some(bridge);
    }
    native.compile("felinaut_cef_application_probe");
    if let Some(bridge) = protocols {
        bridge.compile("felinaut_cef_protocols");
    }
    println!("cargo:rustc-link-lib=framework=AppKit");
}
