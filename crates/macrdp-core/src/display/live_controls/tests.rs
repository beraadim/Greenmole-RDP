use super::*;
use bytes::Bytes;
use ironrdp_server::gfx::GfxState;
use macrdp_capture::{CapturedFrame, FrameData};
use macrdp_encode::{Avc444EncodedFrame, EncodedFrame, VideoEncoder};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn quality_and_bitmap_choice_never_override_capabilities_or_failure_flags() {
    let mut gfx = GfxState::new(16, 16, false, false);
    gfx.session_uses_gfx = true;
    gfx.channel_id = Some(1);
    gfx.caps_confirmed = true;
    gfx.avc420_supported = true;
    for quality in [Quality::LowLatency, Quality::Balanced, Quality::HighQuality] {
        assert!(encoder_lifecycle::accepts_avc(
            &gfx,
            quality,
            EncoderPreference::Hardware
        ));
        assert!(!encoder_lifecycle::accepts_avc(
            &gfx,
            quality,
            EncoderPreference::Bitmap
        ));
    }
    assert!(!encoder_lifecycle::accepts_avc(
        &gfx,
        Quality::Lossless,
        EncoderPreference::Hardware
    ));
    gfx.manual_bitmap_mode = true;
    assert!(!encoder_lifecycle::accepts_avc(
        &gfx,
        Quality::Balanced,
        EncoderPreference::Hardware
    ));
    gfx.bitmap_fallback_mode = true;
    gfx.manual_bitmap_mode = false;
    assert!(!encoder_lifecycle::accepts_avc(
        &gfx,
        Quality::HighQuality,
        EncoderPreference::Hardware
    ));
    gfx.bitmap_fallback_mode = false;
    gfx.avc_fallback_to_uncompressed = true;
    assert!(!encoder_lifecycle::accepts_avc(
        &gfx,
        Quality::HighQuality,
        EncoderPreference::Hardware
    ));
    gfx.avc_fallback_to_uncompressed = false;
    gfx.avc420_supported = false;
    assert!(!encoder_lifecycle::accepts_avc(
        &gfx,
        Quality::HighQuality,
        EncoderPreference::Hardware
    ));
}

#[test]
fn rapid_video_policy_changes_keep_one_bounded_hold_and_explicit_bitmap_ends_it() {
    let now = Instant::now();
    let deadline = rebuild_deadline(None, true, true, now);
    assert_eq!(deadline, Some(now + Duration::from_secs(5)));
    for second in 1..8 {
        assert_eq!(
            rebuild_deadline(deadline, false, true, now + Duration::from_secs(second)),
            deadline
        );
    }
    assert_eq!(rebuild_deadline(deadline, false, false, now), None);
    assert_eq!(rebuild_deadline(None, false, true, now), None);
}

#[test]
fn abandoned_queued_bitmap_geometry_is_replaced_by_latest_full_frame_pixels() {
    let frame = |value| CapturedFrame {
        width: 2,
        height: 1,
        stride: 8,
        timestamp_us: value as u64,
        content_rect: None,
        data: FrameData::Raw(Bytes::from(vec![value; 8])),
        dirty_rects: vec![],
    };
    let mut regions = std::collections::VecDeque::from([super::super::bitmap_updates::Region {
        bounds: super::super::ClampedRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        dirty_rects: vec![],
    }]);
    let mut old = Some(frame(1));
    let mut latest = Some(frame(2));
    let baseline = discard_bitmap_batch(&mut regions, &mut old, &mut latest).unwrap();
    assert!(regions.is_empty() && old.is_none() && latest.is_none());
    let (mut full, emitted_full) = super::super::bitmap_updates::plan(&baseline, true);
    assert!(emitted_full);
    let full =
        super::super::bitmap_updates::materialize(full.pop_front().unwrap(), &baseline, false)
            .unwrap();
    assert_eq!((full.update.width.get(), full.update.height.get()), (2, 1));
    assert_eq!(full.update.data.as_ref(), &[2; 8]);
}

struct PendingEncoder(
    Arc<AtomicUsize>,
    Option<std::sync::mpsc::Sender<std::thread::ThreadId>>,
);
impl Drop for PendingEncoder {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
        if let Some(sender) = self.1.take() {
            sender.send(std::thread::current().id()).unwrap();
        }
    }
}
impl VideoEncoder for PendingEncoder {
    fn backend_name(&self) -> &'static str {
        "test-pending"
    }
    fn encode_bgra(&mut self, _: &[u8], _: u32, _: u32, _: usize) -> anyhow::Result<EncodedFrame> {
        unreachable!()
    }
    fn encode_bgra_444(
        &mut self,
        _: &[u8],
        _: u32,
        _: u32,
        _: usize,
    ) -> anyhow::Result<Avc444EncodedFrame> {
        unreachable!()
    }
    fn set_bitrate(&mut self, _: u32) -> anyhow::Result<()> {
        Ok(())
    }
    fn force_keyframe(&mut self) {}
    fn supports_444(&self) -> bool {
        false
    }
    fn collect_encoded(&mut self, _: Duration) -> anyhow::Result<Option<EncodedFrame>> {
        panic!("Old pending output must not be collected by the new policy")
    }
}

#[test]
fn replacing_a_codec_abandons_its_pending_output_without_collecting_it() {
    let drops = Arc::new(AtomicUsize::new(0));
    let mut encoder: Option<Box<dyn VideoEncoder>> =
        Some(Box::new(PendingEncoder(drops.clone(), None)));
    let mut startup = None;
    let mut pending = true;
    let mut size = Some((16, 16));
    let mut at = Some(Instant::now());
    let mut avc444 = Some(super::super::video_encode::Pending::new(
        Instant::now(),
        16,
        16,
    ));
    abandon_video(
        &mut encoder,
        &mut startup,
        &mut pending,
        &mut size,
        &mut at,
        &mut avc444,
    );
    assert!(avc444.is_none());
    assert!(encoder.is_none() && startup.is_none() && !pending && size.is_none() && at.is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn live_change_cancels_startup_without_adopting_late_codec_or_accumulating_workers() {
    let (resume, blocked) = std::sync::mpsc::channel();
    let (dropped, destruction) = std::sync::mpsc::channel();
    let drops = Arc::new(AtomicUsize::new(0));
    let worker_drops = drops.clone();
    let mut startup = Some(
        encoder_startup::Initializer::start(move || {
            blocked.recv().unwrap();
            Ok(Box::new(PendingEncoder(worker_drops, Some(dropped))) as Box<dyn VideoEncoder>)
        })
        .unwrap(),
    );
    let mut encoder = None;
    let (mut pending, mut size, mut at) = (false, None, None);
    let mut avc444 = Some(super::super::video_encode::Pending::new(
        Instant::now(),
        16,
        16,
    ));
    abandon_video(
        &mut encoder,
        &mut startup,
        &mut pending,
        &mut size,
        &mut at,
        &mut avc444,
    );
    assert!(avc444.is_none());
    assert!(startup.is_none() && encoder.is_none());
    for _ in 0..10 {
        assert!(
            encoder_startup::Initializer::<Box<dyn VideoEncoder>>::start(|| panic!(
                "Stuck initializer must retain the one admission slot"
            ))
            .is_err()
        );
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
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(encoder.is_none());
}

#[test]
fn settings_keep_quality_identity_separate_from_encoder_preference() {
    for (text, quality) in [
        ("low_latency", Quality::LowLatency),
        ("balanced", Quality::Balanced),
        ("high_quality", Quality::HighQuality),
        ("lossless", Quality::Lossless),
    ] {
        assert_eq!(quality_from_setting(Some(text), Quality::Balanced), quality);
    }
    assert_eq!(
        EncoderPreference::from_str_opt(Some("bitmap")),
        EncoderPreference::Bitmap
    );
    assert_eq!(
        quality_from_setting(None, Quality::HighQuality),
        Quality::HighQuality
    );
}
