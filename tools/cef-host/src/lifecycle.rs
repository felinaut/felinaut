//! Pure asynchronous page-close ownership gate. Never release an accepted
//! creation merely because its caller disappeared or a deadline elapsed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    accepted: bool,
    created: bool,
    requested: bool,
    dispatched: bool,
    close_ready: bool,
    detached: bool,
    closed: bool,
}
impl Lifecycle {
    pub fn accepted(&mut self) {
        self.accepted = true;
    }
    pub fn created(&mut self) {
        self.created = true;
    }
    pub fn request_close(&mut self) {
        self.requested = true;
    }
    pub fn closing(&self) -> bool {
        self.requested || self.close_ready || self.closed
    }
    pub fn should_dispatch(&self) -> bool {
        self.created && self.requested && !self.dispatched && !self.close_ready && !self.closed
    }
    pub fn dispatched(&mut self) {
        self.dispatched = true;
    }
    pub fn close_ready(&mut self) {
        self.requested = true;
        self.close_ready = true;
    }
    pub fn should_detach(&self) -> bool {
        self.close_ready && !self.detached && !self.closed
    }
    pub fn detached(&mut self) {
        self.detached = true;
    }
    pub fn closed(&mut self) {
        self.closed = true;
    }
    pub fn releasable(&self) -> bool {
        !self.accepted || self.closed
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drop_and_quit_before_creation_keep_native_owners_until_before_close() {
        let mut gate = Lifecycle::default();
        gate.accepted();
        gate.request_close();
        gate.request_close();
        assert!(!gate.releasable());
        assert!(!gate.should_dispatch());
        gate.created();
        assert!(gate.should_dispatch());
        assert!(gate.should_dispatch()); // absent host permits retry
        gate.dispatched();
        assert!(!gate.should_dispatch());
        assert!(!gate.releasable());
        gate.close_ready();
        assert!(gate.should_detach());
        gate.detached();
        assert!(!gate.should_detach());
        assert!(!gate.releasable());
        gate.closed();
        assert!(gate.releasable());
    }
    #[test]
    fn refused_dispatch_can_release_without_a_callback() {
        let gate = Lifecycle::default();
        assert!(gate.releasable());
        assert!(!gate.closing());
        assert!(!gate.should_dispatch());
    }

    #[test]
    fn native_close_and_duplicate_callbacks_are_idempotent() {
        let mut gate = Lifecycle::default();
        gate.accepted();
        gate.created();
        assert!(!gate.should_dispatch());
        gate.close_ready();
        gate.close_ready();
        assert!(gate.closing());
        assert!(!gate.should_dispatch());
        assert!(gate.should_detach());
        gate.detached();
        gate.close_ready();
        assert!(!gate.should_detach());
        gate.closed();
        gate.closed();
        gate.request_close();
        gate.created();
        assert!(gate.releasable());
        assert!(!gate.should_dispatch());
        assert!(!gate.should_detach());
    }
}
