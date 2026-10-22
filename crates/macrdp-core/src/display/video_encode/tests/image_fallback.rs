//! Exercise actual transport dispatch with synthetic pixels; no native codec,
//! capture, screen, host input or network connection is created by these tests.
use crate::display::*;
use macrdp_encode::EncoderPreference;

use super::fresh_after_slow_encode::{capture, fixture, RecordingEncoder};

fn assert_bitmap(update: Option<DisplayUpdate>, value: u8) {
    let Some(DisplayUpdate::Bitmap(bitmap)) = update else {
        panic!("Unavailable video must use the negotiated compressed bitmap encoder");
    };
    assert_eq!(bitmap.data[0], value);
    assert_eq!((bitmap.width.get(), bitmap.height.get()), (16, 16));
}

#[test]
fn unavailable_or_failed_avc_uses_bitmaps_without_latching_negotiation_failure() {
    for browser in [false, true] {
        for failed in [false, true] {
            let mut display = fixture(false);
            display.encoder = None;
            display.encoder_failed = failed;
            display.needs_full_gfx_seed = true;
            display.gfx_state.lock().unwrap().browser_avc420 = browser;
            // Even a reported no-change image needs a full bitmap baseline:
            // the last image may have been delivered through another codec.
            assert_bitmap(
                display
                    .encode_and_send_with_damage(capture(42), FrameDamage::Unchanged)
                    .unwrap(),
                42,
            );
            let gfx = display.gfx_state.lock().unwrap();
            assert!(gfx.encoder_bitmap_fallback);
            assert!(gfx.avc420_supported && gfx.has_gfx_channel());
            assert!(!gfx.bitmap_mode() && !gfx.avc_fallback_to_uncompressed);
            assert!(encoder_lifecycle::accepts_avc(
                &gfx,
                display.quality,
                display.encoder_pref
            ));
        }
    }
}

#[test]
fn cached_recovery_uses_the_same_compressed_fallback_as_new_captures() {
    let mut display = fixture(false);
    display.encoder = None;
    display.encoder_failed = true;
    display.cache_latest_frame(&capture(99));
    assert_bitmap(display.send_cached_full_frame_recovery(), 99);
    assert!(display.gfx_state.lock().unwrap().encoder_bitmap_fallback);
}

#[test]
fn first_ready_browser_idr_replaces_startup_bitmaps_without_a_raw_seed() {
    let mut display = fixture(false);
    display.encoder = None;
    assert_bitmap(display.encode_and_send(capture(7)).unwrap(), 7);
    display.encoder = Some(Box::new(RecordingEncoder));
    display.needs_full_gfx_seed = true;
    display.client_may_be_stale = true;
    let Some(DisplayUpdate::GfxFrame(video)) = display.encode_and_send(capture(42)).unwrap() else {
        panic!("Ready browser AVC must resume with a complete video IDR");
    };
    assert!(video.is_keyframe && video.h264_aux.is_none());
    assert_eq!(video.h264_data[4], 42);
    assert!(!display.gfx_state.lock().unwrap().encoder_bitmap_fallback);
}

#[test]
fn manual_bitmap_and_lossless_modes_keep_the_existing_image_transport() {
    for lossless in [false, true] {
        let mut display = fixture(false);
        if lossless {
            display.quality = Quality::Lossless;
            display.gfx_state.lock().unwrap().target_quality = Some("lossless".into());
        } else {
            display.encoder_pref = EncoderPreference::Bitmap;
            display.gfx_state.lock().unwrap().manual_bitmap_mode = true;
        }
        display.client_may_be_stale = true;
        assert_bitmap(display.encode_and_send(capture(42)).unwrap(), 42);
        assert!(!display.gfx_state.lock().unwrap().encoder_bitmap_fallback);
    }
}

#[test]
fn explicit_raw_validation_and_non_avc_clients_preserve_the_existing_path() {
    for kind in 0..3 {
        let mut display = fixture(false);
        display.encoder = None;
        display.needs_full_gfx_seed = true;
        {
            let mut gfx = display.gfx_state.lock().unwrap();
            match kind {
                0 => gfx.gfx_validation_mode = true,
                1 => gfx.avc_fallback_to_uncompressed = true,
                _ => gfx.avc420_supported = false,
            }
        }
        assert!(matches!(
            display.encode_and_send(capture(42)).unwrap(),
            Some(DisplayUpdate::GfxUncompressed(_))
        ));
        assert!(!display.gfx_state.lock().unwrap().encoder_bitmap_fallback);
    }
}

#[test]
fn no_ack_decision_uses_bitmap_on_that_same_dispatch() {
    let mut display = fixture(false);
    {
        let mut gfx = display.gfx_state.lock().unwrap();
        gfx.frame_id = 4;
        gfx.pending_acks = 4;
        gfx.frame_send_times
            .insert(1, Instant::now() - Duration::from_secs(2));
    }
    assert_bitmap(display.encode_and_send(capture(42)).unwrap(), 42);
    let gfx = display.gfx_state.lock().unwrap();
    assert!(gfx.bitmap_fallback_mode && gfx.avc420_supported);
    assert!(!gfx.avc_fallback_to_uncompressed);
}

#[test]
fn exhausted_video_failure_budget_switches_to_a_complete_bitmap_baseline() {
    for avc444 in [false, true] {
        let mut display = fixture(avc444);
        display.note_video_encode_failure(
            &anyhow::anyhow!("injected codec failure"),
            avc444,
            Instant::now() - Duration::from_secs(6),
        );
        assert!(display.encoder_failed && display.encoder.is_none());
        assert_bitmap(display.encode_and_send(capture(42)).unwrap(), 42);
    }
}

#[test]
fn compressed_fallback_uses_bitmap_costs_and_does_not_arm_raw_write_pacing() {
    let mut display = fixture(false);
    display.encoder = None;
    {
        let mut gfx = display.gfx_state.lock().unwrap();
        gfx.bitmap_dispatches = 1;
        gfx.bitmap_encode_ms_total = 40.0;
        gfx.bitmap_write_ms_total = 60.0;
    }
    display.sync_bitmap_pacing();
    display.sync_uncompressed_pacing();
    assert_eq!(display.capture_config.frame_rate, 7);
    assert!(!display.uncompressed_pacing.is_active());
    assert!(display.gfx_state.lock().unwrap().encoder_bitmap_fallback);
}

#[test]
fn ready_initializer_supersedes_queued_bitmaps_with_latest_pixels_and_video_fps() {
    let _guard = encoder_startup::test_guard();
    let mut display = fixture(false);
    display.encoder = None;
    assert_bitmap(display.encode_and_send(capture(7)).unwrap(), 7);

    // A partially dispatched bitmap and a later deferred capture must never
    // repaint over the first AVC baseline after the worker finally succeeds.
    display.pending_bitmap_updates = bitmap_updates::plan(&capture(7), true).0;
    display.pending_bitmap_frame = Some(capture(7));
    display.deferred_bitmap_frame = Some(capture(99));
    display.needs_full_bitmap_seed = true;
    display.forced_full_bitmap_updates_remaining = 2;
    display.capture_config.frame_rate = 7;
    display.last_applied_fps = 7;
    display.encoder_startup = Some(
        encoder_startup::Initializer::start(|| {
            Ok(Box::new(RecordingEncoder) as Box<dyn VideoEncoder>)
        })
        .expect("Synthetic initializer admitted"),
    );
    display.startup_mode_444 = Some(false);

    let deadline = Instant::now() + Duration::from_secs(2);
    while display.encoder.is_none() && Instant::now() < deadline {
        display.sync_encoder_startup();
        std::thread::yield_now();
    }
    assert!(display.encoder.is_some() && !display.encoder_failed);
    assert!(display.encoder_startup.is_none() && display.startup_mode_444.is_none());
    assert!(display.pending_bitmap_updates.is_empty());
    assert!(display.pending_bitmap_frame.is_none() && display.deferred_bitmap_frame.is_none());
    assert!(!display.needs_full_bitmap_seed);
    assert_eq!(display.forced_full_bitmap_updates_remaining, 0);
    assert_eq!(
        display.capture_config.frame_rate,
        display.bitrate_ctrl.current_fps()
    );
    assert_eq!(display.capture_config.frame_rate, 30);
    assert!(display.needs_full_gfx_seed && display.client_may_be_stale);
    assert!(display.settings_refresh_pending);
    assert!(!display.gfx_state.lock().unwrap().encoder_bitmap_fallback);

    let Some(DisplayUpdate::GfxFrame(video)) = display.send_cached_full_frame_recovery() else {
        panic!("Late Ready must supply an AVC IDR with the freshest retained image");
    };
    assert!(video.is_keyframe && video.h264_aux.is_none());
    assert_eq!(video.h264_data[4], 99);
    let deadline = Instant::now() + Duration::from_secs(2);
    while encoder_startup::pending() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn negotiated_chroma_change_discards_old_initializer_without_starting_another_worker() {
    let _guard = encoder_startup::test_guard();
    struct AbandonedEncoder(std::sync::mpsc::Sender<std::thread::ThreadId>);
    impl Drop for AbandonedEncoder {
        fn drop(&mut self) {
            self.0.send(std::thread::current().id()).unwrap();
        }
    }
    impl VideoEncoder for AbandonedEncoder {
        fn backend_name(&self) -> &'static str {
            "abandoned-chroma-test"
        }
        fn encode_bgra(
            &mut self,
            _: &[u8],
            _: u32,
            _: u32,
            _: usize,
        ) -> Result<macrdp_encode::EncodedFrame> {
            panic!("Codec for the previous AVC mode must never be used")
        }
        fn encode_bgra_444(
            &mut self,
            _: &[u8],
            _: u32,
            _: u32,
            _: usize,
        ) -> Result<macrdp_encode::Avc444EncodedFrame> {
            panic!("Codec for the previous AVC mode must never be used")
        }
        fn set_bitrate(&mut self, _: u32) -> Result<()> {
            panic!("Codec for the previous AVC mode must never be adopted")
        }
        fn force_keyframe(&mut self) {
            panic!("Codec for the previous AVC mode must never be adopted")
        }
        fn supports_444(&self) -> bool {
            true
        }
    }

    let mut display = fixture(false);
    display.encoder = None;
    let (resume, blocked) = std::sync::mpsc::channel();
    let (dropped, destruction) = std::sync::mpsc::channel();
    display.encoder_startup = Some(
        encoder_startup::Initializer::start(move || {
            blocked.recv().unwrap();
            Ok(Box::new(AbandonedEncoder(dropped)) as Box<dyn VideoEncoder>)
        })
        .expect("Synthetic initializer admitted"),
    );
    // The pending codec was prepared for AVC444, but current negotiation is
    // AVC420. Cancelling it must not free capacity before its worker returns.
    display.startup_mode_444 = Some(true);
    for _ in 0..10 {
        display.sync_encoder_startup();
        assert!(display.encoder_startup.is_none() && display.startup_mode_444.is_none());
        assert!(display.encoder.is_none() && !display.encoder_failed);
        assert!(!display.encoder_startup_attempted);
        assert!(encoder_startup::pending());
    }
    resume.send(()).unwrap();
    assert_ne!(
        destruction.recv_timeout(Duration::from_secs(2)).unwrap(),
        std::thread::current().id()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while encoder_startup::pending() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(display.encoder.is_none() && display.encoder_startup.is_none());
    assert!(destruction.try_recv().is_err());
}
