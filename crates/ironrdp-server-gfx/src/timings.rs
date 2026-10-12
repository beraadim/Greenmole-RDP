//! Bounded event-level timing windows. Sampling the last value once a second
//! misses transient stalls; these windows include every observed stage event.

use std::collections::VecDeque;

const TIMING_WINDOW_SAMPLES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimingSummary {
    pub samples: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

#[derive(Debug, Default)]
pub struct TimingWindow {
    values: VecDeque<f64>,
}

impl TimingWindow {
    pub fn record(&mut self, ms: f64) {
        if !ms.is_finite() || ms < 0.0 {
            return;
        }
        if self.values.len() == TIMING_WINDOW_SAMPLES {
            self.values.pop_front();
        }
        self.values.push_back(ms);
    }

    pub fn summary(&self) -> TimingSummary {
        if self.values.is_empty() {
            return TimingSummary::default();
        }
        let mut sorted: Vec<f64> = self.values.iter().copied().collect();
        sorted.sort_unstable_by(f64::total_cmp);
        let rank = |percent: usize| Some(sorted[(sorted.len() * percent).div_ceil(100) - 1]);
        TimingSummary {
            samples: sorted.len(),
            p50_ms: rank(50),
            p95_ms: rank(95),
            p99_ms: rank(99),
            max_ms: sorted.last().copied(),
        }
    }
}

#[derive(Debug, Default)]
pub struct StageTimings {
    pub capture: TimingWindow,
    pub encode: TimingWindow,
    pub completion: TimingWindow,
    pub write: TimingWindow,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_brief_stalls_and_distinguishes_missing_from_zero() {
        let mut timings = TimingWindow::default();
        assert_eq!(timings.summary(), TimingSummary::default());
        for value in [f64::NAN, f64::INFINITY, -1.0] {
            timings.record(value);
        }
        assert_eq!(timings.summary().samples, 0);
        for _ in 0..95 {
            timings.record(0.0);
        }
        for _ in 0..4 {
            timings.record(40.0);
        }
        timings.record(250.0);
        let summary = timings.summary();
        assert_eq!(summary.samples, 100);
        assert_eq!(summary.p50_ms, Some(0.0));
        assert_eq!(summary.p95_ms, Some(0.0));
        assert_eq!(summary.p99_ms, Some(40.0));
        assert_eq!(summary.max_ms, Some(250.0));
        assert_eq!(
            timings.summary(),
            summary,
            "reading must not consume samples"
        );
    }

    #[test]
    fn bounded_window_expires_old_samples_and_new_session_starts_empty() {
        let mut stages = StageTimings::default();
        stages.write.record(9999.0);
        for _ in 0..TIMING_WINDOW_SAMPLES {
            stages.write.record(2.0);
        }
        assert_eq!(stages.write.summary().samples, TIMING_WINDOW_SAMPLES);
        assert_eq!(stages.write.summary().max_ms, Some(2.0));
        assert_eq!(stages.encode.summary().samples, 0);
        stages = StageTimings::default();
        assert_eq!(stages.write.summary().samples, 0);
    }
}
