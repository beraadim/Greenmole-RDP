//! Serialized service transitions with persistent Stop intent and retained native ownership.
use crate::{event_bridge::TauriEventBridge, state::AppState};
use std::sync::{atomic::Ordering, Arc};
use tauri::Emitter;

async fn cancellable_start<T>(
    stop: &mut tokio::sync::watch::Receiver<u64>,
    start: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::select! {
        biased;
        _ = stop.changed() => Err(anyhow::anyhow!("Server startup cancelled")),
        result = start => result,
    }
}

pub async fn ensure_server_running(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
) -> Result<(), String> {
    if state.exit.requested() {
        return Err("Greenmole RDP is quitting.".into());
    }
    let mut stop_requests = state.stop_requests.subscribe();
    let stop_epoch = *stop_requests.borrow_and_update();
    let _lifecycle = state.lifecycle_lock.lock().await;
    if state.exit.requested() {
        return Err("Greenmole RDP is quitting.".into());
    }
    if *stop_requests.borrow() != stop_epoch {
        return Err("Server startup cancelled by Stop".into());
    }
    let previous = state.server_handle.lock().await.as_ref().cloned();
    if let Some(previous) = previous {
        if previous.status().running {
            return Ok(());
        }
        match previous.stop().await {
            Ok(macrdp_core::StopOutcome::Pending { reason }) => {
                let status = previous.status();
                *state.status.lock().await = status.clone();
                let _ = app.emit("server-status", &status);
                return Err(format!("Previous server cleanup is pending: {reason}"));
            }
            Err(error) if !previous.cleanup_complete() => {
                return Err(format!("Previous server cleanup is still pending: {error}"));
            }
            Err(error) => tracing::warn!(%error, "Reaped a failed server before restarting"),
            Ok(macrdp_core::StopOutcome::Complete) => {}
        }
        state.server_handle.lock().await.take();
    }
    *state.running_config.lock().await = None;
    let generation = state.server_generation.fetch_add(1, Ordering::AcqRel) + 1;

    {
        let mut status = state.status.lock().await;
        status.running = false;
        status.state = "starting".to_string();
        status.error_details = None;
    }
    let _ = app.emit("server-status", &*state.status.lock().await);

    let started_config = state.ui_config.lock().await.clone();
    let config = started_config.to_server_config();
    let bridge = TauriEventBridge::new(app.clone(), generation);
    let started = cancellable_start(
        &mut stop_requests,
        macrdp_core::supervised::start_supervised_server(config, bridge),
    )
    .await;
    let handle = match started {
        Ok(handle) => handle,
        Err(error) => {
            // Invalidate any queued startup events before publishing the failure.
            state.server_generation.fetch_add(1, Ordering::AcqRel);
            let mut status = state.status.lock().await;
            *status = macrdp_core::ServerStatus {
                state: "error: RDP server startup failed".into(),
                error_details: Some(format!("{error:#}")),
                ..Default::default()
            };
            let _ = app.emit("server-status", &*status);
            crate::tray::update_tray_status(app, "stopped", &macrdp_core::Metrics::default());
            return Err(format!("Failed to start server: {error:#}"));
        }
    };

    // A fresh worker started from the durable configuration, so ambiguous
    // acknowledgements from its predecessor no longer require a retry.
    state.unconfirmed_graphics.lock().await.clear();
    if *stop_requests.borrow() != stop_epoch {
        *state.server_handle.lock().await = Some(handle);
        *state.running_config.lock().await = Some(started_config);
        stop_running_server(app, state).await?;
        return Err("Server startup cancelled by Stop".into());
    }
    let port = handle.port();
    tracing::info!(port, "Server started successfully");

    let mut guard = state.server_handle.lock().await;
    *guard = Some(handle);
    *state.running_config.lock().await = Some(started_config);

    Ok(())
}

/// Caller holds the lifecycle lock, including settings failures that require a safe stop.
pub(crate) async fn stop_running_server(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
) -> Result<(), String> {
    state.server_generation.fetch_add(1, Ordering::AcqRel);

    // Keep ownership visible until both the server thread and native cleanup end.
    let handle = state.server_handle.lock().await.as_ref().cloned();
    let stop_result = if let Some(handle) = &handle {
        match handle.stop().await {
            Ok(macrdp_core::StopOutcome::Pending { reason }) => {
                let mut status = handle.status();
                status.error_details = Some(format!(
                    "Cleanup pending: {reason}. Wait before restarting."
                ));
                *state.status.lock().await = status.clone();
                let _ = app.emit("server-status", &status);
                crate::tray::update_tray_status(
                    app,
                    &status.state,
                    &macrdp_core::Metrics::default(),
                );
                return Ok(());
            }
            Err(error) if !handle.cleanup_complete() => {
                let status = handle.status();
                *state.status.lock().await = status.clone();
                let _ = app.emit("server-status", &status);
                return Err(format!("Cleanup is still pending: {error}"));
            }
            Err(error) => Err(format!("Failed to stop server: {error}")),
            Ok(macrdp_core::StopOutcome::Complete) => Ok(()),
        }
    } else if macrdp_core::server_resources_pending() {
        let status = macrdp_core::ServerStatus {
            state: "cleanup_pending".into(),
            error_details: Some(
                "Cancelled startup is still cleaning up. Wait before restarting.".into(),
            ),
            ..Default::default()
        };
        *state.status.lock().await = status.clone();
        let _ = app.emit("server-status", &status);
        return Ok(());
    } else {
        Ok(())
    };
    state.server_handle.lock().await.take();
    *state.running_config.lock().await = None;
    {
        let mut status = state.status.lock().await;
        *status = macrdp_core::ServerStatus {
            error_details: stop_result.as_ref().err().cloned(),
            ..Default::default()
        };
    }
    let _ = app.emit("server-status", &*state.status.lock().await);

    // Clear metrics and connections
    {
        let mut metrics = state.metrics.lock().await;
        *metrics = macrdp_core::Metrics::default();
    }
    {
        let mut connections = state.connections.lock().await;
        connections.clear();
    }

    crate::tray::update_tray_status(app, "stopped", &macrdp_core::Metrics::default());
    let _ = app.emit("metrics", &macrdp_core::Metrics::default());
    let _ = app.emit("connections", Vec::<crate::state::Connection>::new());
    stop_result
}

#[cfg(test)]
mod tests;
