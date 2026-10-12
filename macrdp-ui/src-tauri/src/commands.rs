use std::sync::{atomic::Ordering, Arc};

use tauri::Emitter;
use tauri::Manager;
use tauri::State;

use crate::database::Database;
use crate::event_bridge::TauriEventBridge;
use crate::state::AppState;

pub async fn ensure_server_running(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
) -> Result<(), String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    let previous = {
        let mut guard = state.server_handle.lock().await;
        if guard.as_ref().is_some_and(|handle| handle.status().running) {
            return Ok(());
        }
        guard.take()
    };
    *state.running_config.lock().await = None;
    let generation = state.server_generation.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(previous) = previous {
        if let Err(error) = previous.stop().await {
            tracing::warn!(%error, "Reaped a failed server before restarting");
        }
    }

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

    let handle = match macrdp_core::start_server(config, bridge).await {
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

    let port = handle.port();
    tracing::info!(port, "Server started successfully");

    let mut guard = state.server_handle.lock().await;
    *guard = Some(handle);
    *state.running_config.lock().await = Some(started_config);

    Ok(())
}

#[tauri::command]
pub async fn start_server(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("start_server called");
    ensure_server_running(&app, state.inner()).await
}

#[tauri::command]
pub async fn stop_server(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    tracing::info!("stop_server called");
    let _lifecycle = state.lifecycle_lock.lock().await;
    stop_running_server(&app, state.inner()).await
}

/// Caller holds the lifecycle lock, including settings failures that require a safe stop.
pub(crate) async fn stop_running_server(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
) -> Result<(), String> {
    state.server_generation.fetch_add(1, Ordering::AcqRel);

    // Take the handle out
    let handle = {
        let mut guard = state.server_handle.lock().await;
        guard.take()
    };

    let stop_result = if let Some(handle) = handle {
        handle
            .stop()
            .await
            .map_err(|error| format!("Failed to stop server: {error}"))
    } else {
        Ok(())
    };

    *state.running_config.lock().await = None;

    // Update status
    {
        let mut status = state.status.lock().await;
        status.running = false;
        status.state = "stopped".to_string();
        status.error_details = stop_result.as_ref().err().cloned();
        status.uptime_secs = 0;
        status.port = None;
        status.bind_address = None;
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

#[tauri::command]
pub async fn get_server_status(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    // If we have a live handle, refresh from it
    {
        let guard = state.server_handle.lock().await;
        if let Some(ref handle) = *guard {
            let live_status = handle.status();
            let mut status = state.status.lock().await;
            *status = live_status;
        }
    }
    let status = state.status.lock().await.clone();
    serde_json::to_value(&status).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_metrics(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    // Return the live ticker snapshot without writing the callback cache: an
    // event can publish a newer sample while this polling request is in flight.
    let guard = state.server_handle.lock().await;
    let metrics = guard
        .as_ref()
        .filter(|handle| handle.status().running)
        .map(|handle| handle.metrics())
        .unwrap_or_default();
    serde_json::to_value(&metrics).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_connections(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let live_connections = {
        let guard = state.server_handle.lock().await;
        if let Some(ref handle) = *guard {
            if let Some(connection) = handle.active_connection() {
                vec![crate::state::Connection {
                    client_ip: connection.client_ip,
                    client_name: connection.client_name,
                    connected_at: connection.connected_at,
                    bytes_total: handle.bytes_sent(),
                }]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    };
    {
        let mut connections = state.connections.lock().await;
        *connections = live_connections.clone();
    }
    let connections = state.connections.lock().await.clone();
    serde_json::to_value(&connections).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_permissions() -> Result<serde_json::Value, String> {
    let perms = macrdp_core::permissions::check_permissions();
    serde_json::to_value(&perms).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_input_debug_state(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let debug_state = {
        let guard = state.server_handle.lock().await;
        guard
            .as_ref()
            .map(|handle| handle.input_debug_state())
            .unwrap_or_default()
    };
    serde_json::to_value(&debug_state).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_tls_status(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    {
        let handles = state.server_handle.lock().await;
        if let Some(handle) = handles.as_ref().filter(|handle| handle.status().running) {
            return Ok(
                serde_json::json!({"certificate": handle.tls_certificate(), "source": "running", "error": null}),
            );
        }
    }
    let cert_path = state.ui_config.lock().await.to_server_config().cert_path;
    let result = tokio::task::spawn_blocking(move || {
        macrdp_core::tls::inspect_configured_tls_certificate(cert_path.as_deref())
    })
    .await
    .map_err(|error| error.to_string())?;
    Ok(match result {
        Ok(certificate) => {
            serde_json::json!({"certificate": certificate, "source": "configured", "error": null})
        }
        Err(error) => {
            serde_json::json!({"certificate": null, "source": "configured", "error": error.to_string()})
        }
    })
}

#[tauri::command]
pub async fn get_logs(limit: Option<usize>) -> Result<Vec<serde_json::Value>, String> {
    let limit = limit.unwrap_or(500);

    // Read from log file (lock-free read, writer holds its own lock)
    let Some(path) = macrdp_core::log_file_path() else {
        return Ok(Vec::new());
    };
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let entries: Vec<serde_json::Value> = content
        .lines()
        .rev()
        .take(limit)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(entries)
}

#[tauri::command]
pub fn get_connection_history(
    limit: Option<u32>,
    offset: Option<u32>,
    db: State<'_, Arc<Database>>,
) -> Result<Vec<serde_json::Value>, String> {
    db.get_connection_history(limit.unwrap_or(50), offset.unwrap_or(0))
}

#[tauri::command]
pub fn get_traffic_stats(
    days: Option<u32>,
    db: State<'_, Arc<Database>>,
) -> Result<Vec<serde_json::Value>, String> {
    db.get_traffic_stats(days.unwrap_or(30))
}

#[tauri::command]
pub fn check_for_updates() -> Result<serde_json::Value, String> {
    Err("Automatic update checks are unavailable. Use the Releases link in About.".to_string())
}

#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.show().map_err(|e: tauri::Error| e.to_string())?;
        window
            .set_focus()
            .map_err(|e: tauri::Error| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn open_system_preferences(pane: String) -> Result<(), String> {
    crate::permissions::open_system_preferences(&pane)
}

#[tauri::command]
pub async fn quit_app(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    state.server_generation.fetch_add(1, Ordering::AcqRel);
    // Stop the server if running
    let handle = {
        let mut guard = state.server_handle.lock().await;
        guard.take()
    };

    if let Some(handle) = handle {
        if let Err(e) = handle.stop().await {
            tracing::warn!("Error stopping server during quit: {}", e);
        }
    }

    app.exit(0);
    Ok(())
}

#[tauri::command]
pub fn set_autostart(enabled: bool, app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch
            .enable()
            .map_err(|e| format!("Failed to enable autostart: {}", e))?;
    } else {
        autolaunch
            .disable()
            .map_err(|e| format!("Failed to disable autostart: {}", e))?;
    }
    Ok(())
}
