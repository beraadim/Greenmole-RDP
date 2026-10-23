//! Window close hides the controls; native reopen reveals them. An application
//! quit is a separate transition that retires the supervised server first.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tauri::Manager;

use crate::state::AppState;

#[derive(Default)]
pub(crate) struct ExitState {
    requested: AtomicBool,
    permitted: AtomicBool,
}

impl ExitState {
    fn begin(&self) -> bool {
        !self.requested.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn permitted(&self) -> bool {
        self.permitted.load(Ordering::Acquire)
    }

    fn permit(&self) {
        self.permitted.store(true, Ordering::Release);
    }
}

pub(crate) fn request_show_main(app: &tauri::AppHandle, source: &'static str) {
    if app
        .try_state::<Arc<AppState>>()
        .is_some_and(|state| state.exit.requested())
    {
        return;
    }
    tracing::info!(source, "Requested reopening Greenmole RDP controls");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = crate::tray::show_main_window(&app).await {
            tracing::warn!(%error, source, "Could not reveal Greenmole RDP controls");
        }
    });
}

/// Finder/Dock activation reopens the existing macOS process. It does not
/// necessarily create a second process for the single-instance plugin to see.
pub(crate) fn handle_run_event(app: &tauri::AppHandle, event: tauri::RunEvent) {
    match event {
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => {
            // A visible tray popover can make has_visible_windows true while
            // the main window is hidden. Always reveal the requested controls.
            request_show_main(app, "macos_reopen");
        }
        tauri::RunEvent::ExitRequested { api, .. } => {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                if !state.exit.permitted() {
                    api.prevent_exit();
                    if let Err(error) = request_quit(app) {
                        tracing::error!(%error, "Could not begin orderly application quit");
                    }
                }
            }
        }
        _ => {}
    }
}

/// Native menu and frontend quit requests share one shutdown. Persist Stop
/// intent before waiting on lifecycle work, including a cancelled startup that
/// has not yet published a handle. No new server may start after this point.
pub(crate) fn request_quit(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<Arc<AppState>>()
        .ok_or("Greenmole RDP is still initializing. Please try Quit again shortly.")?;
    if !state.exit.begin() {
        return Ok(());
    }
    state
        .stop_requests
        .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    let state = Arc::clone(state.inner());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tracing::info!("Application quit requested; stopping supervised server");
        let _lifecycle = tokio::time::timeout(Duration::from_secs(5), state.lifecycle_lock.lock())
            .await
            .ok();
        state.server_generation.fetch_add(1, Ordering::AcqRel);
        let handle = state.server_handle.lock().await.clone();
        if let Some(handle) = handle {
            if let Err(error) = handle.recover_local_desktop().await {
                tracing::warn!(%error, "Exiting after supervised shutdown deadline");
            }
        } else {
            // An unpublished worker still owns its cleanup barrier after a
            // cancelled startup. Give it the same bounded shutdown opportunity.
            if tokio::time::timeout(Duration::from_secs(50), async {
                while macrdp_core::server_resources_pending() {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .is_err()
            {
                tracing::warn!("Exiting while cancelled startup cleanup remains pending");
            }
        }
        state.exit.permit();
        app.exit(0);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_native_and_frontend_quit_requests_share_shutdown_and_gate_exit() {
        let state = Arc::new(ExitState::default());
        assert!(!state.requested());
        assert!(!state.permitted());
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || state.begin())
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
        assert!(state.requested());
        // A duplicate Quit, window reopen or queued Start cannot authorize an
        // abrupt exit before the shared shutdown reaches its boundary.
        assert!(!state.begin());
        assert!(!state.permitted());
        state.permit();
        assert!(state.permitted());
        assert!(!state.begin());
    }
}
