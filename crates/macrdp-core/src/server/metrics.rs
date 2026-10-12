use super::*;

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

    if gfx.bitmap_fallback_mode {
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
        if gfx.avc444_supported && gfx.avc444_enabled {
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
        let ack_ms = sanitize_f64(gfx.rtt_ewma_ms);
        Metrics {
            fps: updates_per_sec,
            bitrate_kbps,
            rtt_ms: ack_ms,
            net_ms: ack_ms,
            bytes_sent: gfx.total_bytes_sent,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            encode_ms: sanitize_f64(gfx.last_encode_ms),
            capture_queue_ms: sanitize_f64(gfx.capture_queue_ms),
            completion_queue_ms: sanitize_f64(gfx.completion_queue_ms),
            write_ms: sanitize_f64(gfx.last_write_ms),
            capture_frames_coalesced: gfx.capture_frames_coalesced,
            client_queue_depth_bytes: gfx.client_queue_depth_bytes,
            ack_suspended: gfx.frame_acknowledgements_suspended,
            last_frame_bytes: gfx.last_frame_bytes,
            network_quality: sanitize_f32(gfx.network_quality),
            pending_acks: gfx.pending_acks,
            transport_mode,
            encoder_mode,
            bitrate_mode,
            ..Metrics::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        gfx.bitmap_fallback_mode = true;
        assert_eq!(
            MetricsSnapshot::default().sample(&gfx).encoder_mode,
            "bitmap"
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
