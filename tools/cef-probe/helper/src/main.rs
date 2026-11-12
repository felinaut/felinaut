#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("The CEF probe helper requires native macOS arm64 validation");

use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

const FRAMEWORK: &str = "Chromium Embedded Framework.framework";
const FRAMEWORK_BINARY: &str = "Chromium Embedded Framework";
const SANDBOX_BINARY: &str = "Libraries/libcef_sandbox.dylib";

fn validate_arguments(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut process_type = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let argument = argument
            .to_str()
            .ok_or("Helper arguments must be valid UTF-8")?;
        if argument == "--" {
            return Err("A helper command line must not terminate switch parsing early".into());
        }
        // Chromium accepts both single- and double-dash switch prefixes on POSIX.
        let switch = argument.split('=').next().unwrap_or_default();
        let switch_name = switch
            .strip_prefix("--")
            .or_else(|| switch.strip_prefix('-'));
        if matches!(
            switch_name,
            Some(
                "no-sandbox"
                    | "single-process"
                    | "in-process-gpu"
                    | "no-zygote"
                    | "disable-gpu-sandbox"
                    | "disable-seccomp-filter-sandbox"
                    | "disable-setuid-sandbox"
                    | "allow-sandbox-debugging"
            )
        ) {
            return Err(format!("Unsafe helper switch refused: {switch}"));
        }
        let value = if let Some(value) = argument.strip_prefix("--type=") {
            Some(value.to_owned())
        } else if argument == "--type" {
            Some(
                arguments
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or("Missing helper process type")?,
            )
        } else {
            None
        };
        if let Some(value) = value
            && (value.is_empty() || value.starts_with('-') || process_type.replace(value).is_some())
        {
            return Err("Expected exactly one non-empty helper process type".into());
        }
    }
    if process_type.is_none() {
        return Err("This executable is only a CEF child process; --type is required".into());
    }
    Ok(())
}

fn arm64_dylib(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| format!("Missing dylib: {error}"))?;
    if !metadata.is_file() || metadata.len() <= 32 {
        return Err("Expected a non-empty framework dylib".into());
    }
    let mut header = [0_u8; 32];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .map_err(|error| format!("Cannot inspect dylib: {error}"))?;
    let words: Vec<_> = header[..16]
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().expect("four-byte chunk")))
        .collect();
    if words != [0xfeed_facf, 0x0100_000c, 0, 6] {
        return Err("Expected a thin arm64 Mach-O dylib".into());
    }
    Ok(())
}

fn preflight(executable: &Path) -> Result<(), String> {
    let macos = executable.parent().ok_or("Missing executable directory")?;
    let contents = macos.parent().ok_or("Missing helper Contents directory")?;
    let helper = contents.parent().ok_or("Missing helper app")?;
    let frameworks = helper.parent().ok_or("Missing main Frameworks directory")?;
    let name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid helper executable name")?;
    let allowed = [
        "felinaut-cef-probe Helper",
        "felinaut-cef-probe Helper (GPU)",
        "felinaut-cef-probe Helper (Renderer)",
        "felinaut-cef-probe Helper (Plugin)",
        "felinaut-cef-probe Helper (Alerts)",
    ];
    if macos.file_name().is_none_or(|name| name != "MacOS")
        || contents.file_name().is_none_or(|name| name != "Contents")
        || frameworks
            .file_name()
            .is_none_or(|name| name != "Frameworks")
        || !allowed.contains(&name)
        || helper.file_name() != Some(OsString::from(format!("{name}.app")).as_os_str())
    {
        return Err("Helper is not in the expected CEF app bundle layout".into());
    }
    let framework = frameworks.join(FRAMEWORK);
    let canonical = framework
        .canonicalize()
        .map_err(|error| format!("Missing CEF framework: {error}"))?;
    if canonical != framework {
        return Err("Framework root must not redirect outside the bundle".into());
    }
    for relative in [FRAMEWORK_BINARY, SANDBOX_BINARY] {
        let path = framework
            .join(relative)
            .canonicalize()
            .map_err(|error| format!("Missing framework component: {error}"))?;
        if !path.starts_with(&canonical) {
            return Err("Framework component escapes its bundle".into());
        }
        arm64_dylib(&path)?;
    }
    Ok(())
}

fn run() -> Result<i32, String> {
    validate_arguments(std::env::args_os().skip(1))?;
    let executable: PathBuf = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("Cannot resolve helper executable: {error}"))?;
    // Only filesystem/header checks precede sandbox startup. In particular, do
    // not use Args::as_cmd_line(), which calls CEF before the framework is loaded.
    preflight(&executable)?;
    let args = cef::args::Args::new();
    let mut sandbox = cef::sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());
    let loader = cef::library_loader::LibraryLoader::new(&executable, true);
    if !loader.load() {
        return Err("CEF helper framework load failed".into());
    }
    if cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0).is_null() {
        return Err("CEF rejected the binding's last supported API version".into());
    }
    let code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    if code < 0 {
        return Err("CEF did not recognize this executable as a child process".into());
    }
    // The loader and sandbox remain alive through execute_process and are
    // dropped in reverse order before main propagates the child exit status.
    Ok(code)
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("CEF probe helper refused startup: {error}");
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::validate_arguments;
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    fn validate(arguments: &[&str]) -> Result<(), String> {
        validate_arguments(arguments.iter().map(|argument| OsString::from(*argument)))
    }

    #[test]
    fn accepts_child_roles_and_ordinary_chromium_arguments() {
        assert!(validate(&["--type=renderer", "--lang=en-US", "--renderer-client-id=2"]).is_ok());
        assert!(validate(&["--type", "gpu-process"]).is_ok());
        assert!(
            validate(&[
                "--type=utility",
                "--utility-sub-type=network.mojom.NetworkService"
            ])
            .is_ok()
        );
    }

    #[test]
    fn rejects_browser_or_invalid_role_arguments() {
        for arguments in [
            vec![],
            vec!["--type="],
            vec!["--type"],
            vec!["--type", "--no-sandbox"],
            vec!["--type=renderer", "--type=utility"],
            vec!["--", "--type=renderer"],
        ] {
            assert!(validate(&arguments).is_err(), "{arguments:?}");
        }
        assert!(validate_arguments([OsString::from_vec(vec![0xff])]).is_err());
    }

    #[test]
    fn rejects_sandbox_bypasses_including_equals_forms() {
        for switch in [
            "--no-sandbox",
            "--single-process",
            "--no-sandbox=false",
            "--single-process=0",
            "-no-sandbox",
            "-single-process=0",
            "--disable-gpu-sandbox",
            "--disable-seccomp-filter-sandbox",
            "--disable-setuid-sandbox",
            "--allow-sandbox-debugging",
            "--in-process-gpu",
            "--no-zygote",
        ] {
            assert!(validate(&["--type=renderer", switch]).is_err(), "{switch}");
        }
    }
}
