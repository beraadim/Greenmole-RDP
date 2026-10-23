//! Read-only, bounded session evidence. Callback ingress is synchronous so UI task
//! scheduling cannot turn an older completion into the next session's success.
//! This reducer never queries or controls a display, capture, encoder or guard.

use crate::state::AppState;
use macrdp_core::{ConnectionEvent, Metrics, ServerStatus};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::State;

mod evidence;
#[cfg(test)]
mod tests;
use evidence::{Operation, Record};
const HISTORY_LIMIT: usize = 20;
const OPERATIONS_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Evidence {
    #[default]
    Unconfirmed,
    Pending,
    Complete,
    Failed,
    NotRequired,
}
impl Evidence {
    fn complete(self) -> bool {
        matches!(self, Self::Complete | Self::NotRequired)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Cleanup {
    capture: Evidence,
    display: Evidence,
    native_encoder: Evidence,
    wake: Evidence,
    private_guard: Evidence,
    display_disposition: &'static str,
    verified: bool,
    observation_limited: bool,
}
impl Cleanup {
    fn refresh(&mut self, ended: bool) {
        self.verified = ended
            && !self.observation_limited
            && matches!(self.display_disposition, "removed" | "retained_disabled")
            && [
                self.capture,
                self.display,
                self.native_encoder,
                self.wake,
                self.private_guard,
            ]
            .into_iter()
            .all(Evidence::complete);
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionSummary {
    sequence: u64,
    started_at_ms: i64,
    ended_at_ms: Option<i64>,
    phase: &'static str,
    host_transport: &'static str,
    encoder: &'static str,
    outcome: &'static str,
    cleanup: Cleanup,
}

struct Session {
    summary: SessionSummary,
    closing: bool,
    sample_id: u64,
    worker_pid: u64,
    operations: Vec<Operation>,
    evidence_lost: bool,
}
impl Session {
    fn new(sequence: u64, started_at_ms: i64) -> Self {
        Self {
            summary: SessionSummary {
                sequence,
                started_at_ms,
                ended_at_ms: None,
                phase: "preparing_desktop",
                host_transport: "negotiating",
                encoder: "negotiating",
                outcome: "unknown",
                cleanup: Cleanup {
                    display_disposition: "unconfirmed",
                    ..Cleanup::default()
                },
            },
            closing: false,
            sample_id: 0,
            worker_pid: 0,
            operations: Vec::new(),
            evidence_lost: false,
        }
    }
    fn closing(&mut self) {
        self.closing = true;
        if self.summary.ended_at_ms.is_none() {
            self.summary.phase = "restoring_desktop";
        }
    }
    fn remember(&mut self, operation: Operation) -> bool {
        if self.operations.contains(&operation) {
            return true;
        }
        if self.operations.len() >= OPERATIONS_LIMIT {
            self.limit_evidence();
            return false;
        }
        self.operations.push(operation);
        true
    }
    fn limit_evidence(&mut self) {
        self.evidence_lost = true;
        self.summary.cleanup.observation_limited = true;
        self.summary.cleanup.verified = false;
    }
    fn finish(&mut self, ended_at: i64, outcome: &'static str) {
        self.closing();
        self.summary.ended_at_ms = Some(ended_at.max(self.summary.started_at_ms));
        self.summary.outcome = outcome;
        self.summary.phase = "ended";
        self.summary.cleanup.refresh(!self.evidence_lost);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct SessionStatus {
    current: Option<SessionSummary>,
    recent: Vec<SessionSummary>,
}

#[derive(Default)]
pub(crate) struct SessionStatusObserver {
    generation: u64,
    generation_observation_limited: bool,
    sequence: u64,
    current: Option<Session>,
    recent: Vec<Session>,
    admission_guard: Option<(i64, u64, Evidence)>,
    pending_worker: Option<(i64, u64)>,
}
impl SessionStatusObserver {
    pub(crate) fn begin_generation(&mut self, generation: u64) {
        if self.generation == generation {
            return;
        }
        if let Some(mut current) = self.current.take() {
            current.finish(chrono::Utc::now().timestamp_millis(), "server_restarted");
            self.push_recent(current);
        }
        self.generation = generation;
        self.generation_observation_limited = false;
        self.admission_guard = None;
        self.pending_worker = None;
    }
    fn push_recent(&mut self, session: Session) {
        self.recent.insert(0, session);
        self.recent.truncate(HISTORY_LIMIT);
    }
    pub(crate) fn snapshot(&self) -> SessionStatus {
        SessionStatus {
            current: self.current.as_ref().map(|s| s.summary.clone()),
            recent: self.recent.iter().map(|s| s.summary.clone()).collect(),
        }
    }
    pub(crate) fn status(&mut self, generation: u64, status: &ServerStatus) {
        if generation != self.generation {
            return;
        }
        if matches!(
            status.state.as_str(),
            "recovering" | "cleanup_pending" | "stopping"
        ) {
            if let Some(session) = self.current.as_mut() {
                session.closing();
            }
        }
        if status.state == "stopped" || status.state.starts_with("error") {
            if let Some(mut session) = self.current.take() {
                session.finish(chrono::Utc::now().timestamp_millis(), "server_stopped");
                self.push_recent(session);
            }
        }
    }
    pub(crate) fn connection(&mut self, generation: u64, event: &ConnectionEvent) {
        if generation != self.generation {
            return;
        }
        match event {
            ConnectionEvent::Connected(info) => {
                let Some(start) = parse_time(&info.connected_at) else {
                    return;
                };
                if self
                    .current
                    .as_ref()
                    .is_some_and(|s| s.summary.started_at_ms >= start)
                    || self
                        .recent
                        .first()
                        .is_some_and(|s| s.summary.started_at_ms >= start)
                {
                    return;
                }
                if let Some(mut previous) = self.current.take() {
                    previous.finish(start, "superseded");
                    self.push_recent(previous);
                }
                self.sequence = self.sequence.saturating_add(1);
                let mut session = Session::new(self.sequence, start);
                if self.generation_observation_limited {
                    session.limit_evidence();
                }
                if let Some((at, pid)) = self.pending_worker.take() {
                    if at <= start && start - at <= 5_000 {
                        session.worker_pid = pid;
                    }
                }
                self.current = Some(session);
                self.apply_admission_guard();
            }
            ConnectionEvent::Disconnected(info) => {
                let Some(start) = parse_time(&info.connected_at) else {
                    return;
                };
                if !self
                    .current
                    .as_ref()
                    .is_some_and(|s| s.summary.started_at_ms == start)
                {
                    return;
                }
                let mut session = self.current.take().unwrap();
                self.update_metrics(&mut session, &info.final_metrics);
                session.finish(
                    parse_time(&info.disconnected_at).unwrap_or(start),
                    safe_outcome(&info.reason),
                );
                self.push_recent(session);
            }
        }
    }
    fn apply_admission_guard(&mut self) {
        let Some((at, pid, guard)) = self.admission_guard else {
            return;
        };
        let last_end = self
            .recent
            .first()
            .and_then(|s| s.summary.ended_at_ms)
            .unwrap_or(0);
        if let Some(current) = self.current.as_mut().filter(|s| {
            !s.closing
                && s.worker_pid != 0
                && s.worker_pid == pid
                && at >= last_end
                && at <= s.summary.started_at_ms
                && s.summary.started_at_ms - at <= 30_000
        }) {
            current.summary.cleanup.private_guard = guard;
            self.admission_guard = None;
        }
    }
    pub(crate) fn metrics(&mut self, generation: u64, metrics: &Metrics) {
        if generation != self.generation || !metrics.session_active {
            return;
        }
        if let Some(mut current) = self.current.take() {
            if !current.closing {
                self.update_metrics(&mut current, metrics);
            }
            self.current = Some(current);
        }
    }
    fn update_metrics(&self, session: &mut Session, metrics: &Metrics) {
        if metrics.sample_id > 0 && metrics.sample_id <= session.sample_id {
            return;
        }
        session.sample_id = metrics.sample_id;
        session.summary.host_transport = safe_transport(&metrics.transport_mode);
        session.summary.encoder = safe_encoder(&metrics.encoder_mode);
        if metrics.bytes_sent > 0 && metrics.session_active && !session.closing {
            session.summary.phase = "streaming";
        }
        // This describes only native VideoToolbox ownership. A software/bitmap
        // path needs no native retirement unless an attempted native owner was seen.
        if session.operations.iter().all(|op| !op.is_encoder())
            && matches!(
                session.summary.encoder,
                "bitmap" | "uncompressed" | "openh264 / avc420" | "openh264 / avc444"
            )
        {
            session.summary.cleanup.native_encoder = Evidence::NotRequired;
        }
    }
    pub(crate) fn record(&mut self, generation: u64, encoded: &str) {
        if generation != self.generation {
            return;
        }
        let Some(record) = Record::parse(encoded) else {
            return;
        };
        if record.component == "diagnostics"
            && record.phase == "queue"
            && record.state == "records_dropped"
            && record.dropped_records > 0
        {
            // Loss may precede Connected or conceal an owner creation. Preserve
            // uncertainty for this whole bridge generation, including history.
            self.generation_observation_limited = true;
            if let Some(current) = self.current.as_mut() {
                current.limit_evidence();
            }
            for previous in &mut self.recent {
                previous.limit_evidence();
            }
            return;
        }
        if record.component == "session" && record.state == "connected" {
            if let Some(current) = self.current.as_mut().filter(|s| {
                !s.closing
                    && record.at <= s.summary.started_at_ms
                    && s.summary.started_at_ms - record.at <= 5_000
            }) {
                current.worker_pid = record.pid;
            } else {
                self.pending_worker = Some((record.at, record.pid));
            }
            self.apply_admission_guard();
            return;
        }
        if record.component == "private_guard"
            && matches!(record.state.as_str(), "ready" | "not_required")
        {
            let guard = if record.state == "not_required" {
                Evidence::NotRequired
            } else {
                Evidence::Pending
            };
            self.admission_guard = Some((record.at, record.pid, guard));
            self.apply_admission_guard();
            return;
        }
        if record.component == "encoder_startup" {
            if let Some(current) = self
                .current
                .as_mut()
                .filter(|s| !s.closing && record.at >= s.summary.started_at_ms)
            {
                if record.state == "begin" && current.summary.phase != "streaming" {
                    current.summary.phase = "waiting_encoder";
                }
            }
            return;
        }
        // A completion is routed by the exact observed operation, including PID.
        // New unpaired cleanup evidence is accepted only during a known close.
        let operation = record.operation();
        let known_current = operation.as_ref().is_some_and(|op| {
            self.current
                .as_ref()
                .is_some_and(|s| s.operations.contains(op))
        });
        let known_recent = operation
            .as_ref()
            .and_then(|op| self.recent.iter().position(|s| s.operations.contains(op)));
        let target = if known_current {
            self.current.as_mut()
        } else if let Some(index) = known_recent {
            self.recent.get_mut(index)
        } else if let Some(current) = self.current.as_mut() {
            if record.at < current.summary.started_at_ms {
                self.recent
                    .iter_mut()
                    .find(|s| record.at >= s.summary.started_at_ms)
            } else if current.closing
                || record.component == "video_encoder"
                || (record.component == "session" && record.state == "disconnecting")
            {
                Some(current)
            } else {
                None
            }
        } else {
            self.recent
                .first_mut()
                .filter(|s| record.at >= s.summary.started_at_ms)
        };
        let Some(session) = target else {
            return;
        };
        // Scalar guard/wake/session markers have no operation ID. Require the
        // authenticated worker marker as well as the session time interval.
        if matches!(
            record.component.as_str(),
            "wake_lease"
                | "private_guard"
                | "session"
                | "native_capture"
                | "video_encoder"
                | "display"
        ) && (session.worker_pid == 0 || record.pid != session.worker_pid)
        {
            return;
        }
        evidence::apply(session, &record, operation);
        session
            .summary
            .cleanup
            .refresh(session.summary.ended_at_ms.is_some() && !session.evidence_lost);
    }
}

fn parse_time(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|v| v.timestamp_millis())
}
fn safe_outcome(value: &str) -> &'static str {
    match value {
        "client_disconnect" => "client_disconnect",
        "peer_closed" => "peer_closed",
        "peer_reset" => "peer_reset",
        "peer_eof" => "peer_eof",
        "protocol_error" => "protocol_error",
        "transport_error" => "transport_error",
        "transport_timeout" => "transport_timeout",
        "write_timeout" => "write_timeout",
        "idle_timeout" => "idle_timeout",
        "server_shutdown" => "server_shutdown",
        "credentials_changed" => "credentials_changed",
        "handshake_timeout" => "handshake_timeout",
        "handshake_rejected" => "handshake_rejected",
        "authentication_failed" => "authentication_failed",
        "activation_failed" => "activation_failed",
        "admission_throttled" => "admission_throttled",
        "capture_ended" => "capture_ended",
        "capture_error" => "capture_error",
        "output_overflow" => "output_overflow",
        _ => "unknown",
    }
}
fn safe_transport(value: &str) -> &'static str {
    match value {
        "bitmap" => "bitmap",
        "bitmap-fallback" => "bitmap-fallback",
        "gfx-avc420" => "gfx-avc420",
        "gfx-avc444" => "gfx-avc444",
        "gfx-uncompressed" => "gfx-uncompressed",
        "gfx-uncompressed-fallback" => "gfx-uncompressed-fallback",
        _ => "negotiating",
    }
}
fn safe_encoder(value: &str) -> &'static str {
    match value {
        "bitmap" => "bitmap",
        "uncompressed" => "uncompressed",
        "videotoolbox / avc420" => "videotoolbox / avc420",
        "videotoolbox / avc444" => "videotoolbox / avc444",
        "openh264 / avc420" => "openh264 / avc420",
        "openh264 / avc444" => "openh264 / avc444",
        _ => "negotiating",
    }
}

#[tauri::command]
pub async fn get_session_status(state: State<'_, Arc<AppState>>) -> Result<SessionStatus, String> {
    state
        .session_status
        .lock()
        .map(|observer| observer.snapshot())
        .map_err(|_| "Session status unavailable".into())
}
