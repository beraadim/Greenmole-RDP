use super::*;

static NEXT_SAMPLE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn sanitize_f64(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn sanitize_f32(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn derive_transport_mode(gfx: &GfxState) -> String {
    if !gfx.session_uses_gfx {
        return "bitmap".to_string();
    }

    if gfx.bitmap_mode() || gfx.encoder_bitmap_fallback {
        return "bitmap-fallback".to_string();
    }

    if gfx.gfx_validation_mode && gfx.caps_confirmed {
        return "gfx-uncompressed".to_string();
    }

    if gfx.avc_fallback_to_uncompressed
        || (gfx.caps_confirmed && gfx.avc420_supported && gfx.active_encoder.is_none())
    {
        return "gfx-uncompressed-fallback".to_string();
    }

    if gfx.avc420_supported {
        if gfx.use_avc444() {
            "gfx-avc444".to_string()
        } else {
            "gfx-avc420".to_string()
        }
    } else if gfx.caps_confirmed {
        "gfx-uncompressed".to_string()
    } else {
        "gfx-negotiating".to_string()
    }
}

fn derive_encoder_mode(gfx: &GfxState, transport_mode: &str) -> String {
    let encoder = gfx.active_encoder.unwrap_or("unavailable");

    match transport_mode {
        "gfx-avc444" => format!("{encoder} / avc444"),
        "gfx-avc420" => format!("{encoder} / avc420"),
        "gfx-uncompressed" | "gfx-uncompressed-fallback" => "uncompressed".to_string(),
        "bitmap" | "bitmap-fallback" => "bitmap".to_string(),
        _ => "negotiating".to_string(),
    }
}

fn derive_bitrate_mode(transport_mode: &str) -> String {
    match transport_mode {
        "gfx-avc420" | "gfx-avc444" => "AVC target + wire throughput".to_string(),
        "gfx-uncompressed" | "gfx-uncompressed-fallback" => {
            "Wire throughput only (uncompressed GFX)".to_string()
        }
        "bitmap" | "bitmap-fallback" => "Wire throughput only (bitmap updates)".to_string(),
        _ => "Wire throughput".to_string(),
    }
}

#[derive(Debug, Default)]
pub(super) struct MetricsSnapshot {
    last_sample_at: Option<Instant>,
    connection_started_at: Option<Instant>,
    last_bytes_sent: u64,
    last_updates_sent: u64,
}

impl MetricsSnapshot {
    /// Sample live activity without presenting a completed frame as new work.
    /// Keep totals and the fixed outcome for diagnostics; final session metrics
    /// use `sample()` directly and retain their complete timing summaries.
    pub(super) fn sample_live(&mut self, gfx: &GfxState, session_active: bool) -> Metrics {
        let mut metrics = self.sample(gfx);
        if session_active {
            metrics.session_active = true;
            metrics
        } else {
            Metrics {
                sample_id: metrics.sample_id,
                timestamp: metrics.timestamp,
                timestamp_ms: metrics.timestamp_ms,
                bytes_sent: metrics.bytes_sent,
                connection_outcome: metrics.connection_outcome.take(),
                ..Metrics::default()
            }
        }
    }

    pub(super) fn sample(&mut self, gfx: &GfxState) -> Metrics {
        let now = Instant::now();
        let same_connection = self.connection_started_at == gfx.start_time;
        let elapsed = self
            .last_sample_at
            .map(|last| now.duration_since(last).as_secs_f64());
        let (bitrate_kbps, updates_per_sec) = match elapsed {
            Some(seconds) if seconds > 0.0 && same_connection => (
                (gfx.total_bytes_sent.saturating_sub(self.last_bytes_sent) as f64 * 8.0
                    / seconds
                    / 1000.0)
                    .round() as u64,
                (gfx.total_frames_sent.saturating_sub(self.last_updates_sent) as f64 / seconds)
                    .round() as u32,
            ),
            _ => (0, 0),
        };
        self.last_sample_at = Some(now);
        self.connection_started_at = gfx.start_time;
        self.last_bytes_sent = gfx.total_bytes_sent;
        self.last_updates_sent = gfx.total_frames_sent;
        let transport_mode = derive_transport_mode(gfx);
        let encoder_mode = derive_encoder_mode(gfx, &transport_mode);
        let bitrate_mode = derive_bitrate_mode(&transport_mode);
        let uses_video = matches!(transport_mode.as_str(), "gfx-avc420" | "gfx-avc444");
        let ack_ms = sanitize_f64(gfx.rtt_ewma_ms);
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Metrics {
            fps: updates_per_sec,
            bitrate_kbps,
            rtt_ms: ack_ms,
            net_ms: ack_ms,
            bytes_sent: gfx.total_bytes_sent,
            timestamp: timestamp_ms / 1000,
            timestamp_ms,
            sample_id: NEXT_SAMPLE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            encode_ms: sanitize_f64(gfx.last_encode_ms),
            capture_queue_ms: sanitize_f64(gfx.capture_queue_ms),
            completion_queue_ms: sanitize_f64(gfx.completion_queue_ms),
            write_ms: sanitize_f64(gfx.last_write_ms),
            capture_frames_coalesced: gfx.capture_frames_coalesced,
            requested_capture_frame_rate: gfx.requested_capture_frame_rate,
            applied_frame_rate: gfx.applied_frame_rate,
            capture_frame_rate_update_failures: gfx.capture_frame_rate_update_failures,
            capture_frame_rate_retry_exhausted: gfx.capture_frame_rate_retry_exhausted,
            requested_encoder_bitrate: uses_video
                .then_some(gfx.requested_encoder_bitrate)
                .flatten(),
            applied_encoder_bitrate: uses_video.then_some(gfx.applied_encoder_bitrate).flatten(),
            encoder_bitrate_update_failures: gfx.encoder_bitrate_update_failures,
            bitmap_codec: matches!(transport_mode.as_str(), "bitmap" | "bitmap-fallback")
                .then_some(gfx.bitmap_codec)
                .flatten()
                .map(str::to_owned),
            negotiated_client_depth: gfx.negotiated_client_depth,
            bitmap_update_budget_bytes: matches!(
                transport_mode.as_str(),
                "bitmap" | "bitmap-fallback"
            )
            .then_some(gfx.bitmap_update_budget_bytes)
            .flatten(),
            client_queue_depth_bytes: gfx.client_queue_depth_bytes,
            ack_suspended: gfx.frame_acknowledgements_suspended,
            clipboard_disabled: gfx.clipboard_disabled,
            last_frame_bytes: gfx.last_frame_bytes,
            network_quality: sanitize_f32(gfx.network_quality),
            pending_acks: gfx.pending_acks,
            transport_mode,
            encoder_mode,
            bitrate_mode,
            capture_timings: gfx.timings.capture.summary().into(),
            encode_timings: gfx.timings.encode.summary().into(),
            completion_timings: gfx.timings.completion.summary().into(),
            write_timings: gfx.timings.write.summary().into(),
            connection_outcome: gfx
                .connection_outcome
                .map(|reason| reason.as_str().to_owned()),
            ..Metrics::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_heartbeats_do_not_repeat_completed_frame_activity() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.start_time = Some(Instant::now());
        gfx.total_bytes_sent = 4096;
        gfx.total_frames_sent = 10;
        gfx.record_encode_timing(3.0);
        gfx.record_capture_queue(2.0);
        gfx.last_write_ms = 1.0;
        gfx.rtt_ewma_ms = 7.0;
        gfx.pending_acks = 2;
        gfx.applied_frame_rate = Some(60);
        let mut snapshot = MetricsSnapshot::default();
        let active = snapshot.sample_live(&gfx, true);
        assert!(active.session_active);
        assert_eq!(active.encode_ms, 3.0);
        assert_eq!(active.encode_timings.samples, 1);

        for _ in 0..3 {
            let idle = snapshot.sample_live(&gfx, false);
            assert!(!idle.session_active);
            assert!(idle.sample_id > active.sample_id);
            assert!(idle.timestamp_ms > 0);
            assert_eq!(idle.bytes_sent, 4096);
            assert_eq!(idle.fps, 0);
            assert_eq!(idle.bitrate_kbps, 0);
            assert_eq!(idle.encode_ms, 0.0);
            assert_eq!(idle.capture_queue_ms, 0.0);
            assert_eq!(idle.write_ms, 0.0);
            assert_eq!(idle.rtt_ms, 0.0);
            assert_eq!(idle.pending_acks, 0);
            assert_eq!(idle.applied_frame_rate, None);
            assert_eq!(idle.encode_timings.samples, 0);
            assert!(idle.encoder_mode.is_empty());
        }
        // Publishing idle does not erase the completed session's evidence.
        let final_sample = snapshot.sample(&gfx);
        assert_eq!(final_sample.encode_ms, 3.0);
        assert_eq!(final_sample.encode_timings.samples, 1);
        assert_eq!(final_sample.capture_timings.samples, 1);
    }

    #[test]
    fn new_connection_after_idle_starts_with_fresh_rates_and_timings() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.start_time = Some(Instant::now());
        gfx.total_bytes_sent = 10_000;
        gfx.record_encode_timing(3.0);
        let mut snapshot = MetricsSnapshot::default();
        snapshot.sample_live(&gfx, true);
        snapshot.sample_live(&gfx, false);
        gfx.reset_connection_state(false);
        gfx.start_time = Some(Instant::now());
        let fresh = snapshot.sample_live(&gfx, true);
        assert!(fresh.session_active);
        assert_eq!(fresh.bitrate_kbps, 0);
        assert_eq!(fresh.fps, 0);
        assert_eq!(fresh.encode_ms, 0.0);
        assert_eq!(fresh.encode_timings.samples, 0);
    }

    #[test]
    fn bitmap_metrics_identify_codec_without_claiming_an_active_video_target() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.bitmap_codec = Some("remotefx");
        gfx.negotiated_client_depth = Some(24);
        gfx.requested_encoder_bitrate = Some(200_000_000);
        gfx.applied_encoder_bitrate = Some(200_000_000);
        let metrics = MetricsSnapshot::default().sample(&gfx);
        assert_eq!(metrics.bitmap_codec.as_deref(), Some("remotefx"));
        assert_eq!(metrics.negotiated_client_depth, Some(24));
        assert_eq!(metrics.requested_encoder_bitrate, None);
        assert_eq!(metrics.applied_encoder_bitrate, None);
    }

    #[test]
    fn samples_keep_order_across_service_restarts_and_snapshot_instances() {
        let gfx = GfxState::new(1920, 1080, false, false);
        let first = MetricsSnapshot::default().sample(&gfx);
        let second = MetricsSnapshot::default().sample(&gfx);
        assert!(first.sample_id > 0);
        assert!(second.sample_id > first.sample_id);
        assert_eq!(second.timestamp, second.timestamp_ms / 1000);
    }

    #[test]
    fn encoder_metrics_show_rejected_target_and_clear_with_connection() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.session_uses_gfx = true;
        gfx.caps_confirmed = true;
        gfx.avc420_supported = true;
        gfx.active_encoder = Some("videotoolbox");
        gfx.requested_encoder_bitrate = Some(8_000_000);
        gfx.applied_encoder_bitrate = Some(10_000_000);
        gfx.encoder_bitrate_update_failures = 1;
        let mut snapshot = MetricsSnapshot::default();
        let metrics = snapshot.sample(&gfx);
        assert_eq!(metrics.requested_encoder_bitrate, Some(8_000_000));
        assert_eq!(metrics.applied_encoder_bitrate, Some(10_000_000));
        assert_eq!(metrics.encoder_bitrate_update_failures, 1);
        gfx.reset_connection_state(false);
        let metrics = snapshot.sample(&gfx);
        assert_eq!(metrics.applied_encoder_bitrate, None);
        assert_eq!(metrics.encoder_bitrate_update_failures, 0);
    }

    #[test]
    fn capture_rate_metrics_distinguish_requested_applied_and_exhausted_updates() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.target_frame_rate = Some(60);
        gfx.requested_capture_frame_rate = Some(20);
        gfx.applied_frame_rate = Some(30);
        gfx.capture_frame_rate_update_failures = 4;
        gfx.capture_frame_rate_retry_exhausted = true;
        let mut snapshot = MetricsSnapshot::default();
        let metrics = snapshot.sample(&gfx);
        assert_eq!(metrics.requested_capture_frame_rate, Some(20));
        assert_eq!(metrics.applied_frame_rate, Some(30));
        assert_eq!(metrics.capture_frame_rate_update_failures, 4);
        assert!(metrics.capture_frame_rate_retry_exhausted);
        gfx.reset_connection_state(false);
        let metrics = snapshot.sample(&gfx);
        assert_eq!(metrics.requested_capture_frame_rate, None);
        assert_eq!(metrics.applied_frame_rate, None);
        assert_eq!(metrics.capture_frame_rate_update_failures, 0);
        assert!(!metrics.capture_frame_rate_retry_exhausted);
    }

    #[test]
    fn timing_summaries_include_stage_events_without_polling_duplication() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.record_capture_queue(30.0);
        gfx.record_encode_timing(12.0);
        gfx.record_completion_queue(4.0);
        gfx.record_display_write(1024, 250.0);
        let mut snapshot = MetricsSnapshot::default();
        for _ in 0..3 {
            let metrics = snapshot.sample(&gfx);
            assert_eq!(metrics.encode_timings.samples, 1);
            assert_eq!(metrics.encode_timings.p95_ms, Some(12.0));
            assert_eq!(metrics.write_timings.max_ms, Some(250.0));
            assert_eq!(metrics.capture_timings.max_ms, Some(30.0));
            assert_eq!(metrics.completion_timings.max_ms, Some(4.0));
        }
        gfx.reset_connection_state(false);
        let metrics = snapshot.sample(&gfx);
        assert_eq!(metrics.write_timings.samples, 0);
        assert_eq!(metrics.encode_timings.p95_ms, None);
    }

    #[test]
    fn reports_instantiated_backend_and_actual_fallback_transport() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.encoder_pref = Some("hardware".into());
        gfx.session_uses_gfx = true;
        gfx.caps_confirmed = true;
        gfx.avc420_supported = true;
        gfx.active_encoder = Some("openh264");
        assert_eq!(
            MetricsSnapshot::default().sample(&gfx).encoder_mode,
            "openh264 / avc420"
        );
        gfx.active_encoder = None;
        let metrics = MetricsSnapshot::default().sample(&gfx);
        assert_eq!(metrics.transport_mode, "gfx-uncompressed-fallback");
        assert_eq!(metrics.encoder_mode, "uncompressed");
        gfx.encoder_bitmap_fallback = true;
        let metrics = MetricsSnapshot::default().sample(&gfx);
        assert_eq!(metrics.transport_mode, "bitmap-fallback");
        assert_eq!(metrics.encoder_mode, "bitmap");
        assert_eq!(
            metrics.bitrate_mode,
            "Wire throughput only (bitmap updates)"
        );
        assert!(gfx.avc420_supported && !gfx.bitmap_mode());
        gfx.encoder_bitmap_fallback = false;
        gfx.bitmap_fallback_mode = true;
        assert_eq!(
            MetricsSnapshot::default().sample(&gfx).encoder_mode,
            "bitmap"
        );
        gfx.bitmap_fallback_mode = false;
        gfx.manual_bitmap_mode = true;
        let metrics = MetricsSnapshot::default().sample(&gfx);
        assert_eq!(metrics.transport_mode, "bitmap-fallback");
        assert_eq!(metrics.encoder_mode, "bitmap");
        assert_eq!(
            metrics.bitrate_mode,
            "Wire throughput only (bitmap updates)"
        );

        gfx.manual_bitmap_mode = false;
        gfx.target_quality = Some("lossless".into());
        assert_eq!(
            MetricsSnapshot::default().sample(&gfx).encoder_mode,
            "bitmap"
        );
        gfx.target_quality = Some("high_quality".into());
        gfx.active_encoder = Some("videotoolbox");
        assert_eq!(
            MetricsSnapshot::default().sample(&gfx).encoder_mode,
            "videotoolbox / avc420"
        );
    }

    #[test]
    fn validation_mode_reports_uncompressed_even_when_avc_was_negotiated() {
        let mut gfx = GfxState::new(1920, 1080, true, true);
        gfx.session_uses_gfx = true;
        gfx.caps_confirmed = true;
        gfx.avc420_supported = true;
        gfx.avc444_supported = true;
        gfx.active_encoder = Some("videotoolbox");
        let metrics = MetricsSnapshot::default().sample(&gfx);
        assert_eq!(metrics.transport_mode, "gfx-uncompressed");
        assert_eq!(metrics.encoder_mode, "uncompressed");
        assert_eq!(
            metrics.bitrate_mode,
            "Wire throughput only (uncompressed GFX)"
        );
    }

    #[test]
    fn ack_latency_is_not_reduced_by_encoder_time() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.rtt_ewma_ms = 8.0;
        gfx.last_encode_ms = 12.0;
        let metrics = MetricsSnapshot::default().sample(&gfx);
        assert_eq!(metrics.rtt_ms, 8.0);
        assert_eq!(metrics.net_ms, 8.0);
        assert_eq!(metrics.encode_ms, 12.0);
        assert_eq!(metrics.fps, 0);
    }

    #[test]
    fn a_new_connection_does_not_inherit_previous_throughput() {
        let mut snapshot = MetricsSnapshot::default();
        let mut gfx = GfxState::new(1920, 1080, false, false);
        gfx.start_time = Some(Instant::now());
        gfx.total_bytes_sent = 1000;
        snapshot.sample(&gfx);
        snapshot.last_sample_at = Some(Instant::now() - std::time::Duration::from_secs(1));
        gfx.start_time = Some(Instant::now());
        gfx.total_bytes_sent = 9000;
        gfx.total_frames_sent = 10;
        let metrics = snapshot.sample(&gfx);
        assert_eq!(metrics.bitrate_kbps, 0);
        assert_eq!(metrics.fps, 0);
    }
}
