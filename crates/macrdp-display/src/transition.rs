//! Bounded transition records, including diagnostics from isolated native owners.
//!
//! Native owners intentionally have no tracing subscriber and must not query
//! CoreGraphics modes before first activation. Their small, fixed-schema stderr
//! records are forwarded by the parent; topology observations run only outside
//! those native owners, at operation boundaries, never on an idle timer.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[path = "transition_stderr.rs"]
mod stderr;
#[path = "transition_topology.rs"]
mod topology;
pub(crate) use stderr::forward_stderr;

const PREFIX: &[u8] = b"macrdp-transition-v1:";
const MAX_RECORD_BYTES: usize = 4096;
static HELPER_OUTPUT: AtomicBool = AtomicBool::new(false);
static NEXT_TRANSITION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    OwnerCreate,
    VirtualCreate,
    ModeApply,
    ModeSettle,
    RemoteBegin,
    RemoteEnd,
    PrimaryPromote,
    PrimaryRestore,
    CapturePrepare,
    DisplayEnable,
    DisplayDisable,
    OwnerDrop,
    OwnerRetire,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::OwnerCreate => "owner_create",
            Self::VirtualCreate => "virtual_create",
            Self::ModeApply => "mode_apply",
            Self::ModeSettle => "mode_settle",
            Self::RemoteBegin => "remote_begin",
            Self::RemoteEnd => "remote_end",
            Self::PrimaryPromote => "primary_promote",
            Self::PrimaryRestore => "primary_restore",
            Self::CapturePrepare => "capture_prepare",
            Self::DisplayEnable => "display_enable",
            Self::DisplayDisable => "display_disable",
            Self::OwnerDrop => "owner_drop",
            Self::OwnerRetire => "owner_retire",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum State {
    Begin,
    Succeeded,
    Failed,
    Interrupted,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Begin => "begin",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Details {
    display_id: u32,
    requested_width: u16,
    requested_height: u16,
    primary_id: u32,
}

impl Details {
    pub(crate) fn display(display_id: u32) -> Self {
        Self {
            display_id,
            ..Self::default()
        }
    }
    pub(crate) fn size(mut self, width: u16, height: u16) -> Self {
        self.requested_width = width;
        self.requested_height = height;
        self
    }
    pub(crate) fn primary(mut self, primary_id: u32) -> Self {
        self.primary_id = primary_id;
        self
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u8,
    emitter_pid: u32,
    source_pid: u32,
    transition_id: u64,
    observed_unix_ms: u64,
    phase: Phase,
    state: State,
    elapsed_ms: u64,
    details: Details,
}

/// Call before any native operation in an isolated helper. No display query or
/// logging initialization is performed by selecting the stderr transport.
pub(crate) fn install_helper_output() {
    HELPER_OUTPUT.store(true, Ordering::Release);
}

/// This wrapper adds no query in a native owner. Its begin record survives a
/// stuck native call; an error leaves the original Result untouched.
pub(crate) fn native<T>(
    phase: Phase,
    details: Details,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    measure(phase, details, false, operation)
}

/// Request an asynchronous parent observation without delaying the operation.
/// A single bounded worker cannot poison the isolated owner's mode cache.
/// Observation times distinguish snapshots from exact phase boundary records.
pub(crate) fn observed<T>(
    phase: Phase,
    details: Details,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    measure(phase, details, true, operation)
}

fn measure<T>(
    phase: Phase,
    details: Details,
    observe: bool,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let mut transition = Transition::begin(phase, details, observe);
    let result = operation();
    transition.finish(if result.is_ok() {
        State::Succeeded
    } else {
        State::Failed
    });
    result
}

struct Transition {
    record: Record,
    started: Instant,
    observe: bool,
    finished: bool,
}

impl Transition {
    fn begin(phase: Phase, details: Details, observe: bool) -> Self {
        let observe = observe && !HELPER_OUTPUT.load(Ordering::Acquire);
        let pid = std::process::id();
        let record = Record {
            schema: 1,
            emitter_pid: pid,
            source_pid: pid,
            transition_id: NEXT_TRANSITION.fetch_add(1, Ordering::Relaxed),
            observed_unix_ms: unix_ms(),
            phase,
            state: State::Begin,
            elapsed_ms: 0,
            details,
        };
        let started = Instant::now();
        emit(record);
        if observe {
            topology::schedule(record);
        }
        Self {
            record,
            started,
            observe,
            finished: false,
        }
    }

    fn finish(&mut self, state: State) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.record.state = state;
        self.record.elapsed_ms =
            self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.record.observed_unix_ms = unix_ms();
        emit(self.record);
        if self.observe {
            topology::schedule(self.record);
        }
    }
}

impl Drop for Transition {
    fn drop(&mut self) {
        // A panic should leave a paired diagnostic, without claiming the native
        // operation succeeded or collecting more topology during unwinding.
        self.observe = false;
        self.finish(State::Interrupted);
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn emit(mut record: Record) {
    if HELPER_OUTPUT.load(Ordering::Acquire) {
        record.emitter_pid = std::process::id();
        if let Ok(encoded) = serde_json::to_string(&record) {
            // Fixed-schema fields keep records far below the bounded receiver.
            eprintln!(
                "{}{}",
                std::str::from_utf8(PREFIX).expect("ASCII prefix"),
                encoded
            );
        }
    } else {
        tracing::info!(target: "macrdp_transition",
            component = "display", process_id = record.source_pid,
            transition_id = record.transition_id, phase = record.phase.as_str(),
            state = record.state.as_str(), elapsed_ms = record.elapsed_ms,
            observed_unix_ms = record.observed_unix_ms,
            display_id = record.details.display_id,
            requested_width = record.details.requested_width,
            requested_height = record.details.requested_height,
            primary_id = record.details.primary_id,
            "Display transition");
    }
}

fn decode(line: &[u8], child_pid: u32) -> Option<Record> {
    let encoded = line.strip_prefix(PREFIX)?;
    if line.len() > MAX_RECORD_BYTES {
        return None;
    }
    let record: Record = serde_json::from_slice(encoded).ok()?;
    (record.schema == 1
        && record.emitter_pid == child_pid
        && record.source_pid != 0
        && record.transition_id != 0
        && (record.state != State::Begin || record.elapsed_ms == 0))
        .then_some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded() -> Vec<u8> {
        let record = Record {
            schema: 1,
            emitter_pid: 77,
            source_pid: 66,
            transition_id: 2,
            observed_unix_ms: 123,
            phase: Phase::ModeApply,
            state: State::Begin,
            elapsed_ms: 0,
            details: Details::display(17).size(1920, 1080),
        };
        [PREFIX, &serde_json::to_vec(&record).unwrap()].concat()
    }

    #[test]
    fn nested_record_keeps_origin_but_requires_the_exact_child_emitter() {
        let bytes = encoded();
        assert_eq!(decode(&bytes, 77).unwrap().source_pid, 66);
        assert!(decode(&bytes, 66).is_none());
        assert!(decode(b"ordinary stderr", 77).is_none());
    }

    #[test]
    fn unknown_fields_and_unbounded_records_are_not_transition_messages() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&encoded()[PREFIX.len()..]).unwrap();
        value["unexpected"] = true.into();
        let bytes = [PREFIX, &serde_json::to_vec(&value).unwrap()].concat();
        assert!(decode(&bytes, 77).is_none());
        assert!(decode(&vec![b'x'; MAX_RECORD_BYTES + 1], 77).is_none());
    }
}
