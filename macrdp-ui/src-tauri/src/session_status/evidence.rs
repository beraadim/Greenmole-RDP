use super::{parse_time, Evidence, Session, Value};

/// IDs are only correlation keys in bounded memory; none are exported to the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Operation {
    component: &'static str,
    pid: u64,
    owner: u64,
    id: u64,
    phase: String,
}
impl Operation {
    pub(super) fn is_encoder(&self) -> bool {
        self.component == "video_encoder"
    }
}

pub(super) struct Record {
    pub component: String,
    pub phase: String,
    pub state: String,
    pub outcome: String,
    pub at: i64,
    pub dropped_records: u64,
    pub pid: u64,
    owner: u64,
    id: u64,
}
impl Record {
    pub(super) fn parse(encoded: &str) -> Option<Self> {
        if encoded.len() > 16 * 1024 {
            return None;
        }
        let value: Value = serde_json::from_str(encoded).ok()?;
        if value.get("target")?.as_str()? != "macrdp_transition" {
            return None;
        }
        let fields = value.get("fields")?;
        let component = fields.get("component")?.as_str()?;
        if !matches!(
            component,
            "session"
                | "private_guard"
                | "wake_lease"
                | "native_capture"
                | "display"
                | "video_encoder"
                | "encoder_startup"
                | "diagnostics"
        ) {
            return None;
        }
        let text = |key| {
            fields
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 64)
                .unwrap_or("")
                .to_string()
        };
        let number = |key| fields.get(key).and_then(Value::as_u64).unwrap_or(0);
        let at = fields
            .get(if component == "session" && text("state") == "connected" {
                "session_started_unix_ms"
            } else {
                "observed_unix_ms"
            })
            .and_then(Value::as_i64)
            .or_else(|| {
                value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_time)
            })?;
        Some(Self {
            component: component.into(),
            phase: text("phase"),
            state: text("state"),
            outcome: text("outcome"),
            at,
            dropped_records: number("dropped_records"),
            pid: if component == "display" {
                number("process_id")
            } else {
                number("worker_pid")
            },
            owner: number("owner_id"),
            id: match component {
                "display" => number("transition_id"),
                "video_encoder" => number("retirement_id"),
                _ => number("operation_id"),
            },
        })
    }
    pub(super) fn operation(&self) -> Option<Operation> {
        if self.pid == 0 || self.id == 0 {
            return None;
        }
        let component = match self.component.as_str() {
            "native_capture" => "native_capture",
            "display" => "display",
            "video_encoder" => "video_encoder",
            _ => return None,
        };
        Some(Operation {
            component,
            pid: self.pid,
            owner: self.owner,
            id: self.id,
            // A native encoder retains the same retirement ID from create to release.
            phase: if component == "video_encoder" {
                String::new()
            } else {
                self.phase.clone()
            },
        })
    }
}

pub(super) fn apply(session: &mut Session, record: &Record, operation: Option<Operation>) {
    match (
        record.component.as_str(),
        record.phase.as_str(),
        record.state.as_str(),
    ) {
        ("session", "connection", "disconnecting") => session.closing(),
        ("private_guard", "protection", "complete") => {
            session.summary.cleanup.private_guard = Evidence::Complete
        }
        ("private_guard", "protection", "pending") => {
            session.summary.cleanup.private_guard = Evidence::Pending
        }
        ("wake_lease", "cleanup", "complete") => session.summary.cleanup.wake = Evidence::Complete,
        ("wake_lease", "cleanup", "pending") => session.summary.cleanup.wake = Evidence::Pending,
        ("native_capture", "restore_display", "begin") => {
            session.summary.cleanup.display = Evidence::Pending;
            session.summary.cleanup.display_disposition = "unconfirmed";
            if operation.is_some_and(|op| session.remember(op)) {
                // The lifecycle invokes restoration only after native capture retires.
                session.summary.cleanup.capture = Evidence::Complete;
            } else {
                session.limit_evidence();
            }
        }
        ("native_capture", "restore_display", "end") => {
            if let Some(op) = operation.filter(|op| session.operations.contains(op)) {
                let completed = Operation {
                    phase: if record.outcome == "ok" {
                        "restore_ok"
                    } else {
                        "restore_failed"
                    }
                    .into(),
                    ..op
                };
                session.remember(completed);
                session.summary.cleanup.display = if session
                    .operations
                    .iter()
                    .any(|op| op.component == "native_capture" && op.phase == "restore_failed")
                {
                    Evidence::Failed
                } else if session
                    .operations
                    .iter()
                    .filter(|op| op.component == "native_capture" && op.phase == "restore_display")
                    .any(|op| {
                        !session.operations.iter().any(|done| {
                            done.phase == "restore_ok"
                                && done.pid == op.pid
                                && done.owner == op.owner
                                && done.id == op.id
                        })
                    })
                {
                    Evidence::Pending
                } else {
                    Evidence::Complete
                };
            }
        }
        ("display", "display_disable" | "owner_retire", "begin") => {
            if let Some(op) = operation {
                session.remember(op);
            }
        }
        ("display", "display_disable" | "owner_retire", "succeeded") => {
            if operation.is_some_and(|op| session.operations.contains(&op)) {
                session.summary.cleanup.display_disposition = if record.phase == "owner_retire" {
                    "removed"
                } else {
                    "retained_disabled"
                };
            }
        }
        ("video_encoder", _, "begin" | "queued") => {
            session.summary.cleanup.native_encoder = Evidence::Pending;
            if let Some(op) = operation {
                session.remember(op);
            } else {
                session.limit_evidence();
            }
        }
        ("video_encoder", "retirement", "end") => {
            if let Some(op) = operation.filter(|op| session.operations.contains(op)) {
                // Keep IDs so duplicate ends cannot be rebound to a later session.
                let failed = record.outcome != "ok";
                session.summary.cleanup.native_encoder = if failed {
                    Evidence::Failed
                } else {
                    Evidence::Complete
                };
                // Multiple owners can retire independently (fallback/reconfiguration).
                // Mark this owner's exact retirement and require all observed owners.
                let completed = Operation {
                    phase: if failed { "failed" } else { "retired" }.into(),
                    ..op
                };
                session.remember(completed);
                if session
                    .operations
                    .iter()
                    .any(|op| op.is_encoder() && op.phase == "failed")
                {
                    session.summary.cleanup.native_encoder = Evidence::Failed;
                } else if !failed
                    && session
                        .operations
                        .iter()
                        .filter(|op| op.is_encoder() && op.phase.is_empty())
                        .any(|op| {
                            !session.operations.iter().any(|done| {
                                done.phase == "retired" && done.pid == op.pid && done.id == op.id
                            })
                        })
                {
                    session.summary.cleanup.native_encoder = Evidence::Pending;
                }
            }
        }
        _ => {}
    }
    if session.evidence_lost {
        session.summary.cleanup.verified = false;
    }
}
