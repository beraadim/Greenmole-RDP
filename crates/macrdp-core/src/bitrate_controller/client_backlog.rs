//! Treat client decode backlog separately from socket and encoder pressure.
//! One large keyframe must not trigger a rate collapse. Require fresh elevated
//! reports in consecutive evaluation windows, and let a draining queue settle.

use std::time::{Duration, Instant};

const MAX_REPORT_AGE: Duration = Duration::from_secs(2);
const MIN_BACKLOG_BYTES: u32 = 64 * 1024;

#[derive(Clone, Copy, Default, Debug)]
pub(super) struct Pressure {
    pub bytes: Option<u32>,
    pub estimated_ms: f64,
    pub elevated: bool,
    pub overloaded: bool,
    pub severe: bool,
}

#[derive(Default)]
pub(super) struct ClientBacklog {
    report: Option<(u32, u32, Instant)>,
    evaluated: Option<(u32, u32)>,
    pressure: Pressure,
}

impl ClientBacklog {
    pub(super) fn observe(&mut self, bytes: Option<u32>, decoded: u32, now: Instant) {
        let Some(bytes) = bytes.filter(|bytes| *bytes != 0) else {
            // Zero means unavailable in MS-RDPEGFX, not an empty queue. Missing
            // reports and explicitly suspended ACKs never count as congestion.
            *self = Self::default();
            return;
        };
        if self
            .report
            .is_some_and(|(old_bytes, old_decoded, _)| (old_bytes, old_decoded) == (bytes, decoded))
        {
            return;
        }
        self.report = Some((bytes, decoded, now));
    }

    pub(super) fn evaluate(&mut self, bitrate_bps: u32, now: Instant) -> Pressure {
        let Some((bytes, decoded, received)) = self.report else {
            return Pressure::default();
        };
        if now.saturating_duration_since(received) > MAX_REPORT_AGE {
            self.evaluated = None;
            self.pressure = Pressure::default();
            // Retain identity so polling the same stale metrics cannot make
            // this report fresh again. Only a new ACK observation can do that.
            return self.pressure;
        }
        if self.evaluated == Some((bytes, decoded)) {
            return self.pressure;
        }

        let milliseconds = |bytes: u32| f64::from(bytes) * 8_000.0 / f64::from(bitrate_bps.max(1));
        let elevated = bytes >= MIN_BACKLOG_BYTES && milliseconds(bytes) >= 150.0;
        let previous_elevated = self.evaluated.is_some_and(|(previous, _)| {
            previous >= MIN_BACKLOG_BYTES && milliseconds(previous) >= 150.0
        });
        let draining = self
            .evaluated
            .is_some_and(|(previous, _)| u64::from(bytes) * 4 < u64::from(previous) * 3);
        let overloaded = elevated && previous_elevated && !draining;
        self.pressure = Pressure {
            bytes: Some(bytes),
            estimated_ms: milliseconds(bytes),
            elevated,
            overloaded,
            severe: overloaded && milliseconds(bytes) >= 500.0,
        };
        self.evaluated = Some((bytes, decoded));
        self.pressure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_burst_and_repeated_metrics_are_not_sustained_pressure() {
        let mut backlog = ClientBacklog::default();
        let now = Instant::now();
        backlog.observe(Some(2_000_000), 1, now);
        let first = backlog.evaluate(10_000_000, now);
        assert!(first.elevated);
        assert!(!first.overloaded);
        for _ in 0..20 {
            backlog.observe(Some(2_000_000), 1, now);
            assert!(!backlog.evaluate(10_000_000, now).overloaded);
        }
        backlog.observe(Some(10_000), 2, now);
        assert!(!backlog.evaluate(10_000_000, now).elevated);
    }

    #[test]
    fn persistent_and_growing_backlog_throttle_but_draining_queue_settles() {
        let mut backlog = ClientBacklog::default();
        let now = Instant::now();
        backlog.observe(Some(1_000_000), 1, now);
        backlog.evaluate(10_000_000, now);
        backlog.observe(Some(1_000_000), 2, now);
        assert!(backlog.evaluate(10_000_000, now).severe);
        backlog.observe(Some(2_000_000), 3, now);
        assert!(backlog.evaluate(10_000_000, now).overloaded);
        backlog.observe(Some(500_000), 4, now);
        let drained = backlog.evaluate(10_000_000, now);
        assert!(drained.elevated);
        assert!(!drained.overloaded);
    }

    #[test]
    fn unavailable_suspended_and_stale_reports_cannot_keep_throttling() {
        for missing in [None, Some(0)] {
            let mut backlog = ClientBacklog::default();
            let now = Instant::now();
            backlog.observe(Some(u32::MAX - 1), 1, now);
            backlog.evaluate(1, now);
            backlog.observe(Some(u32::MAX - 1), 2, now);
            assert!(backlog.evaluate(1, now).severe);
            backlog.observe(missing, 3, now);
            assert!(!backlog.evaluate(1, now).elevated);
            backlog.observe(Some(1_000_000), 4, now);
            backlog.evaluate(10_000_000, now);
            let later = now + Duration::from_secs(3);
            assert!(!backlog.evaluate(10_000_000, later).elevated);
            backlog.observe(Some(1_000_000), 4, later);
            assert!(!backlog.evaluate(10_000_000, later).elevated);
            backlog.observe(Some(1_000_000), 5, later);
            assert!(!backlog.evaluate(10_000_000, later).overloaded);
        }
    }
}
