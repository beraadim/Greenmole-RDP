//! Explicit, user-triggered window gathering. Never invoked by session hooks.

use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

#[derive(Serialize)]
pub struct WindowMoveStatus {
    available: bool,
    reason: Option<String>,
}

#[tauri::command]
pub async fn get_window_move_status(
    state: State<'_, Arc<AppState>>,
) -> Result<WindowMoveStatus, String> {
    let handle = state.server_handle.lock().await.clone();
    let reason = match handle {
        None => Some("Start the server and connect to the remote desktop first."),
        Some(handle) => handle.window_move_unavailable_reason(),
    };
    let reason = reason.or_else(|| {
        (!macrdp_core::permissions::check_permissions().accessibility)
            .then_some("Allow Accessibility access in System Settings to move windows.")
    });
    Ok(WindowMoveStatus {
        available: reason.is_none(),
        reason: reason.map(str::to_owned),
    })
}

#[tauri::command]
pub async fn move_windows_to_remote(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<macrdp_core::WindowMoveReport, String> {
    // Also guard the backend: two webviews or duplicate IPC calls must not
    // enqueue overlapping requests or delay disconnect with repeated actions.
    let _busy = state
        .window_move_lock
        .try_lock()
        .map_err(|_| "Windows are already being moved. Please wait.".to_owned())?;
    let handle = state
        .server_handle
        .lock()
        .await
        .clone()
        .ok_or_else(|| "The server is not running.".to_owned())?;
    let request = handle
        .queue_move_windows_to_remote_with(move |target| {
            #[cfg(target_os = "macos")]
            {
                crate::own_windows::move_main_window(&app, target)
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (app, target);
                anyhow::bail!("Moving windows requires macOS")
            }
        })
        .map_err(|error| format!("{error:#}"))?;
    tracing::info!("User requested moving windows to the remote display");
    let result = tokio::task::spawn_blocking(move || request.wait())
        .await
        .map_err(|error| format!("Window move task failed: {error}"))?
        .map_err(|error| format!("{error:#}"));
    if let Err(error) = &result {
        tracing::warn!(%error, "Manual remote window move failed");
    }
    result
}
