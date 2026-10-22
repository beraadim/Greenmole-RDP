//! Per-startup native phase timing. This never starts a timer, worker or codec.

use std::time::Instant;

pub(super) struct Phase {
    retirement_id: u64,
    name: &'static str,
    started: Instant,
    status: Option<i32>,
}

impl Phase {
    pub(super) fn begin(retirement_id: u64, name: &'static str) -> Self {
        let phase = Self {
            retirement_id,
            name,
            started: Instant::now(),
            status: None,
        };
        tracing::info!(target: "macrdp_transition",
            component = "video_encoder", worker_pid = std::process::id(),
            retirement_id, phase = name, state = "begin",
            "Encoder lifecycle transition");
        phase
    }

    pub(super) fn finish(mut self, status: i32) {
        self.status = Some(status);
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let outcome = if std::thread::panicking() {
            "panicked"
        } else {
            match self.status {
                Some(0) => "ok",
                Some(_) => "error",
                // A required property returned early; no successful end may be
                // inferred from ordinary Rust scope exit.
                None => "not_completed",
            }
        };
        tracing::info!(target: "macrdp_transition",
            component = "video_encoder", worker_pid = std::process::id(),
            retirement_id = self.retirement_id, phase = self.name, state = "end",
            outcome, native_status = ?self.status,
            elapsed_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            "Encoder lifecycle transition");
    }
}
