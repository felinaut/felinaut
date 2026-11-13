//! Opt-in CEF/GPUI native harness; the default configuration remains SDK-free.

pub mod pump;

#[cfg(feature = "runtime")]
#[path = "../../../src/audio.rs"]
pub mod audio;
#[cfg(feature = "runtime")]
#[path = "../../../src/page/cef.rs"]
pub mod cef_page;
#[cfg(feature = "runtime")]
#[path = "../../../src/load_error.rs"]
pub mod load_error;
#[cfg(feature = "runtime")]
#[path = "../../../src/page/api.rs"]
pub mod page;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Lifecycle,
    ExistingApplication,
    Pump,
    Cef,
    Coexistence,
}

impl Mode {
    pub fn parse(arguments: &[String]) -> Result<Self, &'static str> {
        match arguments {
            [argument] if argument == "--run-lifecycle" => Ok(Self::Lifecycle),
            [argument] if argument == "--check-existing-app" => Ok(Self::ExistingApplication),
            [argument] if argument == "--check-pump" => Ok(Self::Pump),
            [argument] if argument == "--run-cef" => Ok(Self::Cef),
            [argument] if argument == "--run-coexistence" => Ok(Self::Coexistence),
            _ => Err(
                "Usage: felinaut-cef-probe --run-lifecycle | --check-existing-app | --check-pump | --run-cef | --run-coexistence",
            ),
        }
    }
}

unsafe extern "C" {
    fn felinaut_cef_bootstrap_application() -> i32;
    fn felinaut_cef_verify_application() -> bool;
    fn felinaut_cef_verify_event_scope() -> bool;
    fn felinaut_cef_close_requested() -> bool;
    fn felinaut_cef_finish_termination() -> bool;
    fn felinaut_cef_create_conflicting_application() -> bool;
}

fn bootstrap_result(code: i32) -> Result<(), &'static str> {
    match code {
        0 => Ok(()),
        1 => Err("Application bootstrap must run on the AppKit main thread"),
        2 => Err("An NSApplication singleton already exists; refusing to replace it"),
        3 => Err("Pinned GPUIApplication class is not registered"),
        4 => Err("GPUIApplication platform ivar ABI differs from the reviewed pin"),
        5 => Err("Probe application class already exists"),
        6 => Err("Could not allocate the application subclass"),
        7 => Err("Could not register application subclass methods"),
        8 => Err("AppKit raised an exception creating the application singleton"),
        9 => Err("AppKit did not select the requested application subclass"),
        10 => Err("Could not register CEF application protocol conformance"),
        _ => Err("Unknown native bootstrap failure"),
    }
}

pub fn bootstrap() -> Result<(), &'static str> {
    // SAFETY: Native code checks main-thread ownership and existing singleton state
    // before touching AppKit; no borrowed pointers or Rust callbacks cross the ABI.
    bootstrap_result(unsafe { felinaut_cef_bootstrap_application() })
}

pub fn verify_application() -> bool {
    // SAFETY: The shim checks thread/class identity before inspecting live inherited state.
    unsafe { felinaut_cef_verify_application() }
}

pub fn verify_event_scope() -> bool {
    // SAFETY: Native-only nested/exception dispatch runs only on the owning UI thread.
    unsafe { felinaut_cef_verify_event_scope() }
}

pub fn close_requested() -> bool {
    // SAFETY: Native state is read only on its main thread; other threads get false.
    unsafe { felinaut_cef_close_requested() }
}

pub fn finish_termination() -> bool {
    // SAFETY: Schedules normal termination only after the owned singleton requested it.
    unsafe { felinaut_cef_finish_termination() }
}

pub fn create_conflicting_application() -> bool {
    // SAFETY: Deliberately creates a plain singleton in the separate negative-test process.
    unsafe { felinaut_cef_create_conflicting_application() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_window_modes_require_explicit_opt_in() {
        for (argument, expected) in [
            ("--run-lifecycle", Mode::Lifecycle),
            ("--check-existing-app", Mode::ExistingApplication),
            ("--check-pump", Mode::Pump),
            ("--run-cef", Mode::Cef),
            ("--run-coexistence", Mode::Coexistence),
        ] {
            assert_eq!(Mode::parse(&[argument.to_owned()]), Ok(expected));
        }
        for arguments in [
            vec![],
            vec![""],
            vec!["--run"],
            vec!["--run-lifecycle", "extra"],
        ] {
            let arguments: Vec<_> = arguments.into_iter().map(str::to_owned).collect();
            assert!(Mode::parse(&arguments).is_err());
        }
    }

    #[test]
    fn bootstrap_errors_never_become_success_or_silent_replacement() {
        assert!(bootstrap_result(0).is_ok());
        for code in -2..=15 {
            if code != 0 {
                assert!(bootstrap_result(code).is_err());
            }
        }
        assert!(
            bootstrap_result(2)
                .unwrap_err()
                .contains("refusing to replace")
        );
    }

    #[test]
    fn native_bootstrap_refuses_background_threads_before_mutating_appkit() {
        let result = std::thread::spawn(bootstrap).join().unwrap();
        assert_eq!(result, bootstrap_result(1));
        assert!(!std::thread::spawn(verify_application).join().unwrap());
        assert!(!std::thread::spawn(finish_termination).join().unwrap());
    }
}
