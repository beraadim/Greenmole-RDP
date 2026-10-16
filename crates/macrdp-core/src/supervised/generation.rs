//! One worker generation: health events, settings acknowledgements and ordered window moves.
use super::policy::apply_update;
use super::process::Worker;
use super::protocol::{Command, Event};
use super::runtime::{diagnostic, update_status};
use super::{ParentCommand, Shared, WindowRequest, HEARTBEAT_TIMEOUT, START_TIMEOUT};
use crate::{callbacks::*, tls::TlsCertificateInfo, ServerConfig};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{atomic::Ordering, mpsc, Arc};
use std::time::{Duration, Instant};

pub(super) struct Generation<'a> {
    pub worker: &'a mut Worker,
    pub config: &'a mut ServerConfig,
    pub shared: &'a Arc<Shared>,
    pub handler: &'a Arc<dyn ServerEventHandler>,
    pub commands: &'a mut tokio::sync::mpsc::Receiver<ParentCommand>,
    pub ready: &'a mut Option<tokio::sync::oneshot::Sender<Result<(u16, TlsCertificateInfo)>>>,
    pub metrics_sequence: u64,
    pub next_update_request: u64,
}

pub(super) struct Outcome {
    pub failure: Option<String>,
    pub metrics_sequence: u64,
    pub next_update_request: u64,
}

impl Generation<'_> {
    pub async fn run(self) -> Outcome {
        let Self {
            worker,
            config,
            shared,
            handler,
            commands,
            ready,
            mut metrics_sequence,
            mut next_update_request,
        } = self;
        let launched = Instant::now();
        let mut last_event = launched;
        let mut worker_ready = false;
        let mut requests: HashMap<u64, WindowRequest> = HashMap::new();
        let mut updates: HashMap<u64, (ConfigUpdate, tokio::sync::oneshot::Sender<Result<()>>)> =
            HashMap::new();
        let mut previous_child_sample = None;
        let failure = 'running: loop {
            if shared.stopping.load(Ordering::Acquire) {
                break None;
            }
            for _ in 0..256 {
                match worker.events.try_recv() {
                    Ok(Ok(event)) => match event {
                        Event::Ready { port, certificate } => {
                            config.port = port;
                            worker_ready = true;
                            last_event = Instant::now();
                            {
                                let mut snapshot = shared.snapshot.lock().unwrap();
                                snapshot.metrics = Metrics {
                                    sample_id: metrics_sequence,
                                    ..Default::default()
                                };
                                snapshot.input = InputDebugState::default();
                                snapshot.connection = None;
                                snapshot.status.uptime_secs = 0;
                                snapshot.status.port = Some(port);
                            }
                            update_status(shared, &**handler, "running", None);
                            if let Some(ready) = ready.take() {
                                if ready.send(Ok((port, certificate))).is_err() {
                                    shared.stopping.store(true, Ordering::Release);
                                    break 'running None;
                                }
                            }
                        }
                        Event::Snapshot { mut snapshot } => {
                            last_event = Instant::now();
                            worker.owned_displays.extend(
                                snapshot
                                    .owned_display_ids
                                    .iter()
                                    .copied()
                                    .filter(|id| !worker.baseline.contains(id)),
                            );
                            let reason = snapshot.watchdog.is_stalled().then(|| {
                                snapshot.watchdog.reason().unwrap_or_else(|| {
                                    "Native operation exceeded recovery deadline".into()
                                })
                            });
                            if previous_child_sample != Some(snapshot.metrics.sample_id) {
                                previous_child_sample = Some(snapshot.metrics.sample_id);
                                metrics_sequence = metrics_sequence.saturating_add(1);
                            }
                            snapshot.metrics.sample_id = metrics_sequence;
                            let status_changed = shared.snapshot.lock().unwrap().status.state
                                != snapshot.status.state;
                            let metrics = snapshot.metrics.clone();
                            let status = snapshot.status.clone();
                            *shared.snapshot.lock().unwrap() = *snapshot;
                            handler.on_metrics(metrics);
                            if status_changed {
                                handler.on_status_change(status);
                            }
                            if let Some(reason) = reason {
                                break 'running Some(reason);
                            }
                        }
                        Event::Connection { value } => {
                            shared.snapshot.lock().unwrap().connection = match &value {
                                ConnectionEvent::Connected(info) => Some(info.clone()),
                                ConnectionEvent::Disconnected(_) => None,
                            };
                            handler.on_connection(value);
                        }
                        Event::Diagnostic { record } => diagnostic(record, &**handler),
                        Event::Failed { error } => break 'running Some(error),
                        Event::Stopped => {
                            break 'running Some("Server worker stopped unexpectedly".into())
                        }
                        Event::Updated { request, result } => {
                            if let Some((update, response)) = updates.remove(&request) {
                                if result.is_ok() {
                                    apply_update(config, &update);
                                }
                                let _ = response.send(result.map_err(anyhow::Error::msg));
                            }
                        }
                        Event::PerformanceSummary { report } => {
                            handler.on_performance_summary(report)
                        }
                        Event::MoveTarget { request, target } => {
                            if let Some(action) = requests.get_mut(&request) {
                                if let Some(own_windows) = action.own_windows.take() {
                                    let sender = shared.commands.clone();
                                    let cancelled = action.cancelled.clone();
                                    tokio::task::spawn_blocking(move || {
                                        let result = if cancelled.load(Ordering::Acquire) {
                                            Err("Window move cancelled".into())
                                        } else {
                                            own_windows(target)
                                                .map_err(|error| format!("{error:#}"))
                                        };
                                        let _ = sender.blocking_send(ParentCommand::OwnResult {
                                            id: request,
                                            result,
                                        });
                                    });
                                }
                            }
                        }
                        Event::MoveResult { request, result } => {
                            if let Some(action) = requests.remove(&request) {
                                let _ = action.result.try_send(result.map_err(anyhow::Error::msg));
                            }
                        }
                    },
                    Ok(Err(error)) => break 'running Some(format!("{error:#}")),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        break 'running Some("Server worker event reader stopped".into())
                    }
                }
            }
            if !worker_ready && launched.elapsed() >= START_TIMEOUT {
                break Some("Server worker startup exceeded its deadline".into());
            }
            if worker_ready && last_event.elapsed() >= HEARTBEAT_TIMEOUT {
                break Some("Server worker stopped reporting its health".into());
            }
            tokio::select! {
                command = commands.recv() => match command {
                    Some(ParentCommand::Stop) | None => { shared.stopping.store(true, Ordering::Release); break None; }
                    Some(ParentCommand::Update { update, result }) => {
                        let request = next_update_request;
                        next_update_request = next_update_request.wrapping_add(1);
                        if worker.commands.try_send(Command::Update { request, update: update.clone() }).is_err() {
                            let _ = result.send(Err(anyhow::anyhow!("Server worker control pipe is blocked")));
                            break Some("Server worker control pipe is blocked".into());
                        }
                        updates.insert(request, (update, result));
                    }
                    Some(ParentCommand::Move { id, request }) => {
                        if requests.is_empty() && !request.cancelled.load(Ordering::Acquire) && worker.commands.try_send(Command::MoveWindows { request: id }).is_ok() { requests.insert(id, request); }
                        else { let _ = request.result.try_send(Err(anyhow::anyhow!("Window move unavailable while server is busy"))); }
                    }
                    Some(ParentCommand::OwnResult { id, result }) => {
                        // Requests are scoped to this worker generation. Late
                        // callbacks from an old worker cannot reach a new one.
                        if requests.contains_key(&id) { let _ = worker.commands.try_send(Command::OwnWindows { request: id, result }); }
                    }
                },
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        };
        for (_, request) in requests.drain() {
            request.cancelled.store(true, Ordering::Release);
            let _ = request
                .result
                .try_send(Err(anyhow::anyhow!("Server worker is recovering")));
        }
        for (_, (_, response)) in updates.drain() {
            let _ = response.send(Err(anyhow::anyhow!(
                "Server worker stopped before settings were acknowledged"
            )));
        }
        let disconnected = { shared.snapshot.lock().unwrap().connection.take() };
        if let Some(info) = disconnected {
            let metrics = shared.snapshot.lock().unwrap().metrics.clone();
            let now = chrono::Utc::now();
            let duration_secs = chrono::DateTime::parse_from_rfc3339(&info.connected_at)
                .ok()
                .map(|at| now.signed_duration_since(at).num_seconds().max(0) as u64)
                .unwrap_or(0);
            handler.on_connection(ConnectionEvent::Disconnected(DisconnectionInfo {
                client_ip: info.client_ip,
                client_name: info.client_name,
                connected_at: info.connected_at,
                disconnected_at: now.to_rfc3339(),
                duration_secs,
                bytes_total: metrics.bytes_sent,
                reason: if failure.is_some() {
                    "native_worker_recovery"
                } else {
                    "server_stopped"
                }
                .into(),
                final_metrics: metrics,
            }));
        }
        Outcome {
            failure,
            metrics_sequence,
            next_update_request,
        }
    }
}
