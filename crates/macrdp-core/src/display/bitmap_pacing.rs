//! Pace capture by completed bitmap compression and socket service time.
//! Bitmap quality is lossless; changing an AVC bitrate cannot relieve this path.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default)]
pub(super) struct BitmapCosts {
    pub dispatches: u64,
    pub encode_ms: f64,
    pub write_ms: f64,
}

#[derive(Default)]
pub(super) struct BitmapPacing {
    seen: BitmapCosts,
    batch_cost_ms: f64,
    batch_updates: u64,
    service_ewma_ms: Option<f64>,
    last_adjusted: Option<Instant>,
}

impl BitmapPacing {
    pub fn observe(
        &mut self,
        costs: BitmapCosts,
        batch_finished: bool,
        target_fps: u32,
        current_fps: u32,
        now: Instant,
    ) -> Option<u32> {
        if costs.dispatches < self.seen.dispatches {
            *self = Self::default();
        }
        let updates = costs.dispatches.saturating_sub(self.seen.dispatches);
        let elapsed = (costs.encode_ms - self.seen.encode_ms).max(0.0)
            + (costs.write_ms - self.seen.write_ms).max(0.0);
        self.seen = costs;
        if elapsed.is_finite() {
            self.batch_cost_ms += elapsed;
            self.batch_updates = self.batch_updates.saturating_add(updates);
        }
        if !batch_finished || self.batch_updates == 0 {
            return None;
        }
        // Split regions belong to one capture: sum their costs before updating
        // the estimate. Treating each region as a whole frame hides overload.
        let service = self.service_ewma_ms.map_or(self.batch_cost_ms, |old| {
            old * 0.75 + self.batch_cost_ms * 0.25
        });
        self.service_ewma_ms = Some(service);
        self.batch_cost_ms = 0.0;
        self.batch_updates = 0;
        let target = target_fps.clamp(1, 240);
        let current = current_fps.clamp(1, target);
        let since_adjustment = self
            .last_adjusted
            .map(|last| now.saturating_duration_since(last));
        let budget = 1000.0 / f64::from(current);
        let next = if service > budget * 0.85
            && since_adjustment.is_none_or(|age| age >= Duration::from_millis(500))
        {
            ((750.0 / service).floor() as u32).clamp(1, current)
        } else if service < budget * 0.5
            && since_adjustment.is_none_or(|age| age >= Duration::from_secs(2))
        {
            current.saturating_add((current / 4).max(1)).min(target)
        } else {
            current
        };
        if next == current_fps {
            return None;
        }
        self.last_adjusted = Some(now);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_compression_and_write_cost_across_all_regions() {
        let mut pacing = BitmapPacing::default();
        let now = Instant::now();
        assert_eq!(
            pacing.observe(
                BitmapCosts {
                    dispatches: 1,
                    encode_ms: 10.0,
                    write_ms: 10.0
                },
                false,
                60,
                60,
                now
            ),
            None
        );
        assert_eq!(
            pacing.observe(
                BitmapCosts {
                    dispatches: 2,
                    encode_ms: 20.0,
                    write_ms: 20.0
                },
                true,
                60,
                60,
                now
            ),
            Some(18)
        );
        // Merely polling the same sample cannot reduce capture FPS again.
        assert_eq!(
            pacing.observe(
                BitmapCosts {
                    dispatches: 2,
                    encode_ms: 20.0,
                    write_ms: 20.0
                },
                true,
                60,
                18,
                now + Duration::from_secs(3)
            ),
            None
        );
    }

    #[test]
    fn sustained_recovery_is_gradual_and_never_exceeds_configured_cap() {
        let mut pacing = BitmapPacing::default();
        let now = Instant::now();
        let mut fps = 60;
        let mut costs = BitmapCosts {
            dispatches: 1,
            encode_ms: 20.0,
            write_ms: 80.0,
        };
        fps = pacing.observe(costs, true, 60, fps, now).unwrap();
        assert_eq!(fps, 7);
        for sample in 1..20 {
            costs.dispatches += 1;
            costs.encode_ms += 1.0;
            costs.write_ms += 1.0;
            if let Some(next) =
                pacing.observe(costs, true, 30, fps, now + Duration::from_secs(sample * 2))
            {
                assert!(next <= 30);
                assert!(next <= fps + (fps / 4).max(1));
                fps = next;
            }
        }
        assert_eq!(fps, 30);
    }
}
