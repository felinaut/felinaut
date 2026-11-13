//! CEF child entry point. No browser initialization, UI toolkit or runtime acquisition.

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("The production CEF helper currently requires native macOS arm64");

// Keep the audited native-only transport shared with the probe until it can be
// relocated in a coordinated change. Neither module depends on the probe host.
#[path = "../../cef-probe/helper/src/evaluation.rs"]
mod evaluation;
#[path = "../../../src/page/cef/protocol.rs"]
mod evaluation_protocol;
mod preflight;

use cef::*;

wrap_app! {
    struct HelperApp { evaluation: evaluation::Bridge }
    impl App {
        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            evaluation::contain(|| Some(HelperRenderer::new(self.evaluation.clone())))
        }
    }
}

wrap_render_process_handler! {
    struct HelperRenderer { evaluation: evaluation::Bridge }
    impl RenderProcessHandler {
        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            self.evaluation.context_created(browser, frame, context);
        }
        fn on_context_released(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            self.evaluation.context_released(frame, context);
        }
        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            self.evaluation.browser_destroyed(browser);
        }
        fn on_process_message_received(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, source_process: ProcessId, message: Option<&mut ProcessMessage>) -> i32 {
            self.evaluation.on_message(browser, frame, source_process, message)
        }
    }
}

fn run() -> Result<i32, String> {
    preflight::arguments(std::env::args_os().skip(1))?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("Cannot resolve helper executable: {error}"))?;
    // Only Rust argument, filesystem and Mach-O header checks precede sandbox
    // initialization. Args::as_cmd_line() would call CEF too early; do not use it.
    preflight::bundle(&executable)?;
    let args = cef::args::Args::new();
    let mut sandbox = cef::sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());

    let loader = cef::library_loader::LibraryLoader::new(&executable, true);
    if !loader.load() {
        return Err("Could not load the bundled CEF framework".into());
    }
    let hash = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let commit = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 2);
    if hash.is_null() || commit.is_null() {
        return Err("CEF rejected the pinned binding/API version".into());
    }
    // SAFETY: a successful cef_api_hash returns a library-owned NUL-terminated
    // string. The loader remains alive for this read and all renderer callbacks.
    let commit = unsafe { std::ffi::CStr::from_ptr(commit) };
    if commit.to_bytes() != b"8ea0c73b5dbed1389c8dafb911a98c41246bb8cb" {
        return Err("Bundled CEF build does not match the root's pinned runtime".into());
    }
    let mut app = HelperApp::new(evaluation::Bridge::default());
    let code = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if code < 0 {
        return Err("CEF did not recognize this executable as a child process".into());
    }
    // Drop app/V8 state before unloading CEF, and unload before destroying the
    // sandbox. Normal returns also preserve CEF's child exit status.
    Ok(code)
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Felinaut CEF helper refused startup: {error}");
            1
        }
    };
    std::process::exit(code);
}
