//! Native macOS host for the pinned GPUI/CEF application contract.
//! The default build needs no SDK. Enable `runtime` to register the SDK protocols.
//! All application/container operations are AppKit-main-thread-only; callbacks
//! enqueue work and must not unwind. CEF objects belong to the calling adapter.

pub mod lifecycle;
pub mod pump;
pub mod test_profile;

use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};

unsafe extern "C" {
    fn felinaut_cef_is_main_thread() -> bool;
    fn felinaut_cef_verify_runtime(path: *const std::ffi::c_char) -> bool;
    fn felinaut_cef_bootstrap_application() -> i32;
    fn felinaut_cef_verify_application() -> bool;
    fn felinaut_cef_verify_protocols() -> bool;
    fn felinaut_cef_close_requested() -> bool;
    fn felinaut_cef_finish_termination() -> bool;
    fn felinaut_cef_defer_cleanup(callback: extern "C" fn()) -> bool;
    fn felinaut_cef_container_create(parent: *mut c_void) -> *mut c_void;
    fn felinaut_cef_container_view(owner: *mut c_void) -> *mut c_void;
    fn felinaut_cef_container_bounds(owner: *mut c_void, x: f64, y: f64, w: f64, h: f64) -> bool;
    fn felinaut_cef_container_visible(owner: *mut c_void, visible: bool) -> bool;
    fn felinaut_cef_container_layout(owner: *mut c_void) -> bool;
    fn felinaut_cef_container_focus_parent(owner: *mut c_void) -> bool;
    fn felinaut_cef_container_detach(owner: *mut c_void) -> bool;
    fn felinaut_cef_container_release(owner: *mut c_void) -> bool;
}

pub fn is_main_thread() -> bool {
    // SAFETY: thread query has no preconditions.
    unsafe { felinaut_cef_is_main_thread() }
}
fn bootstrap_result(code: i32) -> Result<(), String> {
    let message = match code {
        0 => return Ok(()),
        1 => "CEF bootstrap requires the AppKit main thread",
        2 => "NSApplication already exists; refusing to replace it",
        3 => "Pinned GPUIApplication class is not registered",
        4 => "GPUIApplication platform ivar ABI differs from the reviewed pin",
        5 => "CEF application class already exists",
        6 => "Could not allocate CEF application subclass",
        7 => "Could not register CEF application methods",
        8 => "AppKit exception creating CEF application singleton",
        9 => "AppKit did not select the CEF application subclass",
        10 => "Could not register SDK CEF application protocols",
        _ => "Unknown CEF bootstrap failure",
    };
    Err(message.into())
}
pub fn bootstrap() -> Result<(), String> {
    // SAFETY: shim validates main thread and singleton state before mutation.
    bootstrap_result(unsafe { felinaut_cef_bootstrap_application() })
}
pub fn verify_application() -> bool {
    // SAFETY: shim validates thread, class, delegate and inherited platform state.
    unsafe { felinaut_cef_verify_application() }
}
/// Check the already-loaded image's own version symbols, not its filename.
pub fn verify_runtime(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: CString remains live for the synchronous, main-thread-only query.
    unsafe { felinaut_cef_verify_runtime(path.as_ptr()) }
}
pub fn verify_protocols() -> bool {
    // SAFETY: native protocol query checks singleton/thread ownership.
    unsafe { felinaut_cef_verify_protocols() }
}
pub fn close_requested() -> bool {
    // SAFETY: native shim returns false off-main.
    unsafe { felinaut_cef_close_requested() }
}
pub fn finish_termination() -> bool {
    // SAFETY: native barrier allows termination only after a prior request.
    unsafe { felinaut_cef_finish_termination() }
}
/// Queue a process-lifetime, no-unwind callback outside the main serial GCD queue.
/// Caller must ensure any callback state survives until invocation.
pub fn defer_cleanup(callback: extern "C" fn()) -> bool {
    // SAFETY: no borrowed state crosses the ABI; shim validates main ownership.
    unsafe { felinaut_cef_defer_cleanup(callback) }
}

/// One child container; not a singleton. Retains its NSView, parent and NSWindow.
/// The adapter must retain this owner through pending creation and OnBeforeClose.
/// Detaching after DoClose releases only CEF's child views, not these owners.
pub struct Container {
    owner: NonNull<c_void>,
    _main_thread: PhantomData<Rc<()>>,
}
impl Container {
    /// # Safety
    /// `parent` must point to a live AppKit NSView borrowed on its main thread.
    pub unsafe fn new(parent: *mut c_void) -> Result<Self, String> {
        // SAFETY: caller provides a live NSView; shim retains it synchronously.
        let owner = NonNull::new(unsafe { felinaut_cef_container_create(parent) })
            .ok_or("Could not create CEF page container")?;
        Ok(Self {
            owner,
            _main_thread: PhantomData,
        })
    }
    pub fn view(&self) -> *mut c_void {
        // SAFETY: owner is live and !Send/!Sync.
        unsafe { felinaut_cef_container_view(self.owner.as_ptr()) }
    }
    pub fn set_bounds(&self, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
        // SAFETY: shim checks finite bounded coordinates and attachment state.
        check(unsafe { felinaut_cef_container_bounds(self.owner.as_ptr(), x, y, w, h) })
    }
    /// Apply previously requested layout to a child created asynchronously.
    pub fn layout_children(&self) -> Result<(), String> {
        // SAFETY: owned UI-thread container; no callbacks or GPUI reentry.
        check(unsafe { felinaut_cef_container_layout(self.owner.as_ptr()) })
    }
    pub fn set_visible(&self, visible: bool) -> Result<(), String> {
        // SAFETY: owner cannot migrate from its AppKit thread.
        check(unsafe { felinaut_cef_container_visible(self.owner.as_ptr(), visible) })
    }
    pub fn focus_parent(&self) -> Result<(), String> {
        // SAFETY: parent and window remain retained by owner.
        check(unsafe { felinaut_cef_container_focus_parent(self.owner.as_ptr()) })
    }
    /// Call outside the DoClose callback, after it returned true.
    pub fn detach(&self) -> Result<(), String> {
        // SAFETY: idempotent main-thread operation on this owned container only.
        check(unsafe { felinaut_cef_container_detach(self.owner.as_ptr()) })
    }
}
fn check(success: bool) -> Result<(), String> {
    success
        .then_some(())
        .ok_or("CEF native container operation failed".into())
}
impl Drop for Container {
    fn drop(&mut self) {
        // SAFETY: unique native owner; !Send ensures main-thread destruction.
        // Adapter ownership, not Drop, enforces CEF asynchronous-close ordering.
        unsafe {
            felinaut_cef_container_release(self.owner.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_errors_fail_closed() {
        assert!(bootstrap_result(0).is_ok());
        for code in -2..=15 {
            if code != 0 {
                assert!(bootstrap_result(code).is_err());
            }
        }
    }
    #[test]
    fn background_bootstrap_cannot_mutate_appkit() {
        assert!(std::thread::spawn(bootstrap).join().unwrap().is_err());
        assert!(!std::thread::spawn(verify_application).join().unwrap());
        assert!(!std::thread::spawn(finish_termination).join().unwrap());
    }
}
