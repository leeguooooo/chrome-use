//! Bounded profile reopening decisions, separate from process and browser I/O.
use std::time::Duration;

const GRACE: Duration = Duration::from_secs(20);
const DEADLINE: Duration = Duration::from_secs(45);

#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    Wait,
    Open,
    Closed { requested: bool },
    Done,
}

enum Phase {
    Observing,
    Requested { at: Duration, accepted: bool },
    Finished,
}

pub(crate) struct Recovery {
    phase: Phase,
    candidate: Option<(String, String, Duration)>,
}

impl Recovery {
    pub(crate) fn new(check_closed: bool) -> Self {
        Self {
            phase: if check_closed {
                Phase::Observing
            } else {
                Phase::Finished
            },
            candidate: None,
        }
    }

    pub(crate) fn needs_profile(&self, now: Duration) -> bool {
        if now >= DEADLINE {
            return false;
        }
        match self.phase {
            Phase::Observing => true,
            Phase::Requested { at, .. } => now.saturating_sub(at) >= GRACE,
            Phase::Finished => false,
        }
    }

    /// `None` means active or unknown. Neither may inherit a closed timer.
    /// Relay readiness always wins; no state turns elapsed time into success.
    pub(crate) fn observe(
        &mut self,
        now: Duration,
        relay_ready: bool,
        closed: Option<(&str, &str)>,
    ) -> Decision {
        if relay_ready || now >= DEADLINE {
            return Decision::Done;
        }
        match self.phase {
            Phase::Observing => {
                let Some((root, dir)) = closed else {
                    self.candidate = None;
                    return Decision::Wait;
                };
                match &self.candidate {
                    Some((old_root, old_dir, since)) if old_root == root && old_dir == dir => {
                        if now.saturating_sub(*since) >= GRACE {
                            return Decision::Open;
                        }
                    }
                    _ => self.candidate = Some((root.into(), dir.into(), now)),
                }
            }
            Phase::Requested { at, accepted } if now.saturating_sub(at) >= GRACE => {
                self.phase = Phase::Finished;
                let same_profile = match (closed, self.candidate.as_ref()) {
                    (Some((root, dir)), Some((old_root, old_dir, _))) => {
                        root == old_root && dir == old_dir
                    }
                    _ => false,
                };
                if same_profile {
                    return Decision::Closed {
                        requested: accepted,
                    };
                }
            }
            _ => {}
        }
        Decision::Wait
    }

    /// Even a failed launch consumes the one attempt. Never loop opening windows.
    pub(crate) fn requested(&mut self, now: Duration, accepted: bool) {
        self.phase = Phase::Requested { at: now, accepted };
        // Retain the exact requested identity for the final diagnosis. A new
        // default profile must not inherit another profile's window request.
    }
}

#[cfg(test)]
mod tests {
    use super::{Decision, Recovery};
    use std::time::Duration;
    const CLOSED: Option<(&str, &str)> = Some(("/fixture", "Default"));
    fn seconds(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn recovery_hardening_late_profile_write_cancels_open_before_grace() {
        let mut r = Recovery::new(true);
        assert_eq!(r.observe(seconds(0), false, CLOSED), Decision::Wait);
        assert_eq!(r.observe(seconds(19), false, CLOSED), Decision::Wait);
        assert_eq!(r.observe(seconds(20), false, None), Decision::Wait);
        assert_eq!(r.observe(seconds(21), false, CLOSED), Decision::Wait);
        assert_eq!(r.observe(seconds(40), false, CLOSED), Decision::Wait);
        assert_eq!(r.observe(seconds(41), false, CLOSED), Decision::Open);
    }

    #[test]
    fn recovery_hardening_changed_profile_cannot_reuse_the_old_timer() {
        let mut r = Recovery::new(true);
        r.observe(seconds(0), false, CLOSED);
        let other = Some(("/fixture", "Profile 1"));
        assert_eq!(r.observe(seconds(20), false, other), Decision::Wait);
        assert_eq!(r.observe(seconds(39), false, other), Decision::Wait);
        assert_eq!(r.observe(seconds(40), false, other), Decision::Open);
    }

    #[test]
    fn recovery_hardening_relay_readiness_wins_before_and_after_request() {
        let mut r = Recovery::new(true);
        r.observe(seconds(0), false, CLOSED);
        assert_eq!(r.observe(seconds(20), true, CLOSED), Decision::Done);
        r.requested(seconds(20), true);
        assert_eq!(r.observe(seconds(21), true, CLOSED), Decision::Done);
        assert_eq!(r.observe(seconds(45), true, CLOSED), Decision::Done);
    }

    #[test]
    fn recovery_hardening_request_has_its_own_grace_and_failure_never_retries() {
        for accepted in [false, true] {
            let mut r = Recovery::new(true);
            r.observe(seconds(0), false, CLOSED);
            assert_eq!(r.observe(seconds(20), false, CLOSED), Decision::Open);
            r.requested(seconds(23), accepted);
            assert!(!r.needs_profile(seconds(42)));
            assert_eq!(r.observe(seconds(42), false, CLOSED), Decision::Wait);
            assert!(r.needs_profile(seconds(43)));
            assert_eq!(
                r.observe(seconds(43), false, CLOSED),
                Decision::Closed {
                    requested: accepted
                }
            );
            assert!(!r.needs_profile(seconds(44)));
            assert_eq!(r.observe(seconds(44), false, CLOSED), Decision::Wait);
            assert_eq!(r.observe(seconds(45), false, CLOSED), Decision::Done);
        }
    }

    #[test]
    fn recovery_hardening_unknown_after_request_does_not_request_another_window() {
        let mut r = Recovery::new(true);
        r.requested(seconds(20), true);
        assert_eq!(r.observe(seconds(40), false, None), Decision::Wait);
        assert_eq!(r.observe(seconds(41), false, CLOSED), Decision::Wait);
        assert_eq!(r.observe(seconds(45), false, CLOSED), Decision::Done);
    }

    #[test]
    fn recovery_hardening_changed_profile_after_request_is_not_misattributed() {
        let mut r = Recovery::new(true);
        r.observe(seconds(0), false, CLOSED);
        assert_eq!(r.observe(seconds(20), false, CLOSED), Decision::Open);
        r.requested(seconds(20), true);
        let other = Some(("/fixture", "Profile 1"));
        assert_eq!(r.observe(seconds(40), false, other), Decision::Wait);
        assert_eq!(r.observe(seconds(41), false, CLOSED), Decision::Wait);
    }

    #[test]
    fn recovery_hardening_slow_final_probe_cannot_open_after_deadline() {
        let mut r = Recovery::new(true);
        r.observe(seconds(0), false, CLOSED);
        assert_eq!(r.observe(seconds(20), false, CLOSED), Decision::Open);
        // The caller re-evaluates after its final relay I/O, before launching.
        assert_eq!(r.observe(seconds(45), false, CLOSED), Decision::Done);
    }

    #[test]
    fn recovery_hardening_cold_start_and_total_deadline_never_open() {
        let mut r = Recovery::new(false);
        assert!(!r.needs_profile(seconds(0)));
        assert_eq!(r.observe(seconds(20), false, CLOSED), Decision::Wait);
        assert_eq!(r.observe(seconds(45), false, CLOSED), Decision::Done);
        let mut r = Recovery::new(true);
        r.observe(seconds(24), false, CLOSED);
        assert_eq!(r.observe(seconds(44), false, CLOSED), Decision::Open);
        r.requested(seconds(44), true);
        assert_eq!(r.observe(seconds(45), false, CLOSED), Decision::Done);
    }
}
