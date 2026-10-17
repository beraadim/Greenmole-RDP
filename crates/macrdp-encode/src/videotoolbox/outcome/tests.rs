use super::*;

fn encoded(keyframe: bool) -> NativeOutput {
    NativeOutput::Encoded(
        vec![0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }],
        keyframe,
    )
}

#[test]
fn normal_drops_do_not_manufacture_keyframe_requests() {
    let mut requests = KeyframeRequests::default();
    for _ in 0..1000 {
        requests.submitted();
        requests.complete(&NativeOutput::Dropped);
        assert!(!requests.pending());
    }
    requests.submitted();
    requests.complete(&encoded(false));
    assert!(!requests.pending());
}

#[test]
fn a_requested_keyframe_survives_drops_and_non_idr_output_until_usable_idr() {
    let mut requests = KeyframeRequests::default();
    requests.request();
    for output in [NativeOutput::Dropped, encoded(false), encoded(true)] {
        assert!(requests.pending());
        requests.submitted();
        requests.complete(&output);
    }
    assert!(!requests.pending());
}

#[test]
fn completion_cannot_consume_a_newer_request_while_its_input_is_pending() {
    let mut requests = KeyframeRequests::default();
    requests.request();
    requests.submitted();
    requests.request();
    requests.complete(&encoded(true));
    assert!(requests.pending());
    requests.submitted();
    requests.complete(&encoded(true));
    assert!(!requests.pending());
}

#[test]
fn failed_or_malformed_output_always_repairs_references() {
    for output in [
        NativeOutput::Failed,
        NativeOutput::Encoded(Vec::new(), true),
    ] {
        let mut requests = KeyframeRequests::default();
        requests.submitted();
        requests.complete(&output);
        assert!(requests.pending());
    }
}

#[test]
fn completed_no_output_has_a_sentinel_distinct_from_pending_collection() {
    for output in [NativeOutput::Dropped, NativeOutput::Failed] {
        let frame = output.into_frame(640, 480);
        assert!(frame.data.is_empty());
        assert!(!frame.is_keyframe);
        assert_eq!((frame.width, frame.height), (640, 480));
    }
    let frame = encoded(true).into_frame(640, 480);
    assert!(!frame.data.is_empty());
    assert!(frame.is_keyframe);
}

#[test]
fn incomplete_avc444_pairs_force_repair_even_when_the_missing_view_was_dropped() {
    for (main, auxiliary) in [
        (encoded(false), NativeOutput::Dropped),
        (NativeOutput::Dropped, encoded(true)),
        (NativeOutput::Failed, NativeOutput::Dropped),
        (
            NativeOutput::Encoded(Vec::new(), false),
            NativeOutput::Dropped,
        ),
    ] {
        let mut requests = KeyframeRequests::default();
        requests.submitted();
        assert!(!requests.complete_pair(&main, &auxiliary));
        assert!(requests.pending());
    }
}

#[test]
fn wholly_dropped_avc444_pairs_only_retain_existing_requests() {
    let mut requests = KeyframeRequests::default();
    requests.submitted();
    assert!(!requests.complete_pair(&NativeOutput::Dropped, &NativeOutput::Dropped));
    assert!(!requests.pending());
    requests.request();
    requests.submitted();
    assert!(!requests.complete_pair(&NativeOutput::Dropped, &NativeOutput::Dropped));
    assert!(requests.pending());
}

#[test]
fn forced_avc444_starts_with_main_idr_and_preserves_newer_requests() {
    let mut requests = KeyframeRequests::default();
    requests.request();
    requests.submitted();
    assert!(!requests.complete_pair(&encoded(false), &encoded(true)));
    assert!(requests.pending());
    requests.submitted();
    assert!(requests.complete_pair(&encoded(true), &encoded(false)));
    assert!(!requests.pending());
    requests.submitted();
    requests.request();
    assert!(requests.complete_pair(&encoded(true), &encoded(true)));
    assert!(requests.pending());
}

#[test]
fn drop_and_ambiguous_empty_reports_are_bounded_but_counters_are_cumulative() {
    let diagnostics = CallbackDiagnostics::default();
    let start = Instant::now();
    for event in [CallbackEvent::Dropped, CallbackEvent::UnexpectedEmpty] {
        assert_eq!(diagnostics.observe(event, start).unwrap().suppressed, 0);
        for _ in 0..1000 {
            assert!(diagnostics
                .observe(event, start + Duration::from_secs(1))
                .is_none());
        }
        let report = diagnostics.observe(event, start + REPORT_INTERVAL).unwrap();
        assert_eq!(report.counts[event.index()], 1002);
        assert_eq!(report.suppressed, 1000);
    }
}

#[test]
fn normal_reports_never_hide_failures_or_parser_panics() {
    let diagnostics = CallbackDiagnostics::default();
    let now = Instant::now();
    assert!(diagnostics.observe(CallbackEvent::Encoded, now).is_none());
    diagnostics.observe(CallbackEvent::Dropped, now).unwrap();
    diagnostics
        .observe(CallbackEvent::UnexpectedEmpty, now)
        .unwrap();
    for event in [
        CallbackEvent::Failure,
        CallbackEvent::Failure,
        CallbackEvent::ParserPanic,
    ] {
        assert!(diagnostics.observe(event, now).is_some());
    }
    assert_eq!(diagnostics.0.lock().unwrap().counts, [1, 1, 1, 3]);
}
