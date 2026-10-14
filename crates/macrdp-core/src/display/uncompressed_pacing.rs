//! Pace raw GFX fallback from completed preparation and socket writes.
//!
//! Wait before taking a capture so the mailbox retains the newest pixels and
//! accumulated damage. Geometry says nothing about whether pixels changed.

use std::time::{Duration, Instant};

const MIN_INTERVAL_MS: f64 = 120.0;
const PRESSURED_INTERVAL_MS: f64 = 250.0;
const MAX_ADDITIONAL_WAIT_MS: f64 = 250.0;

fn acknowledged_backlog(
    last_ack_frame: u32,
    suspended: bool,
    pending_acks: u32,
    oldest_pending: Option<Duration>,
    queue_bytes: Option<u32>,
) -> bool {
    last_ack_frame != 0
        && !suspended
        && ((pending_acks >= 4
            && oldest_pending.is_some_and(|age| age >= Duration::from_millis(250)))
            || queue_bytes.is_some_and(|bytes| bytes >= 4 * 1024 * 1024))
}

impl super::MacDisplayUpdates {
    pub(super) fn sync_uncompressed_pacing(&mut self) {
        let (enabled, normal_avc, costs, backlog) = {
            let gfx = self.gfx_state.lock().unwrap();
            let active_gfx =
                gfx.session_uses_gfx && !gfx.bitmap_fallback_mode && gfx.has_gfx_channel();
            let fallback = self.encoder.is_none()
                || !gfx.avc420_supported
                || gfx.gfx_validation_mode
                || gfx.avc_fallback_to_uncompressed
                || super::DEFAULT_FORCE_FULL_FRAME_UNCOMPRESSED_GFX;
            (
                active_gfx && fallback,
                active_gfx && !fallback,
                UncompressedCosts {
                    dispatches: gfx.uncompressed_dispatches,
                    prepare_ms: gfx.uncompressed_prepare_ms_total,
                    write_ms: gfx.uncompressed_write_ms_total,
                },
                acknowledged_backlog(
                    gfx.last_ack_frame,
                    gfx.frame_acknowledgements_suspended,
                    gfx.pending_acks,
                    gfx.frame_send_times.values().min().map(Instant::elapsed),
                    gfx.client_queue_depth_bytes,
                ),
            )
        };
        if !enabled {
            let was_active = self.uncompressed_pacing.is_active();
            self.uncompressed_pacing.reset(costs);
            if was_active && normal_avc {
                // Encoder startup must not leave AVC running at the raw-output
                // ceiling. Resume the video controller's current capture rate.
                self.apply_capture_fps(
                    self.bitrate_ctrl.current_fps(),
                    "uncompressed-fallback-ended",
                );
            }
            return;
        }
        self.sync_runtime_encoder_overrides();
        if let Some(fps) = self.uncompressed_pacing.observe(
            costs,
            self.bitrate_ctrl.target_fps(),
            self.capture_config.frame_rate,
            backlog,
            Instant::now(),
        ) {
            tracing::debug!(
                requested_fps = fps,
                acknowledged_backlog = backlog,
                completed_dispatches = costs.dispatches,
                "Pacing uncompressed GFX from completed transmission costs"
            );
            self.apply_capture_fps(fps, "uncompressed-service-time");
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct UncompressedCosts {
    pub dispatches: u64,
    pub prepare_ms: f64,
    pub write_ms: f64,
}

#[derive(Default)]
pub(super) struct UncompressedPacing {
    active: bool,
    seen: UncompressedCosts,
    service_ewma_ms: Option<f64>,
    last_completed: Option<(Instant, f64)>,
    ready_at: Option<Instant>,
}

impl UncompressedPacing {
    pub(super) fn is_active(&self) -> bool {
        self.active
    }

    pub(super) fn reset(&mut self, costs: UncompressedCosts) {
        *self = Self {
            seen: costs,
            ..Self::default()
        };
    }

    pub(super) fn observe(
        &mut self,
        costs: UncompressedCosts,
        target_fps: u32,
        current_fps: u32,
        acknowledged_backlog: bool,
        now: Instant,
    ) -> Option<u32> {
        if !self.active || costs.dispatches < self.seen.dispatches {
            // Ignore completions belonging to an earlier transport mode/session.
            self.reset(costs);
            self.active = true;
        }
        let updates = costs.dispatches.saturating_sub(self.seen.dispatches);
        let prepare = costs.prepare_ms - self.seen.prepare_ms;
        let write = costs.write_ms - self.seen.write_ms;
        self.seen = costs;
        if updates > 0 && prepare >= 0.0 && write >= 0.0 && (prepare + write).is_finite() {
            let service = (prepare + write) / updates as f64;
            self.service_ewma_ms = Some(
                self.service_ewma_ms
                    .map_or(service, |old| old * 0.75 + service * 0.25),
            );
            self.last_completed = Some((now, service));
        }

        // Socket writes may initially fit in kernel buffers on a slow link.
        // Keep a conservative fallback ceiling even before congestion appears.
        let target = target_fps.clamp(1, 240);
        let base_ms = (1000.0 / f64::from(target)).max(if acknowledged_backlog {
            PRESSURED_INTERVAL_MS
        } else {
            MIN_INTERVAL_MS
        });
        let interval_ms = base_ms.max(self.service_ewma_ms.unwrap_or(0.0) / 0.75);
        self.ready_at = self.last_completed.map(|(completed, service)| {
            // A slow completed write has already delayed the client. Do not
            // add another multi-second suppression interval on top of it.
            let additional = (interval_ms - service)
                .max(0.0)
                .min(MAX_ADDITIONAL_WAIT_MS.max(base_ms));
            completed + Duration::from_secs_f64(additional / 1000.0)
        });
        let fps = ((1000.0 / interval_ms).floor() as u32).clamp(1, target);
        (fps != current_fps).then_some(fps)
    }

    pub(super) fn delay(&self, now: Instant) -> Option<Duration> {
        self.ready_at
            .and_then(|ready| ready.checked_duration_since(now))
            .filter(|delay| !delay.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_seed_is_immediate_and_completed_writes_bound_buffered_bursts() {
        let now = Instant::now();
        let mut pacing = UncompressedPacing::default();
        assert_eq!(
            pacing.observe(UncompressedCosts::default(), 60, 60, false, now),
            Some(8)
        );
        assert_eq!(pacing.delay(now), None);
        let costs = UncompressedCosts {
            dispatches: 1,
            prepare_ms: 2.0,
            write_ms: 1.0,
        };
        assert_eq!(pacing.observe(costs, 60, 8, false, now), None);
        assert_eq!(pacing.delay(now), Some(Duration::from_millis(117)));
        // Polling or a cursor wake must not postpone the same completed sample.
        pacing.observe(costs, 60, 8, false, now + Duration::from_millis(50));
        assert_eq!(pacing.delay(now + Duration::from_millis(117)), None);
    }

    #[test]
    fn slow_service_reduces_capture_and_recovers_without_long_suppression() {
        let now = Instant::now();
        let mut pacing = UncompressedPacing::default();
        let mut costs = UncompressedCosts::default();
        pacing.observe(costs, 60, 60, false, now);
        costs.dispatches = 1;
        costs.prepare_ms = 100.0;
        costs.write_ms = 1900.0;
        assert_eq!(pacing.observe(costs, 60, 8, false, now), Some(1));
        assert_eq!(pacing.delay(now), Some(Duration::from_millis(250)));
        let mut fps = 1;
        for sample in 1..30 {
            costs.dispatches += 1;
            costs.prepare_ms += 2.0;
            costs.write_ms += 1.0;
            let at = now + Duration::from_secs(sample);
            if let Some(next) = pacing.observe(costs, 60, fps, false, at) {
                assert!(next >= fps && next <= 8);
                fps = next;
            }
            assert!(pacing.delay(at).unwrap_or_default() <= Duration::from_millis(250));
        }
        assert_eq!(fps, 8);
    }

    #[test]
    fn known_ack_backlog_slows_output_but_never_waits_for_an_ack() {
        let now = Instant::now();
        let mut pacing = UncompressedPacing::default();
        pacing.observe(UncompressedCosts::default(), 60, 60, false, now);
        let costs = UncompressedCosts {
            dispatches: 1,
            prepare_ms: 1.0,
            write_ms: 1.0,
        };
        assert_eq!(pacing.observe(costs, 60, 8, true, now), Some(4));
        assert_eq!(pacing.delay(now), Some(Duration::from_millis(248)));
        let later = now + Duration::from_secs(1);
        pacing.observe(costs, 60, 4, true, later);
        assert_eq!(pacing.delay(later), None);
    }

    #[test]
    fn missing_or_suspended_acknowledgements_do_not_create_client_pressure() {
        let old = Some(Duration::from_secs(10));
        let large_queue = Some(8 * 1024 * 1024);
        assert!(!acknowledged_backlog(0, false, 20, old, large_queue));
        assert!(!acknowledged_backlog(3, true, 20, old, large_queue));
        assert!(acknowledged_backlog(3, false, 4, old, None));
        assert!(acknowledged_backlog(3, false, 1, None, large_queue));
        assert!(!acknowledged_backlog(3, false, 3, old, Some(1)));
    }

    #[test]
    fn target_changes_and_transport_resets_do_not_reuse_old_deadlines() {
        let now = Instant::now();
        let mut pacing = UncompressedPacing::default();
        let empty = UncompressedCosts::default();
        assert_eq!(pacing.observe(empty, 2, 60, false, now), Some(2));
        let costs = UncompressedCosts {
            dispatches: 1,
            prepare_ms: 1.0,
            write_ms: 1.0,
        };
        pacing.observe(costs, 2, 2, false, now);
        assert_eq!(pacing.delay(now), Some(Duration::from_millis(498)));
        assert_eq!(pacing.observe(costs, 1, 2, false, now), Some(1));
        assert_eq!(pacing.delay(now), Some(Duration::from_millis(998)));
        pacing.reset(costs);
        assert!(!pacing.is_active());
        assert_eq!(pacing.delay(now), None);
        pacing.observe(costs, 60, 60, false, now);
        assert_eq!(pacing.delay(now), None);
        pacing.observe(empty, 60, 8, false, now); // New session counters.
        assert_eq!(pacing.delay(now), None);
    }

    #[test]
    fn malformed_timing_cannot_panic_or_freeze_delivery() {
        let now = Instant::now();
        let mut pacing = UncompressedPacing::default();
        pacing.observe(UncompressedCosts::default(), 60, 60, false, now);
        pacing.observe(
            UncompressedCosts {
                dispatches: 1,
                prepare_ms: f64::INFINITY,
                write_ms: 0.0,
            },
            60,
            8,
            false,
            now,
        );
        assert_eq!(pacing.delay(now), None);
        pacing.observe(
            UncompressedCosts {
                dispatches: 2,
                prepare_ms: 1.0,
                write_ms: -1.0,
            },
            60,
            8,
            false,
            now,
        );
        assert_eq!(pacing.delay(now), None);
    }

    #[test]
    fn paced_repeated_geometry_converges_to_latest_pixels_and_skips_only_proven_duplicates() {
        use super::super::{
            bitmap_freshness, damage, extract_bgra_rect, prepare_uncompressed_dirty_rects,
            CachedFrame,
        };
        use bytes::Bytes;
        use macrdp_capture::{CapturedFrame, FrameData, Rect};

        let make_frame = |pixels: [u8; 4], changed: &[u32]| CapturedFrame {
            width: 4,
            height: 1,
            stride: 16,
            timestamp_us: 0,
            content_rect: None,
            data: FrameData::Raw(Bytes::from(
                pixels.into_iter().flat_map(|v| [v; 4]).collect::<Vec<_>>(),
            )),
            dirty_rects: changed
                .iter()
                .map(|&x| Rect {
                    x,
                    y: 0,
                    width: 1,
                    height: 1,
                })
                .collect(),
        };
        let now = Instant::now();
        let mut pacing = UncompressedPacing::default();
        pacing.observe(UncompressedCosts::default(), 60, 60, false, now);
        let costs = UncompressedCosts {
            dispatches: 1,
            prepare_ms: 2.0,
            write_ms: 1.0,
        };
        pacing.observe(costs, 60, 8, false, now);
        let baseline = CachedFrame::Bgra {
            data: Bytes::from(vec![0; 16]),
            stride: 16,
            width: 4,
            height: 1,
        };
        // The same rectangle changes twice while the consumer is paced. A
        // second region also changes; latest pixels retain both damage hints.
        let intermediate = make_frame([0, 1, 0, 0], &[1]);
        let mut latest = make_frame([0, 2, 0, 3], &[1, 3]);
        bitmap_freshness::accumulate_frame(&mut latest, &intermediate);
        assert!(pacing.delay(now).is_some());
        let resumed = now + Duration::from_millis(120);
        pacing.observe(costs, 60, 8, false, resumed);
        assert_eq!(pacing.delay(resumed), None);
        let damage = damage::refine_capture_dirty_rects(&mut latest, Some(&baseline), false);
        assert!(!damage.can_skip(false));
        let FrameData::Raw(pixels) = &latest.data else {
            unreachable!()
        };
        let mut client = vec![0; 16];
        for rect in prepare_uncompressed_dirty_rects(4, 1, &latest.dirty_rects) {
            let update = extract_bgra_rect(pixels, 16, rect).unwrap();
            let start = rect.x as usize * 4;
            client[start..start + update.len()].copy_from_slice(&update);
        }
        assert_eq!(client, pixels.as_ref());
        let delivered = CachedFrame::Bgra {
            data: pixels.clone(),
            stride: 16,
            width: 4,
            height: 1,
        };
        let mut duplicate = make_frame([0, 2, 0, 3], &[1, 3]);
        let damage = damage::refine_capture_dirty_rects(&mut duplicate, Some(&delivered), false);
        assert!(damage.can_skip(false));
        assert!(!damage.can_skip(true)); // Seed/stale-client repair still wins.
    }
}
