//! Local recovery does not require the webview or the remote desktop to respond.

use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;
use tauri::{Emitter, Manager};

use crate::state::AppState;

#[cfg(target_os = "macos")]
mod control;

const COMMAND: &str = "--recover-local-desktop";

/// Run before Tauri's single-instance handling: a second invocation must request
/// recovery, rather than merely focusing a window on the unavailable display.
pub(crate) fn run_command_if_requested() -> Option<Result<(), String>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(COMMAND)) {
        return None;
    }
    if args.next().is_some() {
        return Some(Err(
            "Desktop recovery does not accept extra arguments.".into()
        ));
    }
    #[cfg(target_os = "macos")]
    {
        Some(control::request_recovery().map(|()| {
            eprintln!("The server stopped and local desktop recovery completed.");
        }))
    }
    #[cfg(not(target_os = "macos"))]
    Some(Err("Local desktop recovery requires macOS.".into()))
}

pub(crate) fn install_control_endpoint(app: &tauri::AppHandle) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let app_handle = app.clone();
        let endpoint = control::Endpoint::install(move || {
            let (reply, result) = std::sync::mpsc::sync_channel(1);
            let app = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                let _ = reply.send(recover_local_desktop(&app).await);
            });
            result
                .recv_timeout(Duration::from_secs(80))
                .unwrap_or_else(|_| {
                    Err("Recovery is still pending. Check macrdp's local controls and logs.".into())
                })
        })?;
        app.manage(endpoint);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    Ok(())
}

/// Stop intent precedes the lifecycle lock, so a pending startup is cancelled
/// before recovery waits for it. A supervised worker can be terminated without
/// freeing any native object while callbacks still execute in this process.
pub(crate) async fn recover_local_desktop(app: &tauri::AppHandle) -> Result<(), String> {
    let result = recover_local_desktop_inner(app).await;
    if let Err(error) = &result {
        // Native-menu and command-line callers have no component-local error
        // state. Keep partial recovery visible in the local dashboard as well.
        if let Some(state) = app.try_state::<Arc<AppState>>() {
            let mut status = state.status.lock().await;
            status.error_details = Some(error.clone());
            let _ = app.emit("server-status", &*status);
        }
    }
    result
}

async fn recover_local_desktop_inner(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<Arc<AppState>>()
        .ok_or("macrdp is still initializing. Try recovery again shortly.")?;
    state
        .stop_requests
        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    let _lifecycle = tokio::time::timeout(Duration::from_secs(15), state.lifecycle_lock.lock())
        .await
        .map_err(|_| {
            "Recovery requested; a service transition is still pending. Try again shortly."
        })?;
    state.server_generation.fetch_add(1, Ordering::AcqRel);
    tracing::warn!("User requested local desktop recovery and server stop");
    {
        let mut status = state.status.lock().await;
        status.running = false;
        status.state = "stopping".into();
        status.error_details = None;
        let _ = app.emit("server-status", &*status);
    }

    let handle = state.server_handle.lock().await.clone();
    let fallback_private = if handle.is_none() {
        state
            .running_config
            .lock()
            .await
            .as_ref()
            .is_some_and(|config| config.private_session)
            || state.ui_config.lock().await.private_session
    } else {
        false
    };
    let private_recovery = private_recovery_required(
        handle.as_ref().map(|handle| handle.private_session()),
        fallback_private,
    );
    if private_recovery && handle.is_none() {
        tokio::task::spawn_blocking(macrdp_core::lock_session_and_verify)
            .await
            .map_err(|error| format!("Private recovery lock task failed: {error}"))?
            .map_err(|error| {
                format!("Private recovery is waiting for a verified macOS lock: {error:#}")
            })?;
    }
    let result = match &handle {
        Some(handle) => handle
            .recover_local_desktop()
            .await
            .map_err(|e| format!("{e:#}")),
        None => tokio::time::timeout(Duration::from_secs(50), async {
            // Startup cancellation can retire an unpublished worker in the
            // background. Its missing UI handle does not mean cleanup finished.
            while macrdp_core::server_resources_pending() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| "Local recovery is still waiting for cancelled startup cleanup.".to_owned()),
    };
    if let Err(error) = result {
        let status = macrdp_core::ServerStatus {
            running: false,
            state: "cleanup_pending".into(),
            error_details: Some(format!("Local recovery could not finish: {error}")),
            ..Default::default()
        };
        *state.status.lock().await = status.clone();
        let _ = app.emit("server-status", &status);
        // Showing controls on a physical screen remains useful even if the
        // WindowServer did not acknowledge virtual-display removal.
        if !private_recovery {
            let _ = crate::tray::show_main_on_physical_display(app).await;
        }
        return Err(error);
    }

    state.server_handle.lock().await.take();
    *state.running_config.lock().await = None;
    *state.status.lock().await = Default::default();
    *state.metrics.lock().await = Default::default();
    state.connections.lock().await.clear();
    crate::tray::update_tray_status(app, "stopped", &macrdp_core::Metrics::default());
    let _ = app.emit("server-status", &*state.status.lock().await);
    let _ = app.emit("metrics", macrdp_core::Metrics::default());
    let _ = app.emit("connections", Vec::<crate::state::Connection>::new());

    if private_recovery {
        // The guard/supervisor has verified locking before releasing local
        // protection. Do not activate windows or run accessibility moves over
        // that login screen. The user must authenticate locally to continue.
        tracing::info!("Private local recovery completed; Mac remains locked and server stopped");
        return Ok(());
    }

    // Removing the owned virtual display normally brings its windows back. The
    // bounded accessibility pass only gathers windows still unreachable on all
    // surviving displays; it preserves healthy multi-monitor arrangements.
    #[cfg(target_os = "macos")]
    {
        let windows =
            tokio::task::spawn_blocking(macrdp_core::recover_stranded_windows_on_physical_display)
                .await
                .map_err(|e| format!("Local window recovery task failed: {e}"))?;
        match windows {
            Ok(report) => {
                tracing::info!(
                    moved = report.moved,
                    skipped = report.skipped,
                    failed = report.failed,
                    app_failures = report.app_failures,
                    timed_out = report.timed_out,
                    "Local desktop window recovery pass completed"
                );
                if window_recovery_incomplete(&report) {
                    let _ = crate::tray::show_main_on_physical_display(app).await;
                    return Err("The server stopped and the remote display was released. Some windows could not be inspected or moved; check the local desktop and try recovery again if needed.".into());
                }
            }
            Err(error) => {
                tracing::warn!(%error, "Server stopped, but local window recovery was incomplete");
                let _ = crate::tray::show_main_on_physical_display(app).await;
                return Err(format!(
                    "The server stopped. Some windows could not be recovered: {error:#}"
                ));
            }
        }
    }
    crate::tray::show_main_on_physical_display(app).await?;
    tracing::info!("Local desktop recovery completed; server remains stopped");
    Ok(())
}

/// Saved edits are deferred until restart. Only an absent worker may fall back
/// to the last known/preferences policy for cancellation or stranded recovery.
fn private_recovery_required(worker_policy: Option<bool>, fallback_policy: bool) -> bool {
    worker_policy.unwrap_or(fallback_policy)
}

#[cfg(any(target_os = "macos", test))]
fn window_recovery_incomplete(report: &macrdp_core::WindowMoveReport) -> bool {
    report.failed > 0 || report.app_failures > 0 || report.timed_out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_uses_worker_policy_despite_pending_enable_or_disable() {
        // Pending enable must not claim that an ordinary worker locked the Mac.
        assert!(!private_recovery_required(Some(false), true));
        // Pending disable must not bypass an existing worker's guarded recovery.
        assert!(private_recovery_required(Some(true), false));
        assert!(private_recovery_required(Some(true), true));
        assert!(!private_recovery_required(Some(false), false));
        // No published handle can also mean cancelled startup is still retiring.
        assert!(private_recovery_required(None, true));
        assert!(!private_recovery_required(None, false));
    }

    #[test]
    fn recovery_reports_failures_and_deadline_without_promising_all_windows_returned() {
        let mut report = macrdp_core::WindowMoveReport {
            target: macrdp_core::DisplayGeometry {
                display_id: 1,
                origin_x: 0.0,
                origin_y: 0.0,
                logical_width: 1920.0,
                logical_height: 1080.0,
                pixel_width: 1920,
                pixel_height: 1080,
            },
            moved: 1,
            already_visible: 3,
            skipped: 2,
            failed: 0,
            app_failures: 0,
            timed_out: false,
        };
        // Full-screen/minimized windows are deliberately skipped, not failures.
        assert!(!window_recovery_incomplete(&report));
        report.failed = 1;
        assert!(window_recovery_incomplete(&report));
        report.failed = 0;
        report.app_failures = 1;
        assert!(window_recovery_incomplete(&report));
        report.app_failures = 0;
        report.timed_out = true;
        assert!(window_recovery_incomplete(&report));
    }
}
