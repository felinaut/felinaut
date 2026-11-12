//! Native, main-run-loop scheduling for CEF's external message pump.
//!
//! Start before CEF can issue scheduling notifications, but do not run the native
//! loop until CEF initialization has succeeded. Call [`schedule`] from
//! `OnScheduleMessagePumpWork` on any thread; never call CEF's pump there or from
//! GPUI rendering. The native scheduler invokes the callback asynchronously on
//! the main thread, with coalescing and a nested-loop reentrancy guard.

/// A process-lifetime function pointer, not a borrowed closure or CEF object.
///
/// The callback must not unwind or throw through the native timer callback.
/// Supply a trampoline that catches Rust panics internally and reports failure
/// to the owner. `extern "C"` alone aborts on an escaping panic; it does not catch
/// one. Keep callback code loaded until all in-flight calls have returned.
/// CEF initialization/shutdown and callback-owned state remain the host's concern.
pub type PumpCallback = extern "C" fn();

unsafe extern "C" {
    fn felinaut_cef_pump_start(callback: PumpCallback) -> bool;
    fn felinaut_cef_pump_schedule(delay_ms: i64);
    fn felinaut_cef_pump_stop() -> bool;
    fn felinaut_cef_pump_self_test() -> bool;
}

/// Start an idle scheduler on the AppKit main thread.
///
/// Returns false off-main, if already started, while a pump callback is on the
/// stack (even if it called [`stop`]), or if native setup fails. Starting does not
/// call the callback or invent an initial CEF deadline. See [`PumpCallback`] for
/// the callback's no-unwind and lifetime contract.
pub fn start(callback: PumpCallback) -> bool {
    // SAFETY: The shim checks main-thread/session ownership. The ABI accepts an
    // owned function pointer with no borrowed context and never invokes it inline.
    unsafe { felinaut_cef_pump_start(callback) }
}

/// Replace the outstanding deadline from any thread; do not pump synchronously.
///
/// Latest means mutex-serialized arrival order, not the minimum requested delay.
/// Nonpositive delays request prompt asynchronous work. Positive delays saturate
/// at 24 hours and are measured against a monotonic clock. Notifications while
/// stopped are ignored, not replayed on a future start. The host must quiesce old
/// CEF producers before restarting: the C ABI carries no producer/session token.
pub fn schedule(delay_ms: i64) {
    // SAFETY: Native process-lifetime state is synchronized, including source
    // invalidation racing a worker notification; the integer is untrusted/bounded.
    unsafe { felinaut_cef_pump_schedule(delay_ms) }
}

/// Invalidate outstanding deadlines, timers and queued work on the main thread.
///
/// Idempotent on-main; returns false off-main without changing state. Call before
/// CEF shutdown. If called inside the pump callback, that current call is allowed
/// to return, but cannot reschedule another invocation. Do not destroy callback
/// state or shut down CEF until that in-flight call has returned. A later start
/// uses a new generation, so old queued native work cannot invoke its callback.
pub fn stop() -> bool {
    // SAFETY: The shim verifies the owning thread and clears callback ownership
    // before invalidating/releasing native run-loop resources.
    unsafe { felinaut_cef_pump_stop() }
}

/// Run bounded native behavioral tests only in explicit fresh-process check mode.
///
/// Must run on-main before creating NSApplication/GPUI or starting CEF. Refuses an
/// existing application, active scheduler or nested check. Temporarily drives the
/// native loop (including event-tracking mode), then stops and drains stale work.
/// The normal native-loop budget is 1.5 seconds; this is not an engine/SDK test.
pub fn self_test() -> bool {
    // SAFETY: Native tests use only process-lifetime callbacks/state, guard their
    // entry conditions and leave the scheduler stopped on both success and failure.
    unsafe { felinaut_cef_pump_self_test() }
}
