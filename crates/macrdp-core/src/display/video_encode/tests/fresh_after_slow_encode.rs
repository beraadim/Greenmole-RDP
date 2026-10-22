//! Exercise the real display dispatch with synthetic pixels and a synchronous
//! mock codec. The disconnected controller never creates a native display;
//! capture, host input and the running server are not involved.
use crate::display::*;
use macrdp_encode::{Avc444EncodedFrame, EncodedFrame};

pub(super) struct RecordingEncoder;

fn encoded(pixels: &[u8]) -> EncodedFrame {
    EncodedFrame {
        data: Bytes::from(vec![0, 0, 1, 0x65, pixels[0]]),
        is_keyframe: true,
        width: 16,
        height: 16,
    }
}

impl VideoEncoder for RecordingEncoder {
    fn backend_name(&self) -> &'static str {
        "synthetic-display-test"
    }
    fn encode_bgra(&mut self, data: &[u8], _: u32, _: u32, _: usize) -> Result<EncodedFrame> {
        Ok(encoded(data))
    }
    fn encode_bgra_444(
        &mut self,
        data: &[u8],
        _: u32,
        _: u32,
        _: usize,
    ) -> Result<Avc444EncodedFrame> {
        Ok(Avc444EncodedFrame {
            main_view: encoded(data),
            aux_view: encoded(data),
        })
    }
    fn set_bitrate(&mut self, _: u32) -> Result<()> {
        Ok(())
    }
    fn force_keyframe(&mut self) {}
    fn supports_444(&self) -> bool {
        true
    }
}

pub(super) fn fixture(avc444: bool) -> MacDisplayUpdates {
    let mut state = GfxState::new(16, 16, avc444, false);
    state.session_uses_gfx = true;
    state.channel_id = Some(1);
    state.caps_confirmed = true;
    state.avc420_supported = true;
    state.avc444_supported = true;
    state.browser_avc420 = !avc444;
    state.browser_avc444 = avc444;
    let controls_rx = state.controls_receiver();
    let now = Instant::now();
    MacDisplayUpdates {
        capturer: None,
        capture_recovery: None,
        // A disconnected Virtual controller only owns a command worker. No
        // activate/prepare call is made, so no helper or screen is created.
        display_controller: macrdp_display::DisplayController::new(
            macrdp_display::DisplayBackend::Virtual,
            320,
            200,
        )
        .unwrap(),
        capture_config: CaptureConfig {
            display_id: 0,
            width: 16,
            height: 16,
            frame_rate: 30,
            pixel_format: CapturePixelFormat::Bgra,
            show_cursor: false,
        },
        encoder: Some(Box::new(RecordingEncoder)),
        encoder_startup: None,
        startup_mode_444: None,
        encoder_startup_attempted: true,
        encoder_failed: false,
        video_encode_recovery: super::super::Recovery::default(),
        pending_avc444: None,
        quality: Quality::HighQuality,
        gfx_state: Arc::new(Mutex::new(state)),
        coord_mapper: MouseCoordMapper::new(16, 16, 16, 16),
        resize_rx: tokio::sync::watch::channel(DesktopSize {
            width: 16,
            height: 16,
        })
        .1,
        encoder_pref: macrdp_encode::EncoderPreference::Hardware,
        controls_rx,
        encoder_rebuild_deadline: None,
        settings_refresh_pending: false,
        default_frame_rate: 30,
        bitrate_override: Some(50_000_000),
        mode_444: avc444,
        shared_audio_tx: None,
        display_frame_count: 0,
        slow_video_encodes: 0,
        idle_frame_count: 0,
        idle_since: None,
        last_idle_refresh_attempt: now,
        last_idr_time: now,
        last_frame_cache: None,
        last_cursor_fingerprint: None,
        cursor_capture: CursorCapture::default(),
        next_cursor_poll_at: now,
        cursor_fairness: cursor_fairness::CursorFairness::default(),
        uncompressed_pacing: uncompressed_pacing::UncompressedPacing::default(),
        bitrate_ctrl: BitrateController::new(50_000_000, 30, NetworkQuality::from_ip(true)),
        last_applied_fps: 30,
        has_pending_submit: false,
        pending_visible_size: None,
        pending_frame_time: None,
        needs_full_bitmap_seed: false,
        forced_full_bitmap_updates_remaining: 0,
        pending_bitmap_updates: VecDeque::new(),
        pending_bitmap_frame: None,
        bitmap_copy_stats: bitmap_updates::CopyStats::default(),
        deferred_bitmap_frame: None,
        bitmap_pacing: bitmap_pacing::BitmapPacing::default(),
        bitmap_perf_dispatches: 0,
        bitmap_perf_bytes: 0,
        last_full_bitmap_sent: None,
        needs_full_gfx_seed: false,
        logged_gfx_uncompressed_mode: false,
        logged_gfx_avc_mode: false,
        logged_bitmap_mode: false,
        client_may_be_stale: false,
        perf_stats: None,
        session_audit: SessionAudit::new(None),
    }
}

pub(super) fn capture(value: u8) -> CapturedFrame {
    CapturedFrame {
        data: FrameData::Raw(Bytes::from(vec![value; 16 * 16 * 4])),
        width: 16,
        height: 16,
        stride: 64,
        timestamp_us: 1,
        dirty_rects: vec![Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        }],
        content_rect: None,
    }
}

#[test]
fn a_slow_completed_video_frame_never_discards_the_only_newer_capture() {
    for avc444 in [false, true] {
        for require_refresh in [false, true] {
            let mut display = fixture(avc444);
            let previous = super::super::encode_bgra(
                display.encoder.as_mut().unwrap().as_mut(),
                &[7; 1024],
                16,
                16,
                64,
                avc444,
            )
            .unwrap();
            // Both callback waits count toward this elapsed duration. At 30fps,
            // 35ms crossed the old 80% threshold and armed a blind next-frame drop.
            assert!(display
                .finish_bgra_update(previous, Instant::now() - Duration::from_millis(35), 1, 1)
                .is_some());
            assert_eq!(display.slow_video_encodes, 1);
            if require_refresh {
                display.needs_full_gfx_seed = true;
                display.client_may_be_stale = true;
            }
            let damage = if require_refresh {
                FrameDamage::Unchanged
            } else {
                FrameDamage::Regions
            };
            let update = display
                .encode_and_send_with_damage(capture(42), damage)
                .unwrap();
            let Some(DisplayUpdate::GfxFrame(update)) = update else {
                panic!("The only fresh capture was lost after slow encode: avc444={avc444}, refresh={require_refresh}");
            };
            assert_eq!(update.h264_data[4], 42);
            assert_eq!(update.h264_aux.is_some(), avc444);
            assert!(!display.needs_full_gfx_seed && !display.client_may_be_stale);
            assert!(display.pending_avc444.is_none() && !display.has_pending_submit);
        }
    }
}
