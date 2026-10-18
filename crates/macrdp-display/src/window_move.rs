//! One-shot window requests share the display worker's connection ordering.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::{DisplayGeometry, REQUEST_TIMEOUT};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Counts apply to the windows inspected before the operation's time limit.
/// Minimized, fullscreen, and unsupported windows retain their existing state.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WindowMoveReport {
    pub target: DisplayGeometry,
    pub moved: usize,
    pub already_visible: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Applications whose window list could not be read; their window count is unknown.
    pub app_failures: usize,
    pub timed_out: bool,
}

/// Wait on a blocking task. Dropping this cancels work not yet performed.
pub struct WindowMovePreparation {
    receiver: mpsc::Receiver<Result<WindowMoveReport>>,
    cancelled: Arc<AtomicBool>,
}

impl WindowMovePreparation {
    pub fn wait(self) -> Result<WindowMoveReport> {
        self.receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .context("Timed out moving windows to the remote display")?
    }
}

impl Drop for WindowMovePreparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct Budget {
    pub deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl Budget {
    /// Each automatic session phase gets a new deadline; retained native
    /// references must never reuse the deadline from the original snapshot.
    pub(crate) fn for_duration(duration: Duration) -> Self {
        Self {
            deadline: std::time::Instant::now() + duration,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn expired(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }

    pub fn check(&self) -> Result<()> {
        ensure!(
            !self.expired(),
            "Window move request expired or was cancelled"
        );
        Ok(())
    }
}

pub(crate) struct Request {
    pub budget: Budget,
    pub own_windows: Box<dyn FnOnce(DisplayGeometry) -> Result<usize> + Send>,
    pub reply: mpsc::Sender<Result<WindowMoveReport>>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowMoveRequest")
            .finish_non_exhaustive()
    }
}

impl Request {
    pub fn new(
        own_windows: impl FnOnce(DisplayGeometry) -> Result<usize> + Send + 'static,
    ) -> (Self, WindowMovePreparation) {
        let (reply, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        (
            Self {
                budget: Budget {
                    // Expiry includes queueing; abandoned UI actions cannot run
                    // much later against a subsequent remote connection.
                    deadline: Instant::now() + Duration::from_secs(7),
                    cancelled: Arc::clone(&cancelled),
                },
                own_windows: Box::new(own_windows),
                reply,
            },
            WindowMovePreparation {
                receiver,
                cancelled,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abandoning_an_action_cancels_it_before_it_can_move_windows() {
        let (request, preparation) = Request::new(|_| panic!("must not run"));
        assert!(request.budget.check().is_ok());
        drop(preparation);
        assert!(request.budget.check().is_err());
    }

    #[test]
    fn an_expired_request_is_rejected() {
        let (mut request, _preparation) = Request::new(|_| Ok(0));
        request.budget.deadline = Instant::now() - Duration::from_millis(1);
        assert!(request.budget.check().is_err());
    }
}
