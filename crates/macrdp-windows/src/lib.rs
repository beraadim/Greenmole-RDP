//! Move ordinary application windows into a virtual desktop for one RDP session.
//!
//! The Accessibility worker owns every native object and retains the virtual
//! display until restoration finishes. Activation/resizing only updates a
//! coalesced target; only a real disconnect starts a new snapshot epoch.

#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use anyhow::{ensure, Context, Result};
use macrdp_display::{DisplayBackend, DisplayController, DisplayGeometry};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod accessibility;
#[cfg(target_os = "macos")]
mod discovery;
mod geometry;
#[cfg(target_os = "macos")]
mod native;
mod process_filter;

const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default)]
struct Desired {
    epoch: u64,
    target: Option<DisplayGeometry>,
    stopping: bool,
}

#[derive(Debug, Default)]
struct State {
    desired: Desired,
    restored_epoch: u64,
    failed_restores: usize,
    stopped: bool,
    stop_deadline: Option<Instant>,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn desired(&self) -> Desired {
        self.lock().desired
    }

    fn complete_restore(&self, epoch: u64, failures: usize) {
        let mut state = self.lock();
        state.restored_epoch = epoch;
        state.failed_restores = failures;
        self.changed.notify_all();
    }

    fn stop_and_wait(&self) -> Result<()> {
        let mut state = self.lock();
        state.desired.stopping = true;
        state.desired.target = None;
        let deadline = *state
            .stop_deadline
            .get_or_insert_with(|| Instant::now() + STOP_TIMEOUT);
        self.changed.notify_all();
        let (state, _) = self
            .changed
            .wait_timeout_while(
                state,
                deadline.saturating_duration_since(Instant::now()),
                |state| !state.stopped,
            )
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure!(
            state.stopped,
            "Window restoration did not finish within 10 seconds"
        );
        ensure!(
            state.failed_restores == 0,
            "Could not restore {} application windows",
            state.failed_restores
        );
        Ok(())
    }
}

#[derive(Debug)]
struct Inner {
    shared: Arc<Shared>,
    display_id: u32,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Err(error) = self.shared.stop_and_wait() {
            tracing::warn!(%error, "Window session stopped with incomplete restoration");
        }
    }
}

/// Clones share one remote-session snapshot and a single native worker.
#[derive(Clone, Debug)]
pub struct WindowSession(Arc<Inner>);

impl WindowSession {
    /// Requires existing Accessibility permission. This never opens a prompt.
    pub fn new(display: DisplayController) -> Result<Self> {
        ensure!(
            display.backend() == DisplayBackend::Virtual,
            "Window relocation requires a virtual display"
        );
        #[cfg(not(target_os = "macos"))]
        {
            let _ = display;
            anyhow::bail!("Window relocation requires macOS");
        }
        #[cfg(target_os = "macos")]
        {
            let display_id = display.geometry()?.display_id;
            let shared = Arc::new(Shared::default());
            let worker_shared = Arc::clone(&shared);
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("macrdp-windows".to_owned())
                .spawn(move || native::run(display, worker_shared, ready_tx))
                .context("Could not start window relocation worker")?;
            let result = ready_rx
                .recv_timeout(Duration::from_secs(3))
                .context("Window relocation worker did not initialize")
                .and_then(|result| result);
            if let Err(error) = result {
                shared.lock().desired.stopping = true;
                shared.changed.notify_all();
                return Err(error);
            }
            Ok(Self(Arc::new(Inner { shared, display_id })))
        }
    }

    /// Queue the latest committed capture geometry without blocking an RDP task.
    /// Repeated activation and resize preserve the original return positions.
    pub fn update_target(&self, target: DisplayGeometry) -> Result<()> {
        ensure!(
            target.display_id == self.0.display_id && target.display_id != 0,
            "Window target does not match the owned virtual display"
        );
        ensure!(
            geometry::Frame::from(target).valid(),
            "Window target has invalid logical bounds"
        );
        let mut state = self.0.shared.lock();
        ensure!(
            !state.desired.stopping && !state.stopped,
            "Window relocation has stopped"
        );
        state.desired.target = Some(target);
        self.0.shared.changed.notify_all();
        Ok(())
    }

    /// End the actual connection. Epochs preserve this boundary across an
    /// immediately queued reconnect. Restoration happens on the native worker.
    pub fn disconnect(&self) -> Result<()> {
        let mut state = self.0.shared.lock();
        ensure!(
            !state.desired.stopping && !state.stopped,
            "Window relocation has stopped"
        );
        state.desired.target = None;
        state.desired.epoch = state
            .desired
            .epoch
            .checked_add(1)
            .context("Window session epoch exhausted")?;
        self.0.shared.changed.notify_all();
        Ok(())
    }

    /// Optional blocking barrier after disconnect; do not call while holding an
    /// async input-handler mutex. New sessions can still be queued meanwhile.
    pub fn wait_for_restoration(&self, timeout: Duration) -> Result<()> {
        let state = self.0.shared.lock();
        let epoch = state.desired.epoch;
        let (state, _) = self
            .0
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.restored_epoch < epoch && !state.stopped
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure!(
            state.restored_epoch >= epoch,
            "Timed out restoring application windows"
        );
        ensure!(
            state.failed_restores == 0,
            "Could not restore {} application windows",
            state.failed_restores
        );
        Ok(())
    }

    /// Permanently stop all clones and wait for bounded restoration. Use from a
    /// blocking task during graceful shutdown before starting another server.
    pub fn shutdown(&self) -> Result<()> {
        self.0.shared.stop_and_wait()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(width: f64) -> DisplayGeometry {
        DisplayGeometry {
            display_id: 7,
            origin_x: -1920.0,
            origin_y: 0.0,
            logical_width: width,
            logical_height: 1080.0,
            pixel_width: 1920,
            pixel_height: 1080,
        }
    }

    fn test_session() -> WindowSession {
        WindowSession(Arc::new(Inner {
            shared: Arc::new(Shared::default()),
            display_id: 7,
        }))
    }

    fn finish(session: &WindowSession) {
        session.0.shared.lock().stopped = true;
        session.0.shared.changed.notify_all();
    }

    #[test]
    fn resizing_coalesces_without_changing_the_session_epoch() {
        let session = test_session();
        session.update_target(target(1920.0)).unwrap();
        session.update_target(target(1280.0)).unwrap();
        let desired = session.0.shared.desired();
        assert_eq!(desired.epoch, 0);
        assert_eq!(desired.target.unwrap().logical_width, 1280.0);
        finish(&session);
    }

    #[test]
    fn immediate_reconnect_cannot_erase_the_disconnect_boundary() {
        let session = test_session();
        session.update_target(target(1920.0)).unwrap();
        session.disconnect().unwrap();
        session.update_target(target(1280.0)).unwrap();
        assert_eq!(session.0.shared.desired().epoch, 1);
        assert!(session.wait_for_restoration(Duration::ZERO).is_err());
        session.0.shared.complete_restore(1, 0);
        session.wait_for_restoration(Duration::ZERO).unwrap();
        finish(&session);
    }

    #[test]
    fn invalid_geometry_and_stopped_handles_cannot_move_windows() {
        let session = test_session();
        assert!(session.update_target(target(f64::NAN)).is_err());
        let mut wrong = target(100.0);
        wrong.display_id = 9;
        assert!(session.update_target(wrong).is_err());
        finish(&session);
        assert!(session.update_target(target(100.0)).is_err());
    }

    #[test]
    fn clones_only_stop_when_the_last_handle_drops() {
        let session = test_session();
        let shared = Arc::clone(&session.0.shared);
        let clone = session.clone();
        drop(session);
        assert!(!shared.desired().stopping);
        finish(&clone);
        drop(clone);
        assert!(shared.desired().stopping);
    }

    #[test]
    fn handle_is_send_and_sync_without_sharing_native_elements() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WindowSession>();
    }
}
