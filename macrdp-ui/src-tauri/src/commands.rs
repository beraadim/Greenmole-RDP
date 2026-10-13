use std::sync::{atomic::Ordering, Arc};

use tauri::Manager;
use tauri::State;

use crate::database::Database;
use crate::state::AppState;

pub(crate) use crate::server_lifecycle::{ensure_server_running, stop_running_server};

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
    state
        .stop_requests
        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    let _lifecycle = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.lifecycle_lock.lock(),
    )
    .await
    .map_err(|_| "A service transition is still pending; Stop will remain available.")?;
    stop_running_server(&app, state.inner()).await
}

#[tauri::command]
pub async fn get_server_status(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    if let Ok(_lifecycle) = state.lifecycle_lock.try_lock() {
        let handle = state.server_handle.lock().await.as_ref().cloned();
        if let Some(handle) = handle {
            *state.status.lock().await = handle.status();
            if handle.cleanup_complete() {
                state.server_handle.lock().await.take();
                *state.running_config.lock().await = None;
            }
        } else {
            let mut status = state.status.lock().await;
            if macrdp_core::server_resources_pending() {
                status.state = "cleanup_pending".into();
                status.running = false;
            } else if status.state == "cleanup_pending" {
                *status = Default::default();
            }
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
pub async fn request_permissions(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    if macrdp_core::server_resources_pending()
        || state
            .server_handle
            .lock()
            .await
            .as_ref()
            .is_some_and(|handle| !handle.cleanup_complete())
    {
        return Err(
            "Stop the server and wait for display cleanup before requesting permissions.".into(),
        );
    }
    let perms = tokio::task::spawn_blocking(macrdp_core::permissions::request_permissions)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(perms).map_err(|error| error.to_string())
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
    let configured = state.ui_config.lock().await.to_server_config();
    let cert_path = configured.cert_path;
    let directory = configured.managed_tls_directory.expect("GUI TLS directory");
    let result = tokio::task::spawn_blocking(move || {
        macrdp_core::tls::inspect_configured_tls_certificate_at(&directory, cert_path.as_deref())
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
    state
        .stop_requests
        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    let _lifecycle = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        state.lifecycle_lock.lock(),
    )
    .await
    .ok();
    state.server_generation.fetch_add(1, Ordering::AcqRel);
    // Process exit is the final recovery boundary: its display helper observes
    // closed stdin and restores/removes the owned virtual display.
    let handle = state.server_handle.lock().await.as_ref().cloned();
    if let Some(handle) = handle {
        match handle.stop().await {
            Ok(macrdp_core::StopOutcome::Pending { reason }) => {
                tracing::warn!(reason, "Exiting with native cleanup still pending")
            }
            Err(error) => tracing::warn!(%error, "Error stopping server during quit"),
            Ok(macrdp_core::StopOutcome::Complete) => {}
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
