//! Cheap, process-local progress for native calls that cannot be cancelled.
//!
//! A deadline is evidence for the supervising process, never permission to
//! free a stream or its callbacks. Only process termination can safely recover
//! an indefinitely blocked native owner. No capture, input, or clipboard data
//! is retained here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const SLOW_OPERATION: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub owner_pid: u32,
    pub operation_id: u64,
    pub operation: Option<String>,
    pub elapsed_ms: u64,
    pub pending: bool,
    pub stop_requested: bool,
    pub cleanup_pending: bool,
}

impl Snapshot {
    pub fn is_stalled(&self) -> bool {
        self.pending && self.elapsed_ms >= STALL_TIMEOUT.as_millis() as u64
    }

    pub fn reason(&self) -> Option<String> {
        self.operation.as_ref().map(|operation| {
            format!(
                "Native operation {operation} pending for {} ms (stop requested: {}, cleanup pending: {})",
                self.elapsed_ms, self.stop_requested, self.cleanup_pending
            )
        })
    }
}

#[derive(Default)]
struct State {
    phases: Vec<Phase>,
    stop_requested: bool,
    cleanup_pending: bool,
}

#[derive(Clone)]
struct Phase {
    id: u64,
    name: &'static str,
    started_at: Instant,
    thread_id: std::thread::ThreadId,
}

type SharedState = Arc<Mutex<State>>;

fn registry() -> &'static Mutex<Vec<Weak<Mutex<State>>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<Mutex<State>>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// A native owner has one short-held diagnostic lock. It never holds this
/// lock while entering Apple APIs, waiting on workers, or restoring a display.
#[derive(Clone)]
pub(crate) struct Tracker(SharedState);

impl Default for Tracker {
    fn default() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let mut registry = registry().lock().unwrap_or_else(|error| error.into_inner());
        registry.retain(|entry| entry.strong_count() != 0);
        registry.push(Arc::downgrade(&state));
        Self(state)
    }
}

impl Tracker {
    pub(crate) fn phase(&self, name: &'static str) -> Operation {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let started_at = Instant::now();
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .phases
            .push(Phase {
                id,
                name,
                started_at,
                thread_id: std::thread::current().id(),
            });
        Operation {
            tracker: self.clone(),
            id,
            name,
            started_at,
        }
    }

    pub(crate) fn request_stop(&self) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stop_requested = true;
    }

    pub(crate) fn set_cleanup_pending(&self, pending: bool) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cleanup_pending = pending;
    }

    pub(crate) fn cleanup_finished(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.cleanup_pending = false;
        state.stop_requested = false;
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Snapshot {
        snapshot_states([self.0.clone()], Instant::now())
    }

    #[cfg(test)]
    pub(crate) fn snapshot_at(&self, now: Instant) -> Snapshot {
        snapshot_states([self.0.clone()], now)
    }
}

pub(crate) struct Operation {
    tracker: Tracker,
    id: u64,
    name: &'static str,
    started_at: Instant,
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.tracker
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .phases
            .retain(|phase| phase.id != self.id);
        let elapsed = self.started_at.elapsed();
        if elapsed >= SLOW_OPERATION {
            tracing::warn!(
                operation = self.name,
                operation_id = self.id,
                elapsed_ms = elapsed.as_millis() as u64,
                "Slow native operation completed; ownership remained retained until completion"
            );
        }
    }
}

/// Safe to poll from a dedicated heartbeat thread while a native worker is
/// blocked. Sleeping capture and an unchanged desktop have no active phase.
pub fn snapshot() -> Snapshot {
    let states: Vec<_> = registry()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    snapshot_states(states, Instant::now())
}

fn snapshot_states(states: impl IntoIterator<Item = SharedState>, now: Instant) -> Snapshot {
    let mut result = Snapshot {
        owner_pid: std::process::id(),
        ..Snapshot::default()
    };
    let mut threads: HashMap<std::thread::ThreadId, Phase> = HashMap::new();
    for shared in states {
        let state = shared.lock().unwrap_or_else(|error| error.into_inner());
        result.stop_requested |= state.stop_requested;
        result.cleanup_pending |= state.cleanup_pending;
        // The most specific phase on a thread wins, including a lease's
        // restoration nested inside capture-owner release on that same thread.
        if let Some(phase) = state.phases.last() {
            let previous = threads
                .entry(phase.thread_id)
                .or_insert_with(|| phase.clone());
            if phase.id > previous.id {
                *previous = phase.clone();
            }
        }
    }
    // Across native threads select the oldest call. An unrelated progressing
    // operation must never hide a stuck older owner on another thread.
    for phase in threads.into_values() {
        let elapsed_ms = now
            .saturating_duration_since(phase.started_at)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        if !result.pending || elapsed_ms > result.elapsed_ms {
            result.operation_id = phase.id;
            result.operation = Some(phase.name.to_owned());
            result.elapsed_ms = elapsed_ms;
            result.pending = true;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_desktop_has_no_native_deadline() {
        let tracker = Tracker::default();
        let state = snapshot_states(
            [tracker.0.clone()],
            Instant::now() + Duration::from_secs(600),
        );
        assert!(!state.pending);
        assert!(!state.is_stalled());
    }

    #[test]
    fn nested_phase_returns_to_parent_then_clears_after_completion() {
        let tracker = Tracker::default();
        let parent = tracker.phase("replace");
        let stop = tracker.phase("replace_stop");
        assert_eq!(
            tracker.snapshot().operation.as_deref(),
            Some("replace_stop")
        );
        drop(stop);
        let start = tracker.phase("replace_start");
        assert_eq!(
            tracker.snapshot().operation.as_deref(),
            Some("replace_start")
        );
        drop(start);
        assert_eq!(tracker.snapshot().operation.as_deref(), Some("replace"));
        drop(parent);
        assert!(!tracker.snapshot().pending);
        tracker.request_stop();
        tracker.set_cleanup_pending(true);
        tracker.cleanup_finished();
        let _reconnected = tracker.phase("start");
        let reconnected = tracker.snapshot();
        assert!(!reconnected.stop_requested && !reconnected.cleanup_pending);
    }

    #[test]
    fn deadlines_use_monotonic_age_and_cannot_be_hidden_by_a_new_owner() {
        let old = Tracker::default();
        let operation = old.phase("set_frame_rate");
        let later = Tracker::default();
        let other_thread = later.clone();
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _later = other_thread.phase("start");
            entered.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        old.request_stop();
        old.set_cleanup_pending(true);
        let state = snapshot_states(
            [old.0.clone(), later.0.clone()],
            operation.started_at + STALL_TIMEOUT,
        );
        assert!(state.is_stalled());
        assert_eq!(state.operation.as_deref(), Some("set_frame_rate"));
        assert!(state.stop_requested && state.cleanup_pending);
        assert!(state.reason().unwrap().contains("30000 ms"));
        let json = serde_json::to_string(&state).unwrap();
        let decoded: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.operation_id, state.operation_id);
        assert!(decoded.is_stalled());
        release.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn nested_display_restoration_is_more_specific_than_owner_release() {
        let capture = Tracker::default();
        let _owner_release = capture.phase("release_display_owner");
        let display = Tracker::default();
        let _restoration = display.phase("restore_display");
        let snapshot = snapshot_states([capture.0.clone(), display.0.clone()], Instant::now());
        assert_eq!(snapshot.operation.as_deref(), Some("restore_display"));
    }
}
