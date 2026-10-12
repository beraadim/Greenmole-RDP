//! Keep only sanitized operational evidence in memory across service restarts.
//! No identifiers, raw errors, keyboard data or clipboard content enter this ring.

use super::{safe_metrics, safe_outcome};
use macrdp_core::{DisconnectionInfo, Metrics};
use serde_json::{json, Value};
use std::collections::VecDeque;

const MAX_SAMPLES: usize = 120;
const MAX_OUTCOMES: usize = 20;

#[derive(Default)]
pub(crate) struct DiagnosticHistory {
    samples: VecDeque<Value>,
    outcomes: VecDeque<Value>,
    last_sample: Option<(u64, u64)>,
    last_terminal: Option<(u64, String, u64)>,
}

fn push_bounded(queue: &mut VecDeque<Value>, value: Value, limit: usize) {
    if queue.len() == limit {
        queue.pop_front();
    }
    queue.push_back(value);
}

impl DiagnosticHistory {
    pub fn record_metrics(&mut self, generation: u64, metrics: &Metrics) {
        let terminal = metrics.connection_outcome.as_ref().map(|reason| {
            (
                generation,
                safe_outcome(reason).to_owned(),
                metrics.bytes_sent,
            )
        });
        if terminal.is_some() && self.last_terminal == terminal {
            return;
        }
        // An observed live/negotiating state separates consecutive attempts,
        // including attempts that have not sent display bytes yet.
        if terminal.is_none() {
            self.last_terminal = None;
        }
        // Stop/default events must not replace the evidence that preceded them.
        if metrics.bytes_sent == 0 && metrics.connection_outcome.is_none() {
            return;
        }
        let key = (
            generation,
            if metrics.sample_id > 0 {
                metrics.sample_id
            } else {
                metrics.timestamp
            },
        );
        // A terminal transition can share a second with the last live sample.
        // Retain it before deduplicating subsequent inactive polls.
        if terminal.is_none() && self.last_sample == Some(key) {
            return;
        }
        self.last_terminal = terminal;
        self.last_sample = Some(key);
        push_bounded(
            &mut self.samples,
            json!({"server_run": generation,
            "metrics": safe_metrics(metrics)}),
            MAX_SAMPLES,
        );
    }

    pub fn record_disconnect(&mut self, generation: u64, info: &DisconnectionInfo) {
        let timestamp = chrono::DateTime::parse_from_rfc3339(&info.disconnected_at)
            .ok()
            .map(|at| at.to_rfc3339());
        let mut final_metrics = safe_metrics(&info.final_metrics);
        // A terminal snapshot has totals and durations but no sampling interval.
        if let Some(fields) = final_metrics.as_object_mut() {
            fields.remove("fps");
            fields.remove("bitrate_kbps");
        }
        push_bounded(
            &mut self.outcomes,
            json!({"server_run": generation,
            "disconnected_at": timestamp, "reason": safe_outcome(&info.reason),
            "duration_secs": info.duration_secs, "bytes_total": info.bytes_total,
            "final_metrics": final_metrics}),
            MAX_OUTCOMES,
        );
    }

    pub fn snapshot(&self) -> Value {
        json!({"samples": self.samples, "session_outcomes": self.outcomes,
            "sample_limit": MAX_SAMPLES, "outcome_limit": MAX_OUTCOMES,
            "retention": "In memory until the application exits; survives service stop and restart.",
            "timings": "Nearest-rank percentiles of up to 1024 real events per stage. Bitmap write events may be regions; stages have independent sample counts. Not end-to-end latency."})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_samples_in_one_second_survive_duplicate_polls() {
        let mut history = DiagnosticHistory::default();
        let mut metrics = Metrics {
            timestamp: 10,
            timestamp_ms: 10000,
            sample_id: 1,
            bytes_sent: 100,
            ..Default::default()
        };
        history.record_metrics(1, &metrics);
        history.record_metrics(1, &metrics);
        metrics.sample_id = 2;
        metrics.timestamp_ms = 10999;
        history.record_metrics(1, &metrics);
        assert_eq!(history.snapshot()["samples"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn terminal_transition_is_retained_within_the_last_sampled_second() {
        let mut history = DiagnosticHistory::default();
        let mut metrics = Metrics {
            timestamp: 10,
            bytes_sent: 100,
            ..Default::default()
        };
        history.record_metrics(1, &metrics);
        metrics.connection_outcome = Some("write_timeout".into());
        history.record_metrics(1, &metrics);
        metrics.timestamp = 11;
        history.record_metrics(1, &metrics);
        let snapshot = history.snapshot();
        let samples = snapshot["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1]["metrics"]["connection_outcome"], "write_timeout");
        assert_eq!(samples[1]["metrics"]["timestamp"], 10);
    }

    #[test]
    fn preserves_failure_evidence_across_stop_restart_and_bounds_storage() {
        let mut history = DiagnosticHistory::default();
        for timestamp in 1..=150 {
            let mut metrics = Metrics {
                timestamp,
                bytes_sent: timestamp,
                ..Default::default()
            };
            metrics.write_timings.max_ms = Some(350.0);
            history.record_metrics(1, &metrics);
            history.record_metrics(1, &metrics);
        }
        history.record_metrics(1, &Metrics::default());
        history.record_metrics(2, &Metrics::default());
        let snapshot = history.snapshot();
        assert_eq!(snapshot["samples"].as_array().unwrap().len(), MAX_SAMPLES);
        assert_eq!(snapshot["samples"][0]["metrics"]["timestamp"], 31);
        assert_eq!(
            snapshot["samples"][119]["metrics"]["write_timings"]["max_ms"],
            350.0
        );
        for generation in 1..=25 {
            history.record_disconnect(
                generation,
                &DisconnectionInfo {
                    reason: "write_timeout".into(),
                    ..Default::default()
                },
            );
        }
        let snapshot = history.snapshot();
        assert_eq!(
            snapshot["session_outcomes"].as_array().unwrap().len(),
            MAX_OUTCOMES
        );
        assert_eq!(snapshot["session_outcomes"][0]["server_run"], 6);
        assert_eq!(snapshot["session_outcomes"][19]["reason"], "write_timeout");
        assert!(snapshot["session_outcomes"][19]["final_metrics"]
            .get("fps")
            .is_none());
        assert!(snapshot.to_string().len() < 512 * 1024);
    }

    #[test]
    fn sanitizes_before_retaining_and_keeps_rejected_handshake_outcomes() {
        let secret = "private-canary-name-address-error";
        let mut history = DiagnosticHistory::default();
        let metrics = Metrics {
            connection_outcome: Some("authentication_failed".into()),
            encoder_mode: secret.into(),
            transport_mode: secret.into(),
            ..Default::default()
        };
        history.record_metrics(1, &metrics);
        for timestamp in 1..=130 {
            history.record_metrics(
                1,
                &Metrics {
                    timestamp,
                    ..metrics.clone()
                },
            );
        }
        let info = DisconnectionInfo {
            client_ip: secret.into(),
            client_name: secret.into(),
            connected_at: secret.into(),
            disconnected_at: secret.into(),
            reason: secret.into(),
            final_metrics: Metrics {
                connection_outcome: Some(secret.into()),
                ..metrics
            },
            ..Default::default()
        };
        history.record_disconnect(1, &info);
        let snapshot = history.snapshot();
        assert!(!snapshot.to_string().contains(secret));
        assert_eq!(
            snapshot["samples"][0]["metrics"]["connection_outcome"],
            "authentication_failed"
        );
        assert_eq!(
            snapshot["samples"].as_array().unwrap().len(),
            1,
            "inactive terminal polls must retain preceding evidence"
        );
        assert_eq!(snapshot["session_outcomes"][0]["reason"], "unknown");
        assert!(snapshot["session_outcomes"][0]["disconnected_at"].is_null());
        assert!(snapshot.to_string().len() < 32 * 1024);
    }
}
