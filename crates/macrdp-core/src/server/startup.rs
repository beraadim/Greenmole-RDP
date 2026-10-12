//! Startup acknowledgement and cancellation, independent of native desktop APIs.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{oneshot, Notify};

pub(super) type StartupReceiver = oneshot::Receiver<std::result::Result<(), String>>;

pub(super) struct StartupSignal(Option<oneshot::Sender<std::result::Result<(), String>>>);

impl StartupSignal {
    pub(super) fn channel() -> (Self, StartupReceiver) {
        let (sender, receiver) = oneshot::channel();
        (Self(Some(sender)), receiver)
    }

    pub(super) fn ready(&mut self) -> Result<()> {
        self.0
            .take()
            .context("Startup was already acknowledged")?
            .send(Ok(()))
            .map_err(|_| anyhow::anyhow!("Startup requester was cancelled"))
    }

    pub(super) fn failed(&mut self, error: &anyhow::Error) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(Err(format!("{error:#}")));
        }
    }
}

/// A cancelled/expired startup must never leave a thread accepting connections later.
pub(super) struct StartupCancellation(pub(super) Option<Arc<Notify>>);

impl Drop for StartupCancellation {
    fn drop(&mut self) {
        if let Some(shutdown) = self.0.take() {
            shutdown.notify_one();
        }
    }
}

pub(super) async fn wait_for_startup(receiver: StartupReceiver, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, receiver)
        .await
        .context("RDP server startup timed out")?
        .context("RDP server thread exited before becoming ready")?
        .map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_waits_for_explicit_ready_and_propagates_initialization_errors() {
        let (mut signal, receiver) = StartupSignal::channel();
        let mut waiting = Box::pin(wait_for_startup(receiver, Duration::from_secs(1)));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        signal.ready().unwrap();
        waiting.await.unwrap();
        let (mut signal, receiver) = StartupSignal::channel();
        signal.failed(&anyhow::anyhow!("test TLS failure"));
        assert!(wait_for_startup(receiver, Duration::from_secs(1))
            .await
            .unwrap_err()
            .to_string()
            .contains("test TLS failure"));
    }

    #[tokio::test]
    async fn abandoned_startup_preserves_shutdown_notification_and_rejects_late_ready() {
        let shutdown = Arc::new(Notify::new());
        let guard = StartupCancellation(Some(shutdown.clone()));
        let (mut signal, receiver) = StartupSignal::channel();
        assert!(wait_for_startup(receiver, Duration::from_millis(1))
            .await
            .is_err());
        drop(guard);
        assert!(signal.ready().is_err());
        tokio::time::timeout(Duration::from_millis(50), shutdown.notified())
            .await
            .unwrap();
    }
}
