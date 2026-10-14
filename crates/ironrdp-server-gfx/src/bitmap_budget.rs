//! Choose a latency-oriented bitmap partition budget below the client's hard limit.
//!
//! Successful socket writes measure service cost, not end-to-end link capacity.
//! Kernel buffering can conceal congestion, so even the fast-path budget is bounded.
use std::time::{Duration, Instant};

const MIN_BUDGET: usize = 16 * 1024;
const INITIAL_BUDGET: usize = 128 * 1024;
const MAX_BUDGET: usize = 512 * 1024;
const TARGET_WRITE_MS: f64 = 50.0;
const RECOVERY_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) struct BitmapBudget {
    budget: usize,
    bytes_per_ms: Option<f64>,
    last_adjusted: Option<Instant>,
}

impl Default for BitmapBudget {
    fn default() -> Self {
        Self {
            budget: INITIAL_BUDGET,
            bytes_per_ms: None,
            last_adjusted: None,
        }
    }
}

impl BitmapBudget {
    pub(crate) fn bytes(&self) -> usize {
        self.budget
    }

    /// Observe a completed bitmap dispatch once, never a polling snapshot.
    pub(crate) fn observe(&mut self, bytes: usize, write_ms: f64, now: Instant) {
        // Tiny/instantaneous writes mostly measure syscall/kernel-buffer cost.
        // They provide too little information to justify raising the budget.
        if bytes < 1024 || !write_ms.is_finite() || write_ms < 0.25 {
            return;
        }
        let sample = bytes as f64 / write_ms;
        let rate = self.bytes_per_ms.map_or(sample, |previous| {
            if sample < previous {
                sample
            } else {
                previous * 0.75 + sample * 0.25
            }
        });
        self.bytes_per_ms = Some(rate);
        let proposed = ((rate * TARGET_WRITE_MS) as usize).clamp(MIN_BUDGET, MAX_BUDGET);
        if proposed < self.budget * 9 / 10 {
            // Slowdown is actionable immediately; recovery needs sustained evidence.
            self.budget = proposed;
            self.last_adjusted = Some(now);
        } else if proposed > self.budget * 6 / 5
            && self
                .last_adjusted
                .is_none_or(|last| now.saturating_duration_since(last) >= RECOVERY_INTERVAL)
        {
            self.budget = proposed.min(self.budget + self.budget / 4);
            self.last_adjusted = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_writes_shrink_immediately_and_fast_writes_recover_gradually() {
        let mut policy = BitmapBudget::default();
        let now = Instant::now();
        policy.observe(128 * 1024, 1000.0, now);
        assert_eq!(policy.bytes(), MIN_BUDGET);
        policy.observe(1024 * 1024, 20.0, now + Duration::from_secs(1));
        assert_eq!(policy.bytes(), MIN_BUDGET);
        for step in 1..30 {
            let before = policy.bytes();
            policy.observe(1024 * 1024, 20.0, now + Duration::from_secs(step * 2));
            assert!(policy.bytes() <= before + before / 4);
            assert!(policy.bytes() <= MAX_BUDGET);
        }
        // Hysteresis permits a stable budget somewhat below the cap.
        assert!(policy.bytes() >= MAX_BUDGET * 4 / 5);
        policy.observe(32 * 1024, 800.0, now + Duration::from_secs(62));
        assert_eq!(policy.bytes(), MIN_BUDGET);
    }

    #[test]
    fn empty_invalid_and_buffered_writes_cannot_inflate_or_poison_learning() {
        let mut policy = BitmapBudget::default();
        let now = Instant::now();
        for (bytes, ms) in [
            (0, 100.0),
            (512, 5.0),
            (32_768, f64::NAN),
            (32_768, f64::INFINITY),
            (32_768, -1.0),
            (32_768, 0.01),
        ] {
            policy.observe(bytes, ms, now);
        }
        assert_eq!(policy.bytes(), INITIAL_BUDGET);
        assert!(policy.bytes_per_ms.is_none());
        policy.observe(128 * 1024, 500.0, now);
        assert_eq!(policy.bytes(), MIN_BUDGET);
    }
}
