use std::sync::Arc;

use tauri::Emitter;
use tauri::Manager;
use tauri::State;

use serde::Serialize;

use crate::database::Database;
use crate::event_bridge::TauriEventBridge;
use crate::state::AppState;

pub async fn ensure_server_running(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
) -> Result<(), String> {
    {
        let guard = state.server_handle.lock().await;
        if guard.is_some() {
            tracing::info!("Server already running");
            return Ok(());
        }
    }

    {
        let mut status = state.status.lock().await;
        status.running = false;
        status.state = "starting".to_string();
    }
    let _ = app.emit("server-status", &*state.status.lock().await);

    let config = state.ui_config.lock().await.to_server_config();
    let bridge = TauriEventBridge::new(app.clone());

    let handle = macrdp_core::start_server(config, bridge)
        .await
        .map_err(|e| format!("Failed to start server: {}", e))?;

    let port = handle.port();
    tracing::info!(port, "Server started successfully");

    let mut guard = state.server_handle.lock().await;
    *guard = Some(handle);

    Ok(())
}

/// Response from `set_config` indicating whether the server must be restarted.
#[derive(Debug, Clone, Serialize)]
pub struct SetConfigResponse {
    pub restart_required: bool,
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

    // Take the handle out
    let handle = {
        let mut guard = state.server_handle.lock().await;
        guard.take()
    };

    if let Some(handle) = handle {
        handle
            .stop()
            .await
            .map_err(|e| format!("Failed to stop server: {}", e))?;
    }

    // Update status
    {
        let mut status = state.status.lock().await;
        status.running = false;
        status.state = "stopped".to_string();
        status.uptime_secs = 0;
        status.port = None;
        status.bind_address = None;
    }
    let _ = app.emit(
        "server-status",
        &*state.status.lock().await,
    );

    // Clear metrics and connections
    {
        let mut metrics = state.metrics.lock().await;
        *metrics = macrdp_core::Metrics::default();
    }
    {
        let mut connections = state.connections.lock().await;
        connections.clear();
    }

    Ok(())
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
pub async fn get_metrics(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    // If we have a live handle, refresh from it so polling fallback works even
    // if an event update was missed.
    {
        let guard = state.server_handle.lock().await;
        if let Some(ref handle) = *guard {
            let live_metrics = handle.metrics();
            let mut metrics = state.metrics.lock().await;
            *metrics = live_metrics;
        }
    }
    let metrics = state.metrics.lock().await.clone();
    serde_json::to_value(&metrics).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_connections(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
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
pub async fn get_config(
    state: State<'_, Arc<AppState>>,
) -> Result<serde_json::Value, String> {
    let config = state.ui_config.lock().await.clone();
    serde_json::to_value(&config).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_config(
    app: tauri::AppHandle,
    key: String,
    value: serde_json::Value,
    state: State<'_, Arc<AppState>>,
) -> Result<SetConfigResponse, String> {
    tracing::info!("set_config: key={key}, value={value}");

    let restart_required = {
        let mut config = state.ui_config.lock().await;
        let restart = config.set_field(&key, &value)?;
        config.save()?;
        restart
    };

    if key == "log_level" {
        let level = {
            let config = state.ui_config.lock().await;
            config.log_level.clone()
        };
        let filter = format!("macrdp_ui={level},macrdp_core={level},{level}");
        state
            .log_reload
            .reload(tracing_subscriber::EnvFilter::new(&filter))
            .map_err(|e| format!("failed to reload log level: {e}"))?;
        tracing::info!(%level, "Reloaded tracing filter");
    }

    // Hot-update: push changes to running server
    {
        let guard = state.server_handle.lock().await;
        if let Some(handle) = guard.as_ref() {
            let config = state.ui_config.lock().await;
            match key.as_str() {
                "frame_rate" => handle.update_config(macrdp_core::ConfigUpdate::FrameRate(config.frame_rate)),
                "bitrate_mbps" => handle.update_config(macrdp_core::ConfigUpdate::BitrateKbps(config.bitrate_mbps * 1000)),
                "log_level" => handle.update_config(macrdp_core::ConfigUpdate::LogLevel(config.log_level.clone())),
                "show_cursor" => handle.update_config(macrdp_core::ConfigUpdate::ShowCursor(config.show_cursor)),
                "resolution" | "hidpi_scale" => handle.update_config(macrdp_core::ConfigUpdate::Resolution(config.resolution.clone())),
                "encoder" => handle.update_config(macrdp_core::ConfigUpdate::Encoder(config.encoder.clone())),
                "chroma_mode" => handle.update_config(macrdp_core::ConfigUpdate::ChromaMode(config.chroma_mode.clone())),
                "gfx_validation_mode" => handle.update_config(macrdp_core::ConfigUpdate::GfxValidationMode(config.gfx_validation_mode)),
                "username" | "password" => handle.update_config(macrdp_core::ConfigUpdate::Credentials {
                    username: config.username.clone(),
                    password: config.password.clone(),
                }),
                // port: auto-restart needed (can't rebind TCP listener)
                "port" => { /* handled below */ }
                _ => {}
            }
        }
    }

    // Seamless port migration: start new port first, then stop old
    if key == "port" {
        let old_handle = state.server_handle.lock().await.clone();
        if old_handle.is_some() {
            tracing::info!("Port changed, starting new listener before releasing old");
            let config = state.ui_config.lock().await.to_server_config();
            let bridge = TauriEventBridge::new(app.clone());
            match macrdp_core::start_server(config, bridge).await {
                Ok(new_handle) => {
                    let new_port = new_handle.port();
                    // Swap: store new handle, then stop old
                    let old = {
                        let mut guard = state.server_handle.lock().await;
                        guard.replace(new_handle)
                    };
                    if let Some(old) = old {
                        let old_port = old.port();
                        let _ = old.stop().await;
                        tracing::info!(old_port, new_port, "Seamless port migration complete");
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to start on new port: {e}, keeping old");
                }
            }
        }
    }

    Ok(SetConfigResponse { restart_required })
}

#[tauri::command]
pub async fn get_logs(
    limit: Option<usize>,
) -> Result<Vec<serde_json::Value>, String> {
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
    Ok(serde_json::json!({
        "current_version": env!("CARGO_PKG_VERSION"),
        "available": false,
    }))
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
pub fn set_autostart(
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
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
