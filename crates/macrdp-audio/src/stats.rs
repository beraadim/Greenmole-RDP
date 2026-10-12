use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ironrdp_server::AudioQueueStats;
use macrdp_capture::AudioCaptureStats;

/// Counters for one server lifetime. Audio payloads and client identifiers are
/// never retained; snapshots are safe to include in diagnostics exports.
#[derive(Default)]
pub struct AudioStats {
    pub(crate) capture: Arc<AudioCaptureStats>,
    pub(crate) transport: Mutex<Option<Arc<AudioQueueStats>>>,
    pub(crate) dropped: AtomicU64,
    pub(crate) stale: AtomicU64,
    pub(crate) discontinuities: AtomicU64,
    pub(crate) invalid: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStatsSnapshot {
    pub dropped_frames: u64,
    pub stale_frames: u64,
    pub discontinuities: u64,
    pub invalid_frames: u64,
    pub queue_age_ms: f64,
    pub queued_ms: f64,
}

impl AudioStats {
    pub fn snapshot(&self) -> AudioStatsSnapshot {
        let capture = self.capture.snapshot();
        let transport = self
            .transport
            .lock()
            .unwrap()
            .as_ref()
            .map(|stats| stats.snapshot())
            .unwrap_or_default();
        AudioStatsSnapshot {
            dropped_frames: capture.dropped_frames
                + transport.dropped_frames
                + self.dropped.load(Ordering::Relaxed),
            stale_frames: capture.stale_frames
                + transport.stale_frames
                + self.stale.load(Ordering::Relaxed),
            discontinuities: self.discontinuities.load(Ordering::Relaxed),
            invalid_frames: capture.invalid_frames + self.invalid.load(Ordering::Relaxed),
            queue_age_ms: capture.queue_age_ms.max(transport.queue_age_ms),
            queued_ms: capture.queued_ms + transport.queued_ms,
        }
    }
}
