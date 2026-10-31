use super::*;

#[test]
fn validates_visible_layout_with_checked_arithmetic() {
    assert!(sample_bgra(&[], 0, 1, 0).is_none());
    assert!(sample_bgra(&[], 1, 0, 4).is_none());
    assert!(sample_bgra(&[0; 8], 2, 1, 7).is_none());
    assert!(sample_bgra(&[0; 7], 2, 1, 8).is_none());
    assert!(sample_bgra(&[0; 8], 1, 2, 8).is_none());
    assert!(sample_bgra(&[], u32::MAX, u32::MAX, usize::MAX).is_none());
    assert!(sample_bgra(&[], 1, u32::MAX, usize::MAX).is_none());

    // Last-row padding need not be present; only visible bytes are required.
    let sample = sample_bgra(&[0; 12], 1, 2, 8).unwrap();
    assert_eq!(sample.sampled_pixels, 2);
    assert_eq!(sample.near_black_pixels, 2);
}

#[test]
fn ignores_padding_and_alpha_while_classifying_visible_brightness() {
    let pixels = [
        0, 0, 0, 255, 4, 4, 4, 0, 255, 255, 255, 255, 5, 5, 5, 0, 255, 255, 255, 0,
    ];
    // Two rows, two pixels each, with one bright padding pixel after row one.
    let sample = sample_bgra(&pixels, 2, 2, 12).unwrap();
    assert_eq!(sample.sampled_pixels, 4);
    assert_eq!(sample.near_black_pixels, 2);
    assert_eq!(sample.luma_min, 0);
    assert_eq!(sample.luma_max, 255);
    assert_eq!(sample.luma_sum, 264);
    let summary = LogSummary::from(Some(sample));
    assert!(summary.available);
    assert_eq!(summary.luma_mean, 66.0);
}

#[test]
fn samples_at_most_256_pixels_even_for_large_frames() {
    let pixels = [200, 200, 200, 0].repeat(512 * 256);
    let sample = sample_bgra(&pixels, 512, 256, 512 * 4).unwrap();
    assert_eq!(sample.sampled_pixels, 256);
    assert_eq!(sample.near_black_pixels, 0);
    assert_eq!(sample.luma_min, 200);
    assert_eq!(sample.luma_max, 200);
    assert_eq!(sample.luma_sum, 256 * 200);
}

#[test]
fn small_and_uneven_dimensions_remain_bounded_and_cover_the_frame() {
    for (width, height) in [(1, 1), (1, 19), (19, 1), (17, 29), (3, 7)] {
        let pixels = [255, 255, 255, 0].repeat((width * height) as usize);
        let sample = sample_bgra(&pixels, width, height, width as usize * 4).unwrap();
        assert_eq!(
            sample.sampled_pixels,
            (width.min(16) * height.min(16)) as u16
        );
        assert_eq!(sample.near_black_pixels, 0);
        assert_eq!(sample.luma_sum, u32::from(sample.sampled_pixels) * 255);
    }
}

#[test]
fn opaque_or_unavailable_pixels_do_not_report_an_observed_black_image() {
    // NV12 and failed BGRA locks both map to None, without copying/conversion.
    let summary = LogSummary::from(None);
    assert!(!summary.available);
    assert_eq!(summary.sampled_pixels, 0);
    assert_eq!(summary.near_black_pixels, 0);
    assert_eq!(summary.luma_min, -1);
    assert_eq!(summary.luma_max, -1);
    assert_eq!(summary.luma_mean, -1.0);
}

#[test]
fn startup_observations_are_rate_limited_and_expire() {
    let now = Instant::now();
    let mut health = FrameHealth::new(now);
    assert_eq!(health.take_observation(now), Some(("baseline", 0)));
    for second in 1..=5 {
        assert!(health
            .take_observation(now + Duration::from_millis(second * 1_000 - 1))
            .is_none());
        assert_eq!(
            health.take_observation(now + Duration::from_secs(second)),
            Some(("progress", second * 1_000))
        );
    }
    assert!(health
        .take_observation(now + Duration::from_secs(6))
        .is_none());
    assert!(health
        .take_observation(now + Duration::from_secs(6_000))
        .is_none());
    assert!(health.take_observation(now).is_none());
}

#[test]
fn delayed_first_frame_has_one_baseline_and_no_late_progress() {
    let now = Instant::now();
    let mut health = FrameHealth::new(now);
    assert_eq!(
        health.take_observation(now + Duration::from_secs(10)),
        Some(("baseline", 10_000))
    );
    assert!(health
        .take_observation(now + Duration::from_secs(11))
        .is_none());
}

#[test]
fn default_logging_preserves_only_explicit_aggregate_fields() {
    use std::collections::BTreeSet;
    use std::fmt;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{EnvFilter, Layer};

    struct Fields(Vec<(String, String)>);
    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.push((field.name().into(), format!("{value:?}")));
        }
    }
    struct Capture(Arc<Mutex<Vec<Fields>>>);
    impl<S: Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields);
        }
    }
    let records = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry()
        .with(crate::with_transition_diagnostics(EnvFilter::new("warn")))
        .with(Capture(records.clone()));
    let frame = CapturedFrame {
        width: 4,
        height: 1,
        data: FrameData::Raw(bytes::Bytes::from_static(b"private-canary!!")),
        stride: 16,
        timestamp_us: 9_876_543_210,
        dirty_rects: vec![macrdp_capture::Rect {
            x: 55_443_322,
            y: 44_332_211,
            width: 1,
            height: 1,
        }],
        content_rect: None,
    };
    let now = Instant::now();
    let mut health = FrameHealth::new(now);
    tracing::subscriber::with_default(subscriber, || {
        health.observe(&frame, 31, now);
        health.observe(&frame, 31, now + Duration::from_millis(500));
        health.observe(&frame, 31, now + Duration::from_secs(1));
        health.observe(&frame, 31, now + Duration::from_secs(6));
    });
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 2);
    let expected: BTreeSet<_> = [
        "message",
        "component",
        "phase",
        "state",
        "worker_pid",
        "display_id",
        "elapsed_ms",
        "width",
        "height",
        "source_format",
        "sample_available",
        "sampled_pixels",
        "near_black_pixels",
        "near_black_luma_max",
        "luma_min",
        "luma_max",
        "luma_mean",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for record in records.iter() {
        assert_eq!(
            record
                .0
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<BTreeSet<_>>(),
            expected
        );
        let text = format!("{:?}", record.0);
        assert!(!text.contains("private-canary"));
        assert!(!text.contains("55443322"));
        assert!(!text.contains("44332211"));
        assert!(!text.contains("9876543210"));
    }
    assert!(records[0]
        .0
        .contains(&("phase".into(), "\"baseline\"".into())));
    assert!(records[1]
        .0
        .contains(&("phase".into(), "\"progress\"".into())));
}
