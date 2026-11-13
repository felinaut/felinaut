use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::Path,
};

const FRAMEWORK: &str = "Chromium Embedded Framework.framework";
const FRAMEWORK_BINARY: &str = "Chromium Embedded Framework";
const SANDBOX_BINARY: &str = "Libraries/libcef_sandbox.dylib";
const HELPERS: [&str; 5] = [
    "felinaut Helper",
    "felinaut Helper (GPU)",
    "felinaut Helper (Renderer)",
    "felinaut Helper (Plugin)",
    "felinaut Helper (Alerts)",
];

/// Parse without calling CEF: sandbox setup must precede the CEF loader.
/// Chromium recognizes single- as well as double-dash switches on POSIX.
pub fn arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut role = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let argument = argument.to_str().ok_or("Helper arguments must be UTF-8")?;
        if argument == "--" || argument.contains('\0') {
            return Err("Invalid helper switch boundary".into());
        }
        let Some(switch) = argument
            .strip_prefix("--")
            .or_else(|| argument.strip_prefix('-'))
        else {
            continue;
        };
        let (name, value) = switch
            .split_once('=')
            .map_or((switch, None), |(name, value)| (name, Some(value)));
        if matches!(
            name,
            "no-sandbox"
                | "single-process"
                | "in-process-gpu"
                | "no-zygote"
                | "disable-gpu-sandbox"
                | "disable-seccomp-filter-sandbox"
                | "disable-setuid-sandbox"
                | "allow-sandbox-debugging"
        ) {
            return Err("Sandbox-bypass helper switch refused".into());
        }
        if name == "type" {
            let value = match value {
                Some(value) => value.to_owned(),
                None => arguments
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or("Missing helper process type")?,
            };
            if !matches!(value.as_str(), "renderer" | "gpu-process" | "utility")
                || role.replace(value).is_some()
            {
                return Err("Expected exactly one supported helper process type".into());
            }
        }
    }
    if role.is_none() {
        return Err("This executable is only a CEF child process; --type is required".into());
    }
    Ok(())
}

fn arm64_dylib(path: &Path) -> Result<(), String> {
    let mut file = File::open(path).map_err(|_| "Missing framework dylib")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect framework dylib")?;
    if !metadata.is_file() || metadata.len() <= 32 {
        return Err("Expected a non-empty framework dylib".into());
    }
    let mut header = [0_u8; 32];
    file.read_exact(&mut header)
        .map_err(|_| "Cannot read framework dylib header")?;
    let words: Vec<_> = header[..16]
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().expect("four-byte chunk")))
        .collect();
    if words != [0xfeed_facf, 0x0100_000c, 0, 6] {
        return Err("Expected a thin native arm64 Mach-O dylib".into());
    }
    Ok(())
}

/// Permit versioned-framework symlinks, but never a redirected framework root or
/// links outside it. Do not follow directory links while walking (cycles are
/// resolved/rejected by canonicalize); bound work on malformed bundle trees.
fn framework_links(root: &Path) -> Result<(), String> {
    let mut directories = vec![root.to_path_buf()];
    let mut remaining = 16_384_usize;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).map_err(|_| "Cannot inspect framework directory")? {
            remaining = remaining
                .checked_sub(1)
                .ok_or("Framework tree exceeds the preflight limit")?;
            let entry = entry.map_err(|_| "Cannot inspect framework entry")?;
            let kind = entry
                .file_type()
                .map_err(|_| "Cannot inspect framework entry type")?;
            if kind.is_symlink() {
                let target = entry
                    .path()
                    .canonicalize()
                    .map_err(|_| "Broken framework symlink")?;
                if !target.starts_with(root) {
                    return Err("Framework symlink escapes its bundle".into());
                }
            } else if kind.is_dir() {
                directories.push(entry.path());
            } else if !kind.is_file() {
                return Err("Unexpected framework filesystem entry".into());
            }
        }
    }
    Ok(())
}

pub fn bundle(executable: &Path) -> Result<(), String> {
    let macos = executable
        .parent()
        .ok_or("Missing helper executable directory")?;
    let contents = macos.parent().ok_or("Missing helper Contents directory")?;
    let helper = contents.parent().ok_or("Missing helper app")?;
    let frameworks = helper.parent().ok_or("Missing main Frameworks directory")?;
    let main_contents = frameworks
        .parent()
        .ok_or("Missing main Contents directory")?;
    let main_app = main_contents.parent().ok_or("Missing main app")?;
    let name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid helper executable name")?;
    if macos.file_name().is_none_or(|name| name != "MacOS")
        || contents.file_name().is_none_or(|name| name != "Contents")
        || frameworks
            .file_name()
            .is_none_or(|name| name != "Frameworks")
        || main_contents
            .file_name()
            .is_none_or(|name| name != "Contents")
        || main_app.extension().is_none_or(|name| name != "app")
        || !HELPERS.contains(&name)
        || helper.file_name() != Some(OsString::from(format!("{name}.app")).as_os_str())
    {
        return Err("Helper is not in the expected Felinaut CEF app bundle layout".into());
    }
    for path in [
        executable.to_path_buf(),
        contents.join("Info.plist"),
        main_contents.join("Info.plist"),
        main_contents.join("MacOS/felinaut"),
    ] {
        if !path.is_file()
            || path
                .canonicalize()
                .map_err(|_| "Missing bundle component")?
                != path
        {
            return Err("Required bundle file is missing or redirected".into());
        }
    }
    let framework = frameworks.join(FRAMEWORK);
    let canonical = framework
        .canonicalize()
        .map_err(|_| "Missing CEF framework")?;
    if canonical != framework || !framework.is_dir() {
        return Err("Framework root must not redirect outside the bundle".into());
    }
    framework_links(&framework)?;
    for relative in [FRAMEWORK_BINARY, SANDBOX_BINARY] {
        let path = framework
            .join(relative)
            .canonicalize()
            .map_err(|_| "Missing framework component")?;
        if !path.starts_with(&canonical) {
            return Err("Framework component escapes its bundle".into());
        }
        arm64_dylib(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::{ffi::OsStringExt, fs::symlink},
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn validate(values: &[&str]) -> Result<(), String> {
        arguments(values.iter().map(OsString::from))
    }

    #[test]
    fn accepts_only_child_roles_with_normal_chromium_arguments() {
        for role in ["renderer", "gpu-process", "utility"] {
            for prefix in ["--", "-"] {
                assert!(validate(&[&format!("{prefix}type={role}"), "--lang=en-US"]).is_ok());
                assert!(validate(&[&format!("{prefix}type"), role]).is_ok());
            }
        }
        assert!(
            validate(&[
                "--type=utility",
                "--utility-sub-type=network.mojom.NetworkService",
                "--service-sandbox-type=network",
                "--seatbelt-client=42",
            ])
            .is_ok()
        );
    }

    #[test]
    fn rejects_missing_unknown_malformed_and_duplicate_roles() {
        for values in [
            vec![],
            vec!["--type="],
            vec!["--type"],
            vec!["--type", "--no-sandbox"],
            vec!["--type=browser"],
            vec!["--type=zygote"],
            vec!["--type=Renderer"],
            vec!["--type= renderer"],
            vec!["--type=renderer\n"],
            vec!["--type=renderer", "--type=renderer"],
            vec!["--type=renderer", "-type=utility"],
            vec!["-type", "renderer", "--type", "utility"],
            vec!["--", "--type=renderer"],
            vec!["--type=renderer", "--"],
            vec!["--type=renderer", "--lang=\0"],
        ] {
            assert!(validate(&values).is_err(), "{values:?}");
        }
        assert!(arguments([OsString::from_vec(vec![0xff])]).is_err());
        assert!(arguments([OsString::from("--type"), OsString::from_vec(vec![0xff])]).is_err());
        assert!(
            arguments([
                OsString::from("--type=renderer"),
                OsString::from_vec(vec![0xff])
            ])
            .is_err()
        );
    }

    #[test]
    fn rejects_every_sandbox_bypass_including_false_values_and_single_dashes() {
        for name in [
            "no-sandbox",
            "single-process",
            "in-process-gpu",
            "no-zygote",
            "disable-gpu-sandbox",
            "disable-seccomp-filter-sandbox",
            "disable-setuid-sandbox",
            "allow-sandbox-debugging",
        ] {
            for prefix in ["--", "-"] {
                for suffix in ["", "=false", "=0", "=1"] {
                    let switch = format!("{prefix}{name}{suffix}");
                    assert!(validate(&["--type=renderer", &switch]).is_err(), "{switch}");
                    assert!(validate(&[&switch, "--type=renderer"]).is_err(), "{switch}");
                }
            }
        }
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".test-fixtures");
            fs::create_dir_all(&root).unwrap();
            let root = root.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn framework(&self) -> PathBuf {
            self.0
                .join("Felinaut.app/Contents/Frameworks")
                .join(FRAMEWORK)
        }
        fn helper(&self, name: &str) -> PathBuf {
            self.0
                .join("Felinaut.app/Contents/Frameworks")
                .join(format!("{name}.app/Contents/MacOS/{name}"))
        }
        fn write(&self, path: &Path, bytes: &[u8]) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        fn prepare(&self, name: &str) -> PathBuf {
            let exe = self.helper(name);
            self.write(&exe, b"helper");
            self.write(
                &exe.parent().unwrap().parent().unwrap().join("Info.plist"),
                b"plist",
            );
            self.write(&self.0.join("Felinaut.app/Contents/Info.plist"), b"plist");
            self.write(
                &self.0.join("Felinaut.app/Contents/MacOS/felinaut"),
                b"root",
            );
            for relative in [FRAMEWORK_BINARY, SANDBOX_BINARY] {
                self.write(&self.framework().join(relative), &dylib());
            }
            exe
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn dylib() -> Vec<u8> {
        let mut bytes = vec![0; 33];
        for (index, word) in [0xfeed_facf_u32, 0x0100_000c, 0, 6].into_iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn exact_five_helper_bundle_names_and_main_executable_are_required() {
        let fixture = Fixture::new();
        for name in HELPERS {
            assert!(bundle(&fixture.prepare(name)).is_ok(), "{name}");
        }
        for name in [
            "felinaut-helper",
            "felinaut-cef-probe Helper",
            "Felinaut Helper",
            "felinaut Helper (Other)",
        ] {
            assert!(bundle(&fixture.prepare(name)).is_err(), "{name}");
        }
        assert!(bundle(Path::new("/tmp/felinaut-helper")).is_err());
        let exe = fixture.helper(HELPERS[0]);
        let root = fixture.0.join("Felinaut.app/Contents/MacOS/felinaut");
        fs::rename(&root, root.with_file_name("other")).unwrap();
        assert!(bundle(&exe).is_err());
        symlink(root.with_file_name("other"), &root).unwrap();
        assert!(bundle(&exe).is_err());
    }

    #[test]
    fn rejects_mismatched_helper_app_missing_metadata_and_redirected_executable() {
        let fixture = Fixture::new();
        let exe = fixture.prepare(HELPERS[0]);
        let wrong = exe.with_file_name(HELPERS[1]);
        fs::rename(&exe, &wrong).unwrap();
        assert!(bundle(&wrong).is_err());
        fs::rename(&wrong, &exe).unwrap();
        fs::remove_file(fixture.0.join("Felinaut.app/Contents/Info.plist")).unwrap();
        assert!(bundle(&exe).is_err());
        fixture.prepare(HELPERS[0]);
        fs::remove_file(&exe).unwrap();
        let other = fixture.0.join("other-helper");
        fixture.write(&other, b"helper");
        symlink(other, &exe).unwrap();
        assert!(bundle(&exe).is_err());
    }

    #[test]
    fn contained_versioned_framework_links_are_allowed_but_escaping_and_broken_links_are_not() {
        let fixture = Fixture::new();
        let exe = fixture.prepare(HELPERS[0]);
        let framework = fixture.framework();
        let version = framework.join("Versions/A");
        fs::create_dir_all(&version).unwrap();
        for relative in [FRAMEWORK_BINARY, "Libraries"] {
            fs::rename(framework.join(relative), version.join(relative)).unwrap();
            symlink(
                format!("Versions/Current/{relative}"),
                framework.join(relative),
            )
            .unwrap();
        }
        symlink("A", framework.join("Versions/Current")).unwrap();
        assert!(bundle(&exe).is_ok());
        let link = version.join("extra");
        symlink(&fixture.0, &link).unwrap();
        assert!(bundle(&exe).is_err());
        fs::remove_file(&link).unwrap();
        symlink("missing", &link).unwrap();
        assert!(bundle(&exe).is_err());
        fs::remove_file(&link).unwrap();
        symlink("extra", &link).unwrap();
        assert!(bundle(&exe).is_err());
    }

    #[test]
    fn rejects_redirected_framework_root_and_each_escaping_dylib() {
        for relative in [FRAMEWORK_BINARY, SANDBOX_BINARY] {
            let fixture = Fixture::new();
            let exe = fixture.prepare(HELPERS[0]);
            let path = fixture.framework().join(relative);
            let outside = fixture.0.join("outside-dylib");
            fixture.write(&outside, &dylib());
            fs::remove_file(&path).unwrap();
            symlink(outside, path).unwrap();
            assert!(bundle(&exe).is_err());
        }
        let fixture = Fixture::new();
        let exe = fixture.prepare(HELPERS[0]);
        let framework = fixture.framework();
        let outside = fixture.0.join("outside-framework");
        fs::rename(&framework, &outside).unwrap();
        symlink(outside, framework).unwrap();
        assert!(bundle(&exe).is_err());
    }

    #[test]
    fn both_dylibs_must_be_nonempty_thin_arm64_dylibs() {
        let fixture = Fixture::new();
        let exe = fixture.prepare(HELPERS[0]);
        for relative in [FRAMEWORK_BINARY, SANDBOX_BINARY] {
            let path = fixture.framework().join(relative);
            for length in [0, 4, 31, 32] {
                fs::write(&path, &dylib()[..length]).unwrap();
                assert!(bundle(&exe).is_err());
            }
            // Fat, 32-bit, x86_64, arm64e and executable images are not this runtime.
            for (offset, word) in [
                (0, 0xcafe_babe_u32),
                (0, 0xfeed_face),
                (4, 0x0100_0007),
                (8, 2),
                (12, 2),
            ] {
                let mut bytes = dylib();
                bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
                fs::write(&path, bytes).unwrap();
                assert!(bundle(&exe).is_err());
            }
            fs::remove_file(&path).unwrap();
            assert!(bundle(&exe).is_err());
            fs::create_dir(&path).unwrap();
            assert!(bundle(&exe).is_err());
            fs::remove_dir(&path).unwrap();
            fs::write(path, dylib()).unwrap();
            assert!(bundle(&exe).is_ok());
        }
    }
}
