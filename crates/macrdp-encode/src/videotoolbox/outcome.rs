//! Native completion, reference repair and bounded per-session diagnostics.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::EncodedFrame;

/// Apple's informational FrameDropped flag is not an encoding error.
pub(super) const FRAME_DROPPED: u32 = 1 << 1;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum NativeOutput {
    Encoded(Vec<u8>, bool),
    Dropped,
    Failed,
}

impl NativeOutput {
    fn is_usable(&self) -> bool {
        matches!(self, Self::Encoded(data, _) if !data.is_empty())
    }

    fn is_idr(&self) -> bool {
        matches!(self, Self::Encoded(data, true) if !data.is_empty())
    }

    fn needs_repair(&self) -> bool {
        matches!(self, Self::Failed) || matches!(self, Self::Encoded(data, _) if data.is_empty())
    }

    pub(super) fn into_frame(self, width: u32, height: u32) -> EncodedFrame {
        let (data, is_keyframe) = match self {
            Self::Encoded(data, keyframe) if !data.is_empty() => (Bytes::from(data), keyframe),
            _ => (Bytes::new(), false),
        };
        // Empty is a COMPLETED no-output sentinel. None remains reserved for
        // an outstanding callback, so a drop cannot leave the display waiting.
        EncodedFrame {
            data,
            is_keyframe,
            width,
            height,
        }
    }
}

#[derive(Default)]
pub(super) struct KeyframeRequests {
    pending: bool,
    in_flight_requested: bool,
}

impl KeyframeRequests {
    pub(super) fn request(&mut self) {
        self.pending = true;
    }

    pub(super) fn pending(&self) -> bool {
        self.pending
    }

    /// Called only after native submission succeeds. A later request remains
    /// independent of the one carried by this sole outstanding input.
    pub(super) fn submitted(&mut self) {
        self.in_flight_requested = std::mem::take(&mut self.pending);
    }

    pub(super) fn complete(&mut self, output: &NativeOutput) {
        self.finish(output.needs_repair(), output.is_idr());
    }

    /// AVC444 views are one delivery unit. If either is absent, an encoded
    /// counterpart must also be discarded and can no longer be a client
    /// reference. Force repair only for that loss, failure or an older request.
    pub(super) fn complete_pair(&mut self, main: &NativeOutput, auxiliary: &NativeOutput) -> bool {
        let usable = main.is_usable() && auxiliary.is_usable();
        let rejected = main.needs_repair()
            || auxiliary.needs_repair()
            || (!usable && (main.is_usable() || auxiliary.is_usable()));
        self.finish(rejected, usable && main.is_idr() && auxiliary.is_idr());
        usable
    }

    fn finish(&mut self, rejected: bool, is_idr: bool) {
        if rejected || (self.in_flight_requested && !is_idr) {
            // Rejected encoded output may already be a native reference. A
            // normal dropped frame has no such output and needs no NEW IDR.
            self.pending = true;
        }
        self.in_flight_requested = false;
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum CallbackEvent {
    Encoded,
    Dropped,
    UnexpectedEmpty,
    Failure,
    ParserPanic,
}

impl CallbackEvent {
    fn index(self) -> usize {
        match self {
            Self::Encoded => 0,
            Self::Dropped => 1,
            Self::UnexpectedEmpty => 2,
            Self::Failure | Self::ParserPanic => 3,
        }
    }
}

const REPORT_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
struct DiagnosticState {
    counts: [u64; 4],
    reported: [u64; 4],
    last_report: [Option<Instant>; 4],
}

#[derive(Debug)]
struct DiagnosticSnapshot {
    counts: [u64; 4],
    suppressed: u64,
}

#[derive(Default)]
pub(super) struct CallbackDiagnostics(Mutex<DiagnosticState>);

impl CallbackDiagnostics {
    #[cfg(test)]
    pub(super) fn counts(&self) -> [u64; 4] {
        self.0.lock().unwrap().counts
    }

    fn observe(&self, event: CallbackEvent, now: Instant) -> Option<DiagnosticSnapshot> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = event.index();
        state.counts[index] = state.counts[index].saturating_add(1);
        if matches!(event, CallbackEvent::Encoded)
            || (matches!(
                event,
                CallbackEvent::Dropped | CallbackEvent::UnexpectedEmpty
            ) && state.last_report[index]
                .is_some_and(|last| now.saturating_duration_since(last) < REPORT_INTERVAL))
        {
            return None;
        }
        let snapshot = DiagnosticSnapshot {
            counts: state.counts,
            suppressed: state.counts[index]
                .saturating_sub(state.reported[index])
                .saturating_sub(1),
        };
        state.reported[index] = state.counts[index];
        state.last_report[index] = Some(now);
        Some(snapshot)
    }

    pub(super) fn record(
        &self,
        event: CallbackEvent,
        status: i32,
        info_flags: u32,
        detail: Option<&dyn std::fmt::Display>,
    ) {
        let Some(snapshot) = self.observe(event, Instant::now()) else {
            return;
        };
        let [encoded, dropped, unexpected_empty, failures] = snapshot.counts;
        let suppressed = snapshot.suppressed;
        match event {
            CallbackEvent::Encoded => {}
            CallbackEvent::Dropped => tracing::info!(status, info_flags, encoded, dropped,
                unexpected_empty, failures, suppressed,
                "VideoToolbox dropped frames normally; retaining the decoder reference chain"),
            CallbackEvent::UnexpectedEmpty => tracing::warn!(status, info_flags, encoded, dropped,
                unexpected_empty, failures, suppressed,
                "VideoToolbox completed without output or an explicit drop flag; requesting reference repair"),
            CallbackEvent::Failure => {
                let detail = detail.map(ToString::to_string).unwrap_or_default();
                tracing::warn!(status, info_flags, encoded, dropped, unexpected_empty, failures,
                    suppressed, detail, "VideoToolbox encoding failed; requesting reference repair");
            }
            CallbackEvent::ParserPanic => tracing::error!(status, info_flags, encoded, dropped,
                unexpected_empty, failures, "VideoToolbox output parser panicked; requesting reference repair"),
        }
    }
}

#[cfg(test)]
mod tests;
