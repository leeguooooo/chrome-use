//! Out-of-band revocation for a single native key attempt. Revocation never
//! waits for AX inspection or the native operation's async mutex.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

const AVAILABLE: u8 = 0;
const REVOKED: u8 = 1;
const DISPATCHING: u8 = 2;
const COMPLETE: u8 = 3;

pub(super) struct DispatchFence {
    state: AtomicU8,
    deadline: Mutex<Instant>,
}

impl DispatchFence {
    pub(super) fn new(deadline: Instant) -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(AVAILABLE),
            deadline: Mutex::new(deadline),
        })
    }
    pub(super) fn available(&self) -> bool {
        self.state.load(Ordering::SeqCst) == AVAILABLE
            && Instant::now()
                < *self
                    .deadline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(super) fn renew(&self, deadline: Instant) -> bool {
        let mut current = self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.state.load(Ordering::SeqCst) != AVAILABLE || Instant::now() >= *current {
            return false;
        }
        *current = deadline;
        true
    }
    /// True means revocation won before dispatch entered. False does not
    /// retract an API call already begun, or claim that a key was delivered.
    pub(super) fn revoke(&self) -> bool {
        self.state
            .compare_exchange(AVAILABLE, REVOKED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    pub(super) fn expire(&self) {
        if Instant::now()
            >= *self
                .deadline
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            self.revoke();
        }
    }
    pub(super) fn begin(self: &Arc<Self>) -> Result<DispatchGuard, &'static str> {
        if !self.available() {
            self.revoke();
            return Err("native input authority was revoked or expired");
        }
        self.state
            .compare_exchange(AVAILABLE, DISPATCHING, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "native input authority was already revoked or consumed")?;
        Ok(DispatchGuard(self.clone()))
    }
}

pub(super) struct DispatchGuard(Arc<DispatchFence>);
impl Drop for DispatchGuard {
    fn drop(&mut self) {
        self.0.state.store(COMPLETE, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn revoked_authority_cannot_dispatch_or_be_renewed() {
        let fence = DispatchFence::new(Instant::now() + Duration::from_secs(1));
        assert!(fence.revoke());
        assert!(fence.begin().is_err());
        assert!(!fence.renew(Instant::now() + Duration::from_secs(2)));
    }
    #[test]
    fn dispatch_is_consumed_once_even_when_cancellation_arrives_later() {
        let fence = DispatchFence::new(Instant::now() + Duration::from_secs(1));
        let dispatch = fence.begin().unwrap();
        assert!(!fence.revoke());
        assert!(fence.begin().is_err());
        drop(dispatch);
        assert!(fence.begin().is_err());
    }
    #[test]
    fn an_expired_preparation_cannot_gain_a_fresh_arm() {
        let fence = DispatchFence::new(Instant::now());
        assert!(!fence.renew(Instant::now() + Duration::from_secs(2)));
        assert!(fence.begin().is_err());
    }
}
