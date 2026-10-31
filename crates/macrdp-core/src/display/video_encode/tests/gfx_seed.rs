//! Real display dispatch with synthetic pixels and a codec that records IDR
//! requests. No native capture, desktop, input, codec or connection is used.

use super::fresh_after_slow_encode::{capture, fixture};
use crate::display::*;
use macrdp_encode::{Avc444EncodedFrame, EncodedFrame};

#[derive(Default)]
struct EncoderCalls {
    encoded: usize,
    keyframe_requests: usize,
    force_pending: bool,
}

struct KeyframeEncoder(Arc<Mutex<EncoderCalls>>);
impl VideoEncoder for KeyframeEncoder {
    fn backend_name(&self) -> &'static str {
        "synthetic-keyframe-test"
    }
    fn encode_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        _: usize,
    ) -> Result<EncodedFrame> {
        let mut calls = self.0.lock().unwrap();
        calls.encoded += 1;
        let keyframe = std::mem::take(&mut calls.force_pending);
        Ok(EncodedFrame {
            data: Bytes::from(vec![0, 0, 1, if keyframe { 0x65 } else { 0x41 }, data[0]]),
            is_keyframe: keyframe,
            width,
            height,
        })
    }
    fn encode_bgra_444(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Avc444EncodedFrame> {
        let main = self.encode_bgra(data, width, height, stride)?;
        let aux = EncodedFrame {
            data: main.data.clone(),
            is_keyframe: main.is_keyframe,
            width,
            height,
        };
        Ok(Avc444EncodedFrame {
            main_view: main,
            aux_view: aux,
        })
    }
    fn set_bitrate(&mut self, _: u32) -> Result<()> {
        Ok(())
    }
    fn force_keyframe(&mut self) {
        let mut calls = self.0.lock().unwrap();
        calls.keyframe_requests += 1;
        calls.force_pending = true;
    }
    fn supports_444(&self) -> bool {
        true
    }
    fn supports_pixel_buffer(&self) -> bool {
        true
    }
}

fn windows_display() -> (MacDisplayUpdates, Arc<Mutex<EncoderCalls>>) {
    let mut display = fixture(false);
    let calls = Arc::new(Mutex::new(EncoderCalls::default()));
    display.encoder = Some(Box::new(KeyframeEncoder(calls.clone())));
    display.needs_full_gfx_seed = true;
    display.client_may_be_stale = true;
    {
        let mut gfx = display.gfx_state.lock().unwrap();
        gfx.browser_avc420 = false;
        gfx.browser_avc444 = false;
    }
    (display, calls)
}

fn assert_full_seed(update: Option<DisplayUpdate>, width: u16, height: u16, value: u8) {
    let Some(DisplayUpdate::GfxUncompressed(update)) = update else {
        panic!("Windows activation must prepare a complete raw GFX baseline before video");
    };
    assert_eq!((update.width, update.height), (width, height));
    assert_eq!(update.rects.len(), 1);
    let rect = &update.rects[0];
    assert_eq!(
        (rect.x, rect.y, rect.width, rect.height),
        (0, 0, width, height)
    );
    assert_eq!(
        rect.pixel_data.len(),
        usize::from(width) * usize::from(height) * 4
    );
    assert!(rect.pixel_data.iter().all(|byte| *byte == value));
}

#[test]
fn cached_windows_startup_prepares_full_1920x1200_rgb_then_matching_video_idr() {
    let (mut display, calls) = windows_display();
    let mut source = capture(42);
    source.width = 1920;
    source.height = 1200;
    source.stride = 1920 * 4;
    source.data = FrameData::Raw(Bytes::from(vec![42; 1920 * 1200 * 4]));
    display.cache_latest_frame(&source);
    assert_full_seed(display.send_cached_full_frame_recovery(), 1920, 1200, 42);
    assert!(!display.needs_full_gfx_seed && display.raw_gfx_baseline_prepared);
    assert!(!display.client_may_be_stale);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.encoded, 0,
            "raw seed cannot consume the prepared video IDR"
        );
        assert!(calls.force_pending);
    }
    let Some(DisplayUpdate::GfxFrame(video)) = display.send_cached_full_frame_recovery() else {
        panic!("the first video update after RGB must be an IDR");
    };
    assert_eq!(
        (video.width, video.height, video.enc_width, video.enc_height),
        (1920, 1200, 1920, 1200)
    );
    assert!(video.is_keyframe);
    assert_eq!(video.h264_data[3], 0x65);
    assert_eq!(calls.lock().unwrap().encoded, 1);
}

#[test]
fn fresh_windows_frame_uses_the_same_complete_seed_even_when_damage_is_unchanged() {
    let (mut display, calls) = windows_display();
    assert_full_seed(
        display
            .encode_and_send_with_damage(capture(99), FrameDamage::Unchanged)
            .unwrap(),
        16,
        16,
        99,
    );
    assert_eq!(calls.lock().unwrap().encoded, 0);
    assert!(display.raw_gfx_baseline_prepared && !display.needs_full_gfx_seed);
}

#[test]
fn browser_avc420_and_avc444_cached_startup_keep_their_first_idr_without_raw_seed() {
    for dual in [false, true] {
        let mut display = fixture(dual);
        let calls = Arc::new(Mutex::new(EncoderCalls::default()));
        display.encoder = Some(Box::new(KeyframeEncoder(calls.clone())));
        display.needs_full_gfx_seed = true;
        display.client_may_be_stale = true;
        display.cache_latest_frame(&capture(7));
        let Some(DisplayUpdate::GfxFrame(video)) = display.send_cached_full_frame_recovery() else {
            panic!("browser video must remain an uninterrupted AVC stream");
        };
        assert!(video.is_keyframe);
        assert_eq!(video.h264_aux.is_some(), dual);
        assert_eq!(calls.lock().unwrap().encoded, 1);
        assert!(!display.raw_gfx_baseline_prepared && !display.needs_full_gfx_seed);
    }
}

#[test]
fn unavailable_or_invalid_raw_baseline_never_falls_through_to_video_or_clears_seed() {
    let (mut display, calls) = windows_display();
    assert!(display.send_cached_full_frame_recovery().is_none());
    let mut invalid = capture(7);
    invalid.data = FrameData::Raw(Bytes::from_static(&[7; 31]));
    display.cache_latest_frame(&invalid);
    assert!(display.send_cached_full_frame_recovery().is_none());
    assert!(display.needs_full_gfx_seed && display.client_may_be_stale);
    assert!(!display.raw_gfx_baseline_prepared);
    assert_eq!(calls.lock().unwrap().encoded, 0);

    display.cache_latest_frame(&capture(42));
    assert_full_seed(display.send_cached_full_frame_recovery(), 16, 16, 42);
    assert!(!display.needs_full_gfx_seed && display.raw_gfx_baseline_prepared);
}

#[test]
fn pending_video_completion_cannot_be_overtaken_by_a_new_raw_seed() {
    let (mut display, calls) = windows_display();
    display.cache_latest_frame(&capture(42));
    display.has_pending_submit = true;
    assert!(display.send_cached_full_frame_recovery().is_none());
    assert!(display.needs_full_gfx_seed && !display.raw_gfx_baseline_prepared);
    assert_eq!(calls.lock().unwrap().encoded, 0);
}

#[test]
fn nv12_recovery_wakes_do_not_restart_an_already_prepared_rgb_baseline() {
    let (mut display, _) = windows_display();
    display.gfx_state.lock().unwrap().last_ack_frame = 1;
    assert_eq!(
        display.desired_capture_pixel_format(),
        CapturePixelFormat::Bgra
    );

    display.cache_latest_frame(&capture(42));
    assert_full_seed(display.send_cached_full_frame_recovery(), 16, 16, 42);
    display.last_frame_cache = None;
    display.capture_config.pixel_format = CapturePixelFormat::Nv12;
    // Native capture replacement requests a video baseline before a new
    // source image exists. Simulate multiple cursor/pacing wakes in that gap.
    display.needs_full_gfx_seed = true;
    for _ in 0..3 {
        assert_eq!(
            display.desired_capture_pixel_format(),
            CapturePixelFormat::Nv12
        );
    }
    assert!(display.raw_gfx_baseline_prepared && display.needs_full_gfx_seed);

    // Browser video never acquires an RGB baseline; its pending IDR must also
    // remain on the opaque video path once normal ACK readiness is satisfied.
    display.raw_gfx_baseline_prepared = false;
    display.gfx_state.lock().unwrap().browser_avc420 = true;
    assert_eq!(
        display.desired_capture_pixel_format(),
        CapturePixelFormat::Nv12
    );
    assert!(!display.raw_gfx_baseline_prepared && display.needs_full_gfx_seed);
}

#[test]
fn cached_manual_bitmap_lossless_and_unavailable_video_paths_remain_compressed_bitmaps() {
    for kind in 0..3 {
        let (mut display, calls) = windows_display();
        match kind {
            0 => display.gfx_state.lock().unwrap().manual_bitmap_mode = true,
            1 => {
                display.quality = Quality::Lossless;
                display.gfx_state.lock().unwrap().target_quality = Some("lossless".into());
            }
            _ => display.encoder = None,
        }
        display.cache_latest_frame(&capture(42));
        assert!(matches!(
            display.send_cached_full_frame_recovery(),
            Some(DisplayUpdate::Bitmap(_))
        ));
        assert_eq!(calls.lock().unwrap().encoded, 0);
        assert!(!display.raw_gfx_baseline_prepared);
    }
}
