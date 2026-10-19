//! Pure recovery policy. Native code performs effects only in this order.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LockState {
    Locked,
    Unlocked,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EndReason {
    Normal { force_lock: bool },
    PipeClosed,
    Malformed,
    HeartbeatExpired,
    WorkerExited,
    TapLost,
    DisplayChanged,
    SecureInput,
    GuardStalled,
}

impl EndReason {
    pub(crate) fn failed(self) -> bool {
        !matches!(self, Self::Normal { .. })
    }
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Normal { .. } => 0,
            Self::PipeClosed => 1,
            Self::Malformed => 2,
            Self::HeartbeatExpired => 3,
            Self::WorkerExited => 4,
            Self::TapLost => 5,
            Self::DisplayChanged => 6,
            Self::SecureInput => 7,
            Self::GuardStalled => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    InhibitRemote,
    RequestLock,
    TerminateWorker,
    RetainBarriers,
    ReleaseBarriers,
}

pub(crate) struct Recovery {
    must_lock: bool,
    failed: bool,
    terminated: bool,
    finished: bool,
}

impl Recovery {
    pub(crate) fn begin(initial: LockState, reason: EndReason) -> (Self, Vec<Action>) {
        let must_lock = initial != LockState::Unlocked
            || reason.failed()
            || matches!(reason, EndReason::Normal { force_lock: true });
        let mut actions = vec![Action::InhibitRemote];
        if must_lock {
            actions.push(Action::RequestLock);
        }
        if reason.failed() {
            actions.push(Action::TerminateWorker);
        }
        (
            Self {
                must_lock,
                failed: reason.failed(),
                terminated: reason.failed(),
                finished: false,
            },
            actions,
        )
    }

    pub(crate) fn poll(&mut self, lock_verified: bool) -> Action {
        if self.finished {
            return Action::ReleaseBarriers;
        }
        if self.must_lock && !lock_verified {
            return Action::RetainBarriers;
        }
        self.finished = true;
        Action::ReleaseBarriers
    }

    /// A normal cleanup may itself lose its owner/lease. Escalation must lock
    /// even if the initial session was positively known to be unlocked.
    pub(crate) fn escalate(&mut self) -> Vec<Action> {
        if self.finished || (self.failed && self.terminated) {
            return vec![];
        }
        self.failed = true;
        self.must_lock = true;
        let mut actions = vec![Action::InhibitRemote, Action::RequestLock];
        if !self.terminated {
            self.terminated = true;
            actions.push(Action::TerminateWorker);
        }
        actions
    }

    pub(crate) fn force_lock(&mut self) -> Vec<Action> {
        if self.finished || self.must_lock {
            return vec![];
        }
        self.must_lock = true;
        vec![Action::InhibitRemote, Action::RequestLock]
    }
}

/// Tags identify this worker's injected events, not an authentication boundary.
pub(crate) fn permits_event(
    remote_pid: u32,
    event_tag: i64,
    source_pid: i64,
    source_tag: i64,
    inhibited: bool,
) -> bool {
    !inhibited && event_tag != 0 && source_pid == i64::from(remote_pid) && source_tag == event_tag
}

// Public CoreGraphics key/pointer/tablet events plus system-defined controls.
// The requested mask is all events; these must not have been silently stripped.
pub(crate) const REQUIRED_EVENTS: u64 = (1 << 1)
    | (1 << 2)
    | (1 << 3)
    | (1 << 4)
    | (1 << 5)
    | (1 << 6)
    | (1 << 7)
    | (1 << 10)
    | (1 << 11)
    | (1 << 12)
    | (1 << 14)
    | (1 << 22)
    | (1 << 23)
    | (1 << 24)
    | (1 << 25)
    | (1 << 26)
    | (1 << 27);

pub(crate) fn eligible_tap(
    point: u32,
    options: u32,
    mask: u64,
    process: i32,
    enabled: bool,
    expected_pid: u32,
) -> bool {
    point == 1
        && options == 0
        && enabled
        && process == expected_pid as i32
        && mask & REQUIRED_EVENTS == REQUIRED_EVENTS
}

/// Secure Event Input can bypass a tap which still appears enabled. A known
/// bypass must refuse admission/end an active guarded session.
pub(crate) fn keyboard_barrier_failure(tap_enabled: bool, secure_input: bool) -> Option<EndReason> {
    if secure_input {
        Some(EndReason::SecureInput)
    } else if !tap_enabled {
        Some(EndReason::TapLost)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_disconnect_preserves_only_a_positively_unlocked_session() {
        for initial in [LockState::Locked, LockState::Unlocked, LockState::Unknown] {
            let (mut recovery, actions) =
                Recovery::begin(initial, EndReason::Normal { force_lock: false });
            assert_eq!(actions[0], Action::InhibitRemote);
            assert_eq!(
                actions.contains(&Action::RequestLock),
                initial != LockState::Unlocked
            );
            assert!(!actions.contains(&Action::TerminateWorker));
            if initial != LockState::Unlocked {
                assert_eq!(recovery.poll(false), Action::RetainBarriers);
            }
            assert_eq!(recovery.poll(true), Action::ReleaseBarriers);
        }
    }

    #[test]
    fn emergency_and_every_crash_path_lock_before_releasing_barriers() {
        for reason in [
            EndReason::PipeClosed,
            EndReason::Malformed,
            EndReason::HeartbeatExpired,
            EndReason::WorkerExited,
            EndReason::TapLost,
            EndReason::DisplayChanged,
            EndReason::SecureInput,
            EndReason::GuardStalled,
        ] {
            let (mut recovery, actions) = Recovery::begin(LockState::Unlocked, reason);
            assert_eq!(
                actions,
                [
                    Action::InhibitRemote,
                    Action::RequestLock,
                    Action::TerminateWorker
                ]
            );
            for _ in 0..20 {
                assert_eq!(recovery.poll(false), Action::RetainBarriers);
            }
            assert_eq!(recovery.poll(true), Action::ReleaseBarriers);
        }
        let (mut recovery, actions) =
            Recovery::begin(LockState::Unlocked, EndReason::Normal { force_lock: true });
        assert_eq!(actions, [Action::InhibitRemote, Action::RequestLock]);
        assert_eq!(recovery.poll(false), Action::RetainBarriers);
    }

    #[test]
    fn failed_normal_cleanup_escalates_even_when_originally_unlocked() {
        let (mut recovery, _) =
            Recovery::begin(LockState::Unlocked, EndReason::Normal { force_lock: false });
        assert_eq!(
            recovery.escalate(),
            [
                Action::InhibitRemote,
                Action::RequestLock,
                Action::TerminateWorker
            ]
        );
        assert_eq!(recovery.poll(false), Action::RetainBarriers);
        assert!(!recovery.escalate().contains(&Action::TerminateWorker));
        assert_eq!(recovery.poll(true), Action::ReleaseBarriers);
    }

    #[test]
    fn local_and_unrelated_synthetic_events_are_blocked_and_recovery_blocks_all() {
        assert!(permits_event(21, 1234, 21, 1234, false));
        for (pid, tag, inhibited) in [
            (0, 0, false),
            (21, 0, false),
            (22, 1234, false),
            (21, 1234, true),
        ] {
            assert!(!permits_event(21, 1234, pid, tag, inhibited));
        }
        assert!(!permits_event(21, 0, 21, 0, false));
    }

    #[test]
    fn admission_requires_a_verified_active_session_tap_with_every_required_event() {
        assert!(eligible_tap(1, 0, u64::MAX, 42, true, 42));
        for bit in 0..64 {
            if REQUIRED_EVENTS & (1 << bit) != 0 {
                assert!(!eligible_tap(1, 0, u64::MAX & !(1 << bit), 42, true, 42));
            }
        }
        for (point, options, pid, enabled) in [
            (0, 0, 42, true),
            (1, 1, 42, true),
            (1, 0, 41, true),
            (1, 0, 42, false),
        ] {
            assert!(!eligible_tap(point, options, u64::MAX, pid, enabled, 42));
        }
    }

    #[test]
    fn secure_input_bypass_is_rejected_even_while_tap_remains_enabled() {
        assert_eq!(keyboard_barrier_failure(true, false), None);
        assert_eq!(
            keyboard_barrier_failure(false, false),
            Some(EndReason::TapLost)
        );
        assert_eq!(
            keyboard_barrier_failure(true, true),
            Some(EndReason::SecureInput)
        );
        assert_eq!(
            keyboard_barrier_failure(false, true),
            Some(EndReason::SecureInput)
        );
    }
}
