//! Supervision actor: retire one owner completely before launching another.
use super::policy::{allow_restart, apply_update};
use super::process::Worker;
use super::protocol::Initialization;
use super::{ParentCommand, Shared, SupervisorLease, STOP_GRACE};
use crate::{callbacks::*, tls::TlsCertificateInfo, ServerConfig};
use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

pub(super) fn update_status(
    shared: &Shared,
    handler: &dyn ServerEventHandler,
    state: &str,
    error: Option<String>,
) {
    let status = {
        let mut snapshot = shared.snapshot.lock().unwrap();
        snapshot.status.running = state == "running";
        snapshot.status.state = state.into();
        snapshot.status.error_details = error;
        if state != "running" {
            snapshot.connection = None;
        }
        snapshot.status.clone()
    };
    handler.on_status_change(status);
}

pub(super) fn diagnostic(record: String, handler: &dyn ServerEventHandler) {
    if let Ok(entry) = serde_json::from_str::<LogEntry>(&record) {
        handler.on_log(entry);
    }
    crate::log_bridge::record_forwarded_record(record.into_bytes());
}

pub(super) async fn supervise(
    mut config: ServerConfig,
    handler: Arc<dyn ServerEventHandler>,
    shared: Arc<Shared>,
    mut commands: tokio::sync::mpsc::Receiver<ParentCommand>,
    ready: tokio::sync::oneshot::Sender<Result<(u16, TlsCertificateInfo)>>,
    mut lease: SupervisorLease,
    perf_enabled: bool,
) {
    let mut ready = Some(ready);
    let mut restarts = VecDeque::new();
    let mut metrics_sequence = 0u64;
    let mut next_update_request = 1u64;
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        let baseline = match macrdp_display::list_owned_virtual_displays() {
            Ok(ids) => ids,
            Err(error) => {
                lease.safe_to_release = true;
                shared.finished.store(true, Ordering::Release);
                finish_failure(
                    &shared,
                    &*handler,
                    &mut ready,
                    format!("Cannot verify display ownership: {error:#}"),
                );
                return;
            }
        };
        let initialization = Initialization {
            managed_tls_directory: config.managed_tls_directory.clone(),
            config: config.clone(),
            baseline_display_ids: baseline,
            perf_enabled,
        };
        let mut worker = match Worker::spawn(initialization) {
            Ok(worker) => worker,
            Err(error) => {
                lease.safe_to_release = true;
                shared.finished.store(true, Ordering::Release);
                finish_failure(
                    &shared,
                    &*handler,
                    &mut ready,
                    format!("Cannot start isolated server: {error:#}"),
                );
                return;
            }
        };
        shared
            .worker_pid
            .store(worker.child.id(), Ordering::Release);
        let outcome = super::generation::Generation {
            worker: &mut worker,
            config: &mut config,
            shared: &shared,
            handler: &handler,
            commands: &mut commands,
            ready: &mut ready,
            metrics_sequence,
            next_update_request,
        }
        .run()
        .await;
        metrics_sequence = outcome.metrics_sequence;
        next_update_request = outcome.next_update_request;
        let failure = outcome.failure;
        if let Some(reason) = &failure {
            let native = shared.snapshot.lock().unwrap().watchdog.clone();
            tracing::error!(
                worker_pid = worker.child.id(),
                reason,
                operation = native.operation.as_deref().unwrap_or("none"),
                operation_elapsed_ms = native.elapsed_ms,
                operation_id = native.operation_id,
                stop_requested = native.stop_requested,
                cleanup_pending = native.cleanup_pending,
                "Server worker failure; restoring local desktop before restart"
            );
            update_status(&shared, &*handler, "recovering", Some(reason.clone()));
        } else {
            update_status(&shared, &*handler, "stopping", None);
        }
        let retirement_baseline = worker.baseline.clone();
        let cleanup_handler = handler.clone();
        let mut retiring = tokio::task::spawn_blocking(move || {
            worker.retire_with_handler(STOP_GRACE, Some(cleanup_handler))
        });
        let retirement = loop {
            tokio::select! {
                result = &mut retiring => break result,
                command = commands.recv() => match command {
                    Some(ParentCommand::Update { update, result }) => {
                        // The retiring engine is already stopping. Accept into
                        // the replacement's desired configuration immediately,
                        // including credential changes, rather than waiting on
                        // the native teardown deadline to acknowledge the UI.
                        apply_update(&mut config, &update);
                        let _ = result.send(Ok(()));
                    }
                    Some(ParentCommand::Stop) | None => { shared.stopping.store(true, Ordering::Release); }
                    Some(ParentCommand::Move { request, .. }) => { let _ = request.result.try_send(Err(anyhow::anyhow!("Server is recovering"))); }
                    Some(ParentCommand::OwnResult { .. }) => {}
                }
            }
        };
        if let Err(error) = retirement
            .context("Native worker recovery task failed")
            .and_then(|result| result)
        {
            *shared.recovery_result.lock().unwrap() = "blocked";
            finish_failure(
                &shared,
                &*handler,
                &mut ready,
                format!("Recovery incomplete; restart blocked: {error:#}"),
            );
            shared.stopping.store(true, Ordering::Release);
            update_status(
                &shared,
                &*handler,
                "cleanup_pending",
                Some(format!("{error:#}")),
            );
            // A transient WindowServer query/removal failure must not make
            // recovery permanently unavailable. The owned group has been
            // retired; retain the barrier until read-only checks prove every
            // display introduced during its lifetime has disappeared.
            loop {
                if macrdp_display::list_owned_virtual_displays()
                    .is_ok_and(|ids| ids.iter().all(|id| retirement_baseline.contains(id)))
                {
                    break;
                }
                while let Ok(command) = commands.try_recv() {
                    match command {
                        ParentCommand::Update { result, .. } => {
                            let _ = result
                                .send(Err(anyhow::anyhow!("Local desktop recovery is pending")));
                        }
                        ParentCommand::Move { request, .. } => {
                            let _ = request.result.try_send(Err(anyhow::anyhow!(
                                "Local desktop recovery is pending"
                            )));
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            *shared.terminal_error.lock().unwrap() = None;
            tracing::info!(
                "Deferred display verification completed; local desktop recovery is now safe"
            );
        }
        shared.worker_pid.store(0, Ordering::Release);
        *shared.recovery_result.lock().unwrap() = "restored";
        if shared.stopping.load(Ordering::Acquire) {
            break;
        }
        let reason = failure.unwrap_or_else(|| "Server worker stopped".into());
        if ready.is_some() {
            lease.safe_to_release = true;
            finish_failure(&shared, &*handler, &mut ready, reason);
            shared.finished.store(true, Ordering::Release);
            return;
        }
        if !allow_restart(&mut restarts, Instant::now()) {
            lease.safe_to_release = true;
            finish_failure(
                &shared,
                &*handler,
                &mut ready,
                format!("Automatic recovery stopped after repeated failures: {reason}"),
            );
            shared.finished.store(true, Ordering::Release);
            return;
        }
        shared.restart_count.fetch_add(1, Ordering::AcqRel);
        tracing::warn!(
            restart_attempt = restarts.len(),
            "Local desktop restored; restarting RDP listener for a fresh connection"
        );
        // Apply settings accepted while recovery was in progress before starting
        // the replacement, including credential changes. Stale window replies die.
        while let Ok(command) = commands.try_recv() {
            match command {
                ParentCommand::Update { update, result } => {
                    apply_update(&mut config, &update);
                    let _ = result.send(Ok(()));
                }
                ParentCommand::Stop => {
                    shared.stopping.store(true, Ordering::Release);
                }
                ParentCommand::Move { request, .. } => {
                    let _ = request
                        .result
                        .try_send(Err(anyhow::anyhow!("Server is recovering")));
                }
                ParentCommand::OwnResult { .. } => {}
            }
        }
    }
    shared.finished.store(true, Ordering::Release);
    lease.safe_to_release = true;
    update_status(&shared, &*handler, "stopped", None);
    if let Some(ready) = ready {
        let _ = ready.send(Err(anyhow::anyhow!("Server startup cancelled")));
    }
}

fn finish_failure(
    shared: &Shared,
    handler: &dyn ServerEventHandler,
    ready: &mut Option<tokio::sync::oneshot::Sender<Result<(u16, TlsCertificateInfo)>>>,
    error: String,
) {
    *shared.terminal_error.lock().unwrap() = Some(error.clone());
    update_status(shared, handler, "error", Some(error.clone()));
    if let Some(ready) = ready.take() {
        let _ = ready.send(Err(anyhow::Error::msg(error)));
    }
}
