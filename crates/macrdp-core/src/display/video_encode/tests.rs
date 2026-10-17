use super::*;
use bytes::Bytes;
use macrdp_encode::{Avc444EncodedFrame, EncodedFrame};

#[derive(Default)]
struct Encoder {
    single_calls: usize,
    pair_calls: usize,
    keyframe_requests: usize,
    empty_main: bool,
    empty_aux: bool,
    mismatched_aux_size: bool,
    predictive_auxiliary: bool,
    fail: bool,
    drop: bool,
}

fn frame(empty: bool) -> EncodedFrame {
    EncodedFrame {
        data: if empty {
            Bytes::new()
        } else {
            Bytes::from_static(&[0, 0, 1, 0x65])
        },
        is_keyframe: true,
        width: 16,
        height: 16,
    }
}

impl VideoEncoder for Encoder {
    fn backend_name(&self) -> &'static str {
        "injected"
    }
    fn encode_bgra(&mut self, _: &[u8], _: u32, _: u32, _: usize) -> Result<EncodedFrame> {
        self.single_calls += 1;
        Ok(frame(false))
    }
    fn encode_bgra_444(
        &mut self,
        _: &[u8],
        _: u32,
        _: u32,
        _: usize,
    ) -> Result<Avc444EncodedFrame> {
        self.pair_calls += 1;
        if self.drop {
            return Err(macrdp_encode::Avc444EncodeError::Dropped.into());
        }
        anyhow::ensure!(!self.fail, "injected pair submission failure");
        let mut auxiliary = frame(self.empty_aux);
        auxiliary.is_keyframe = !self.predictive_auxiliary;
        if self.mismatched_aux_size {
            auxiliary.width = 32;
        }
        Ok(Avc444EncodedFrame {
            main_view: frame(self.empty_main),
            aux_view: auxiliary,
        })
    }
    fn set_bitrate(&mut self, _: u32) -> Result<()> {
        Ok(())
    }
    fn force_keyframe(&mut self) {
        self.keyframe_requests += 1;
    }
    fn supports_444(&self) -> bool {
        true
    }
}

#[test]
fn missing_either_view_never_advances_the_encoder_through_avc420() {
    for empty_main in [false, true] {
        let mut encoder = Encoder {
            empty_main,
            empty_aux: !empty_main,
            ..Default::default()
        };
        assert!(encode_bgra(&mut encoder, &[0; 1024], 16, 16, 64, true).is_err());
        assert_eq!(
            (
                encoder.pair_calls,
                encoder.single_calls,
                encoder.keyframe_requests
            ),
            (1, 0, 1)
        );
        encoder.empty_main = false;
        encoder.empty_aux = false;
        let recovered = encode_bgra(&mut encoder, &[0; 1024], 16, 16, 64, true).unwrap();
        assert!(recovered.h264_aux.is_some() && recovered.is_keyframe);
        assert_eq!((encoder.pair_calls, encoder.single_calls), (2, 0));
    }
}

#[test]
fn pair_failure_and_dimension_mismatch_request_repair_without_mixing_codecs() {
    for fail in [false, true] {
        let mut encoder = Encoder {
            fail,
            mismatched_aux_size: !fail,
            ..Default::default()
        };
        assert!(encode_bgra(&mut encoder, &[0; 1024], 16, 16, 64, true).is_err());
        assert_eq!((encoder.single_calls, encoder.keyframe_requests), (0, 1));
    }
}

#[test]
fn native_explicit_drop_preserves_native_paired_reference_repair_decision() {
    let mut encoder = Encoder {
        drop: true,
        ..Default::default()
    };
    let error = encode_bgra(&mut encoder, &[0; 1024], 16, 16, 64, true).unwrap_err();
    assert!(is_transient_drop(&error));
    assert_eq!((encoder.single_calls, encoder.keyframe_requests), (0, 0));
}

#[test]
fn avc420_selection_remains_a_single_view() {
    let mut encoder = Encoder::default();
    let output = encode_bgra(&mut encoder, &[0; 1024], 16, 16, 64, false).unwrap();
    assert!(output.h264_aux.is_none());
    assert_eq!((encoder.single_calls, encoder.pair_calls), (1, 0));
}

#[test]
fn transient_failures_have_a_fixed_budget_and_do_not_flood_warnings() {
    let start = Instant::now();
    let mut recovery = Recovery::default();
    for milliseconds in [0, 1, 100, 1000, 4999] {
        let action = recovery.failed(
            start + Duration::from_millis(milliseconds),
            start + Duration::from_millis(milliseconds),
            true,
        );
        assert!(!action.warn && !action.fallback);
    }
    let action = recovery.failed(start + FAILURE_BUDGET, start + FAILURE_BUDGET, true);
    assert!(action.fallback && !action.warn);
    assert_eq!(action.failures, 6);
}

#[test]
fn warnings_are_rate_limited_and_success_starts_a_fresh_failure_budget() {
    let start = Instant::now();
    let mut recovery = Recovery::default();
    assert!(recovery.failed(start, start, false).warn);
    assert!(
        !recovery
            .failed(
                start + Duration::from_secs(1),
                start + Duration::from_secs(1),
                false
            )
            .warn
    );
    assert_eq!(
        recovery.succeeded(start + Duration::from_secs(2)),
        Some((2, Duration::from_secs(2)))
    );
    let action = recovery.failed(
        start + Duration::from_secs(30),
        start + Duration::from_secs(30),
        false,
    );
    assert!(action.warn && !action.fallback);
    assert_eq!(action.failures, 1);
}

fn completed() -> Option<macrdp_encode::EncodeTiming> {
    Some(macrdp_encode::EncodeTiming {
        encode_duration: Duration::from_millis(12),
        completed_at: Instant::now(),
    })
}

#[tokio::test]
async fn pending_views_wake_without_capture_and_keep_original_deadline_and_size() {
    let started_at = Instant::now();
    let pending = Pending::new(started_at, 2560, 1264);
    let (completion, mut receiver) = tokio::sync::watch::channel(None);
    let main = async {
        tokio::task::yield_now().await;
        completion.send_replace(completed());
    };
    let (wake, ()) = tokio::join!(wait_completion(&pending, &mut receiver, None), main);
    assert_eq!(wake, CompletionWake::Ready);
    // Submission of the auxiliary view resets the same completion watch, and
    // must not grant a second five-second deadline or replace visible metadata.
    completion.send_replace(None);
    assert_eq!(
        wait_completion(&pending, &mut receiver, Some(Duration::ZERO)).await,
        CompletionWake::Cursor
    );
    completion.send_replace(completed());
    assert_eq!(
        wait_completion(&pending, &mut receiver, None).await,
        CompletionWake::Ready
    );
    assert_eq!(
        (pending.started_at, pending.width, pending.height),
        (started_at, 2560, 1264)
    );
    assert_eq!(
        pending.remaining(started_at + Duration::from_secs(4)),
        Duration::from_secs(1)
    );
    assert_eq!(
        pending.remaining(started_at + Duration::from_secs(5)),
        Duration::ZERO
    );
}

#[tokio::test]
async fn cancelled_completion_wait_keeps_pair_owned_for_resume_or_explicit_control_retirement() {
    let pending = Pending::new(Instant::now(), 16, 16);
    let (completion, mut receiver) = tokio::sync::watch::channel(None);
    // The outer display select can cancel on a settings/resize request. Waiting
    // borrows the pair and signal; cancellation consumes neither native owner.
    assert!(tokio::time::timeout(
        Duration::from_millis(2),
        wait_completion(&pending, &mut receiver, None)
    )
    .await
    .is_err());
    completion.send_replace(completed());
    assert_eq!(
        wait_completion(&pending, &mut receiver, None).await,
        CompletionWake::Ready
    );
    let pending = Pending::new(Instant::now() - FAILURE_BUDGET, 16, 16);
    // An already-present late callback cannot extend or bypass the deadline.
    assert_eq!(
        wait_completion(&pending, &mut receiver, None).await,
        CompletionWake::Timeout
    );
}

#[tokio::test]
async fn closed_native_completion_channel_is_a_failure_without_an_extra_capture() {
    let pending = Pending::new(Instant::now(), 16, 16);
    let (completion, mut receiver) = tokio::sync::watch::channel(None);
    drop(completion);
    assert_eq!(
        wait_completion(&pending, &mut receiver, None).await,
        CompletionWake::Closed
    );
}

#[test]
fn main_idr_with_predictive_auxiliary_is_a_self_contained_pair() {
    let mut encoder = Encoder {
        predictive_auxiliary: true,
        ..Default::default()
    };
    let output = encode_bgra(&mut encoder, &[0; 1024], 16, 16, 64, true).unwrap();
    assert!(output.is_keyframe && output.h264_aux.is_some());
    assert_eq!(encoder.single_calls, 0);
}

#[tokio::test]
async fn a_slow_drop_and_later_pending_pair_share_the_first_unsent_submission_budget() {
    let second_started = Instant::now();
    let first_started = second_started - FAILURE_BUDGET + Duration::from_millis(30);
    let mut recovery = Recovery::default();
    // The first pair spends almost its entire budget in native code before
    // completing as a drop. Its completion cannot start a new five-second wait.
    let dropped = recovery.failed(first_started, second_started, true);
    assert_eq!(dropped.elapsed, FAILURE_BUDGET - Duration::from_millis(30));
    assert!(!dropped.fallback);
    let mut pending = Pending::new(second_started, 2560, 1264);
    pending.limit_deadline(recovery.deadline());
    assert_eq!(pending.remaining(second_started), Duration::from_millis(30));
    assert_eq!(pending.started_at, second_started);
    assert_eq!((pending.width, pending.height), (2560, 1264));
    let (completion, mut receiver) = tokio::sync::watch::channel(None);
    // A permanently pending retry must time out under the old deadline, well
    // before the second pair's own five-second budget could expire.
    let wake = tokio::time::timeout(
        Duration::from_millis(500),
        wait_completion(&pending, &mut receiver, None),
    )
    .await
    .unwrap();
    assert_eq!(wake, CompletionWake::Timeout);
    // Even an already-available late auxiliary callback cannot bypass it.
    completion.send_replace(completed());
    assert_eq!(
        wait_completion(&pending, &mut receiver, None).await,
        CompletionWake::Timeout
    );
    assert!(
        recovery
            .failed(second_started, first_started + FAILURE_BUDGET, true)
            .fallback
    );
}

mod fresh_after_slow_encode;
