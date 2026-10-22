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
pub(crate) struct Tracker(SharedState, u64);

impl Default for Tracker {
    fn default() -> Self {
        static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
        let state = Arc::new(Mutex::new(State::default()));
        let mut registry = registry().lock().unwrap_or_else(|error| error.into_inner());
        registry.retain(|entry| entry.strong_count() != 0);
        registry.push(Arc::downgrade(&state));
        Self(state, NEXT_OWNER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Tracker {
    pub(crate) fn diagnostic_id(&self) -> u64 {
        self.1
    }

    pub(crate) fn phase(&self, name: &'static str) -> Operation {
        self.operation(name, true)
    }

    /// Add detail inside an existing deadline without resetting its age.
    pub(crate) fn diagnostic_phase(&self, name: &'static str) -> Operation {
        self.operation(name, false)
    }

    fn operation(&self, name: &'static str, watchdog_tracked: bool) -> Operation {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let started_at = Instant::now();
        let parent_operation_id = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            let thread_id = std::thread::current().id();
            let parent = state
                .phases
                .iter()
                .rev()
                .find(|phase| phase.thread_id == thread_id)
                .map_or(0, |phase| phase.id);
            if watchdog_tracked {
                state.phases.push(Phase {
                    id,
                    name,
                    started_at,
                    thread_id,
                });
            }
            parent
        };
        // Adaptive FPS configuration is deliberately excluded: diagnostics
        // describe lifecycle transitions, never frame or steady-state work.
        if logs_transition(name) {
            tracing::info!(target: "macrdp_transition",
                component = "native_capture", worker_pid = std::process::id(),
                owner_id = self.1, operation_id = id, parent_operation_id,
                phase = name, state = "begin", "Native lifecycle transition");
        }
        Operation {
            tracker: self.clone(),
            id,
            name,
            started_at,
            parent_operation_id,
            outcome: "returned",
            watchdog_tracked,
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
    parent_operation_id: u64,
    outcome: &'static str,
    watchdog_tracked: bool,
}

fn logs_transition(name: &str) -> bool {
    matches!(
        name,
        "start"
            | "start_failure_cleanup"
            | "stop"
            | "stream_drop"
            | "replace"
            | "replace_stop"
            | "replace_start"
            | "release_display_owner"
            | "restore_display"
            | "shutdown_display"
            | "recovery_old_stop"
            | "recovery_new_start"
            | "recovery_probe"
            | "recovery_cancel_cleanup"
            | "recovery_probe_cleanup"
            | "recovery_unclaimed_cleanup"
    )
}

impl Operation {
    pub(crate) fn set_outcome(&mut self, outcome: &'static str) {
        self.outcome = outcome;
    }

    /// Record the call's actual result; plain scope exit only means it returned.
    pub(crate) fn finish(mut self, outcome: &'static str) {
        self.set_outcome(outcome);
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        if self.watchdog_tracked {
            self.tracker
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .phases
                .retain(|phase| phase.id != self.id);
        }
        let elapsed = self.started_at.elapsed();
        if logs_transition(self.name) {
            let outcome = if std::thread::panicking() {
                "panicked"
            } else {
                self.outcome
            };
            tracing::info!(target: "macrdp_transition",
                component = "native_capture", worker_pid = std::process::id(),
                owner_id = self.tracker.1, operation_id = self.id,
                parent_operation_id = self.parent_operation_id,
                phase = self.name, state = "end", outcome,
                elapsed_ms = elapsed.as_millis().min(u64::MAX as u128) as u64,
                "Native lifecycle transition");
        }
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
#[path = "native_watchdog/tests.rs"]
mod tests;
