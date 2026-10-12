use std::time::Duration;

/// ACK-less clients retain the established repair cadence. A confirmed AVC
/// decoder does not need a new IDR merely because the desktop is unchanged.
pub(super) const CONSERVATIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const ADAPTATION_RESET_IDLE: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ResumeAction {
    pub full_refresh: bool,
    pub reset_adaptation: bool,
}

pub(super) fn resume_action(
    idle_for: Duration,
    conservative_client: bool,
    recovery_required: bool,
) -> ResumeAction {
    ResumeAction {
        full_refresh: recovery_required
            || (conservative_client && idle_for >= CONSERVATIVE_REFRESH_INTERVAL),
        // A short pause is not evidence that network congestion disappeared.
        reset_adaptation: idle_for >= ADAPTATION_RESET_IDLE,
    }
}

pub(super) fn refresh_delay(
    idle_for: Duration,
    since_refresh: Duration,
    conservative_client: bool,
    recovery_required: bool,
) -> Option<Duration> {
    (conservative_client || recovery_required)
        .then(|| CONSERVATIVE_REFRESH_INTERVAL.saturating_sub(idle_for.min(since_refresh)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_pauses_preserve_delta_frames_and_congestion_learning() {
        for conservative in [false, true] {
            assert_eq!(
                resume_action(Duration::from_millis(200), conservative, false),
                ResumeAction {
                    full_refresh: false,
                    reset_adaptation: false
                }
            );
        }
    }

    #[test]
    fn recovery_is_immediate_and_long_idle_retains_compatibility_repair() {
        assert!(resume_action(Duration::ZERO, false, true).full_refresh);
        assert!(resume_action(Duration::from_secs(2), true, false).full_refresh);
        let healthy = resume_action(Duration::from_secs(30), false, false);
        assert!(!healthy.full_refresh);
        assert!(healthy.reset_adaptation);
    }

    #[test]
    fn healthy_idle_is_quiet_but_repairs_are_rate_limited_even_after_failure() {
        let long = Duration::from_secs(100);
        assert_eq!(refresh_delay(long, long, false, false), None);
        assert_eq!(refresh_delay(long, long, false, true), Some(Duration::ZERO));
        assert_eq!(
            refresh_delay(long, Duration::ZERO, true, false),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            refresh_delay(Duration::from_millis(50), long, true, false),
            Some(Duration::from_millis(1950))
        );
    }
}
