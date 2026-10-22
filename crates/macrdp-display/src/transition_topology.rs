//! Asynchronous observations never delay native display transactions. A stuck
//! Apple query occupies at most one worker; further jobs remain bounded/dropped.
use core_graphics::display::CGDisplay;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::Instant;

use super::Record;

const QUEUE_CAPACITY: usize = 8;
static WORKER: OnceLock<Option<mpsc::SyncSender<Observation>>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

struct Observation {
    record: Record,
    queued: Instant,
}

pub(super) fn schedule(record: Record) {
    let worker = WORKER.get_or_init(|| match start_worker(observe) {
        Ok(sender) => Some(sender),
        Err(_) => {
            tracing::info!(target: "macrdp_transition",
                component = "display_topology", process_id = std::process::id(),
                state = "unavailable", reason = "observer_worker_start_failed",
                "Display topology observations unavailable; transitions continue without waiting");
            None
        }
    });
    if worker.as_ref().map_or(true, |sender| {
        sender
            .try_send(Observation {
                record,
                queued: Instant::now(),
            })
            .is_err()
    }) {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

fn start_worker(
    observe: impl Fn(Observation) + Send + 'static,
) -> std::io::Result<mpsc::SyncSender<Observation>> {
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("macrdp-topology-diagnostics".to_owned())
        .spawn(move || {
            while let Ok(observation) = receiver.recv() {
                observe(observation);
            }
        })?;
    // No shutdown join: this process owns the worker, and a diagnostic-only
    // native query must never prevent display restoration or application exit.
    Ok(sender)
}

fn observe(observation: Observation) {
    let queue_delay_ms = observation
        .queued
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let query_started = Instant::now();
    let query_started_unix_ms = super::unix_ms();
    let topology = snapshot();
    let query_elapsed_ms = query_started
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let record = observation.record;
    let dropped_observations = DROPPED.swap(0, Ordering::Relaxed);
    tracing::info!(target: "macrdp_transition",
        component = "display_topology", process_id = record.source_pid,
        transition_id = record.transition_id, phase = record.phase.as_str(),
        state = record.state.as_str(), display_id = record.details.display_id,
        requested_phase_unix_ms = record.observed_unix_ms,
        query_started_unix_ms, observed_unix_ms = super::unix_ms(),
        queue_delay_ms, query_elapsed_ms, dropped_observations,
        topology = %topology,
        "Asynchronous display topology observation; state may follow the requested phase");
}

fn snapshot() -> serde_json::Value {
    let result = (|| -> anyhow::Result<_> {
        let online = crate::macos::online_display_ids()?;
        anyhow::ensure!(online.len() <= 16, "topology diagnostic display limit");
        let main = CGDisplay::main().id;
        let displays: Vec<_> = online
            .into_iter()
            .map(|id| {
                let display = CGDisplay::new(id);
                let bounds = display.bounds();
                let mode = display.display_mode().map(|mode| {
                    json!({
                        "pixels": [mode.pixel_width(), mode.pixel_height()],
                        "logical": [mode.width(), mode.height()],
                        "refresh_hz": mode.refresh_rate(),
                        "io_flags": mode.io_flags()
                    })
                });
                json!({"id":id,"active":display.is_active(),
                "mirrored":display.is_in_mirror_set(),
                "bounds":[bounds.origin.x,bounds.origin.y,bounds.size.width,bounds.size.height],
                "cg_cached_mode":mode})
            })
            .collect();
        Ok(json!({"main":main,"online":displays}))
    })();
    // API failures are data, never grounds to change the real operation result.
    // Do not include device descriptions, window metadata or arbitrary errors.
    result.unwrap_or_else(|_| json!({"unavailable":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn blocked_observer_cannot_block_producers_or_grow_the_queue() {
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let release = barrier.clone();
        let (entered, started) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let sender = start_worker(move |_| {
            let _ = entered.send(());
            release.wait();
            let _ = completed.send(());
        })
        .unwrap();
        let job = || Observation {
            record: super::super::Record {
                schema: 1,
                emitter_pid: 1,
                source_pid: 1,
                transition_id: 1,
                observed_unix_ms: 1,
                phase: super::super::Phase::OwnerCreate,
                state: super::super::State::Begin,
                elapsed_ms: 0,
                details: super::super::Details::default(),
            },
            queued: Instant::now(),
        };
        sender.try_send(job()).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..QUEUE_CAPACITY {
            sender.try_send(job()).unwrap();
        }
        assert!(matches!(
            sender.try_send(job()),
            Err(mpsc::TrySendError::Full(_))
        ));
        // Drain the fake worker and drop its sole sender; no real display query
        // or permanently blocked test thread is left behind.
        for _ in 0..=QUEUE_CAPACITY {
            barrier.wait();
            completion.recv_timeout(Duration::from_secs(2)).unwrap();
        }
    }
}
