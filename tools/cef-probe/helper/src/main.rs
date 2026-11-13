#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("The CEF probe helper requires native macOS arm64 validation");

use cef::*;
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

// Darwin SDK errno.h / sys/fcntl.h. This probe is native macOS arm64 only.
const EPERM: i32 = 1;
const O_NOFOLLOW: i32 = 0x100;
const O_NONBLOCK: i32 = 4;

unsafe extern "C" {
    fn getppid() -> i32;
    fn write(fd: i32, bytes: *const std::ffi::c_void, length: usize) -> isize;
}

fn audit_record(mut message: String) -> bool {
    if message.len() > 1023 || message.contains(['\n', '\r']) {
        return false;
    }
    message.push('\n');
    // SAFETY: this live buffer is borrowed only for the synchronous write. Each
    // bounded record is one OS write: stderr locks cannot serialize other helpers.
    // Short/error writes fail closed as missing/malformed observer evidence.
    unsafe { write(2, message.as_ptr().cast(), message.len()) == message.len() as isize }
}

#[derive(Debug, PartialEq, Eq)]
struct AuditRole {
    role: String,
    subtype: String,
    sandbox: String,
    seatbelt: bool,
}

fn audit_role(arguments: &[OsString]) -> Result<AuditRole, String> {
    validate_arguments(arguments.iter().cloned())?;
    let values: Vec<_> = arguments
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    let switch = |name: &str| -> Option<&str> {
        values.iter().enumerate().find_map(|(i, value)| {
            value.strip_prefix(&format!("--{name}=")).or_else(|| {
                (*value == format!("--{name}"))
                    .then(|| values.get(i + 1).copied())
                    .flatten()
            })
        })
    };
    let role = switch("type").ok_or("Missing helper role")?;
    if !["renderer", "gpu-process", "utility"].contains(&role) {
        return Err("Unobserved helper role is not supported by this probe".into());
    }
    let token = |value: &str| -> Result<String, String> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            Err("Invalid helper audit field".into())
        } else {
            Ok(value.to_owned())
        }
    };
    Ok(AuditRole {
        role: role.to_owned(),
        subtype: token(switch("utility-sub-type").unwrap_or("none"))?,
        sandbox: token(switch("service-sandbox-type").unwrap_or("none"))?,
        seatbelt: switch("seatbelt-client").is_some(),
    })
}

// This callback is native-only. No filesystem operation is exposed to JavaScript.
wrap_app! {
    struct ProbeApp { canary: PathBuf }
    impl App {
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(ProbeRenderer::new(self.canary.clone()))
        }
    }
}
wrap_render_process_handler! {
    struct ProbeRenderer { canary: PathBuf }
    impl RenderProcessHandler {
        fn on_web_kit_initialized(&self) {
            let path = self.canary.clone();
            // Start only after sandbox initialization; no UI thread waits on disk I/O.
            std::thread::spawn(move || {
                use std::os::unix::fs::OpenOptionsExt;
                let result = fs::OpenOptions::new().read(true).custom_flags(O_NOFOLLOW | O_NONBLOCK).open(path);
                let (outcome, error) = match result {
                    Ok(file) => { drop(file); ("allowed", 0) }
                    Err(error) if error.raw_os_error() == Some(EPERM) => ("denied", EPERM),
                    Err(error) => ("error", error.raw_os_error().unwrap_or(-1)),
                };
                audit_record(format!("CEF_RENDERER_CANARY pid={} outcome={outcome} errno={error}", std::process::id()));
            });
        }
    }
}

fn canary_path(executable: &Path, requested: &Path) -> Result<PathBuf, String> {
    let output = executable
        .ancestors()
        .nth(7)
        .ok_or("Missing run directory")?;
    let expected = output.join("sandbox-canary");
    if requested != expected
        || !output
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("cef-probe-run-"))
    {
        return Err("Canary must be the fixed file in this helper's owned run directory".into());
    }
    let info = fs::symlink_metadata(&expected).map_err(|_| "Missing generated canary")?;
    if !info.is_file()
        || info.len() > 128
        || requested.canonicalize().map_err(|_| "Redirected canary")? != requested
    {
        return Err("Invalid generated canary".into());
    }
    Ok(expected)
}

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
    let role = audit_role(&std::env::args_os().skip(1).collect::<Vec<_>>())?;
    let executable: PathBuf = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("Cannot resolve helper executable: {error}"))?;
    // Only filesystem/header checks precede sandbox startup. In particular, do
    // not use Args::as_cmd_line(), which calls CEF before the framework is loaded.
    preflight(&executable)?;
    let canary = canary_path(
        &executable,
        &PathBuf::from(
            std::env::var_os("FELINAUT_CEF_PROBE_CANARY").ok_or("Missing probe canary")?,
        ),
    )?;
    // SAFETY: getppid has no pointer arguments or ownership transfer.
    let parent = unsafe { getppid() };
    if !audit_record(format!(
        "CEF_HELPER_START pid={} ppid={parent} role={} subtype={} sandbox={} seatbelt={}",
        std::process::id(),
        role.role,
        role.subtype,
        role.sandbox,
        u8::from(role.seatbelt)
    )) {
        return Err("Could not emit helper startup evidence".into());
    }
    let args = cef::args::Args::new();
    let mut sandbox = cef::sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());
    if !audit_record(format!(
        "CEF_HELPER_SANDBOX_READY pid={}",
        std::process::id()
    )) {
        return Err("Could not emit helper sandbox phase evidence".into());
    }
    let loader = cef::library_loader::LibraryLoader::new(&executable, true);
    if !loader.load() {
        return Err("CEF helper framework load failed".into());
    }
    if cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0).is_null() {
        return Err("CEF rejected the binding's last supported API version".into());
    }
    let mut app = ProbeApp::new(canary);
    let code = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
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
    use super::{audit_record, audit_role, canary_path, validate_arguments};
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    fn validate(arguments: &[&str]) -> Result<(), String> {
        validate_arguments(arguments.iter().map(|argument| OsString::from(*argument)))
    }

    #[test]
    fn audit_record_child() {
        if std::env::var_os("FELINAUT_AUDIT_RECORD_CHILD").is_none() {
            return;
        }
        for _ in 0..128 {
            assert!(audit_record(format!(
                "CEF_HELPER_SANDBOX_READY pid={}",
                std::process::id()
            )));
        }
    }

    #[test]
    fn concurrent_processes_emit_complete_bounded_audit_records() {
        use std::process::{Command, Stdio};
        assert!(!audit_record("x".repeat(1024)));
        assert!(!audit_record("injected\nrecord".into()));
        assert!(!audit_record("injected\rrecord".into()));
        let path =
            std::env::temp_dir().join(format!("felinaut-audit-records-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut children = Vec::new();
        for _ in 0..4 {
            children.push(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "tests::audit_record_child"])
                    .env("FELINAUT_AUDIT_RECORD_CHILD", "1")
                    .stdout(Stdio::null())
                    .stderr(Stdio::from(file.try_clone().unwrap()))
                    .spawn()
                    .unwrap(),
            );
        }
        let expected: Vec<_> = children
            .iter()
            .map(|child| format!("CEF_HELPER_SANDBOX_READY pid={}", child.id()))
            .collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        drop(file);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 4 * 128);
        for record in expected {
            assert_eq!(text.lines().filter(|line| *line == record).count(), 128);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn audit_fields_are_bounded_and_do_not_log_seatbelt_descriptors() {
        let role = audit_role(
            &[
                "--type=utility",
                "--utility-sub-type=network.mojom.NetworkService",
                "--service-sandbox-type=network",
                "--seatbelt-client=42",
            ]
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(role.role, "utility");
        assert_eq!(role.subtype, "network.mojom.NetworkService");
        assert_eq!(role.sandbox, "network");
        assert!(role.seatbelt);
        let role = audit_role(&[OsString::from("--type=renderer")]).unwrap();
        assert_eq!(role.subtype, "none");
        assert!(!role.seatbelt);
        for arguments in [
            vec!["--type=other".to_owned()],
            vec![
                "--type=utility".to_owned(),
                "--utility-sub-type=private value".to_owned(),
            ],
            vec![
                "--type=utility".to_owned(),
                format!("--service-sandbox-type={}", "x".repeat(129)),
            ],
        ] {
            assert!(
                audit_role(
                    &arguments
                        .into_iter()
                        .map(OsString::from)
                        .collect::<Vec<_>>()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn canary_path_never_accepts_arbitrary_files_or_missing_inputs() {
        let exe = std::path::Path::new(
            "/tmp/cef-probe-run-unit/probe.app/Contents/Frameworks/felinaut-cef-probe Helper.app/Contents/MacOS/felinaut-cef-probe Helper",
        );
        assert!(canary_path(exe, std::path::Path::new("/etc/passwd")).is_err());
        assert!(
            canary_path(
                exe,
                std::path::Path::new("/tmp/cef-probe-run-unit/sandbox-canary")
            )
            .is_err()
        );
    }

    #[test]
    fn generated_canary_accepts_only_contained_regular_files_within_size_bound() {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("cef-probe-run-helper-unit-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let exe = root.join("probe.app/Contents/Frameworks/felinaut-cef-probe Helper.app/Contents/MacOS/felinaut-cef-probe Helper");
        let canary = root.join("sandbox-canary");
        std::fs::write(&canary, [0; 128]).unwrap();
        assert_eq!(canary_path(&exe, &canary).unwrap(), canary);
        std::fs::write(&canary, [0; 129]).unwrap();
        assert!(canary_path(&exe, &canary).is_err());
        std::fs::remove_file(&canary).unwrap();
        let real = root.join("real-canary");
        std::fs::write(&real, b"fixture").unwrap();
        std::os::unix::fs::symlink(&real, &canary).unwrap();
        assert!(canary_path(&exe, &canary).is_err());
        std::fs::remove_file(&canary).unwrap();
        std::fs::remove_file(&real).unwrap();
        std::fs::remove_dir(root).unwrap();
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
