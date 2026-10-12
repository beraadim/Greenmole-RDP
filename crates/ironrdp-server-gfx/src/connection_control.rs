//! Deadlines and server control remain live before a client is activated.

use std::future::Future;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use tokio::sync::watch;
use tokio::time::{sleep_until, Instant};

use super::{RdpServer, ServerEvent};

pub(super) const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

impl RdpServer {
    /// Limit each negotiation, TLS, CredSSP and activation stage. A stage's
    /// deadline is fixed, so a peer cannot extend it by slowly sending bytes.
    pub fn set_handshake_timeout(&mut self, timeout: Duration) {
        self.handshake_timeout = timeout.max(Duration::from_millis(1));
    }

    /// Disconnect after this much keyboard/mouse inactivity. Graphics ACKs,
    /// audio and clipboard traffic do not keep an unattended session alive.
    /// `None` or zero disables the timeout. Resizing preserves the deadline.
    pub fn set_idle_timeout(&mut self, timeout: Option<Duration>) {
        self.idle_timeout = timeout.filter(|duration| !duration.is_zero());
    }

    pub(super) async fn await_connection_stage<T>(
        &mut self,
        stage: &'static str,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let receiver = self.ev_receiver.clone();
        let mut receiver = receiver.lock().await;
        let deadline = Instant::now()
            .checked_add(self.handshake_timeout)
            .context("Handshake timeout exceeds the clock's supported range")?;
        let sender = self.ev_sender.clone();
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                reason = sender.failure() => bail!("{stage}: {reason}"),
                _ = sleep_until(deadline) => bail!("{stage} timed out"),
                event = receiver.controls.recv() => match event {
                    Some(ServerEvent::Quit(_)) => {
                        self.quit_requested = true;
                        bail!("server stopped during {stage}");
                    }
                    Some(ServerEvent::GetLocalAddr(reply)) => {
                        let _ = reply.send(self.local_addr);
                    }
                    Some(ServerEvent::SetCredentials(credentials)) => {
                        self.set_credentials(Some(credentials));
                        // The acceptor holds a credential snapshot. Cancel it
                        // rather than allowing revoked credentials to log in.
                        bail!("credentials changed during {stage}");
                    }
                    Some(_) => unreachable!("only server controls use this lane"),
                    None => bail!("server control channel closed during {stage}"),
                },
                result = &mut future => return result.with_context(|| format!("{stage} failed")),
            }
        }
    }
}

pub(super) async fn wait_for_idle_timeout(
    mut activity: watch::Receiver<Instant>,
    timeout: Option<Duration>,
) {
    let Some(timeout) = timeout else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        let last_input = *activity.borrow_and_update();
        let Some(deadline) = last_input.checked_add(timeout) else {
            // An idle duration beyond the monotonic clock's range cannot
            // expire in this process, but must remain cancellation safe.
            if activity.changed().await.is_err() {
                return;
            }
            continue;
        };
        tokio::select! {
            biased;
            changed = activity.changed() => {
                if changed.is_err() { return; }
            }
            _ = sleep_until(deadline) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn input_extends_idle_deadline_and_silence_expires() {
        let (activity, receiver) = watch::channel(Instant::now());
        let waiter = wait_for_idle_timeout(receiver, Some(Duration::from_millis(80)));
        tokio::pin!(waiter);
        assert!(tokio::time::timeout(Duration::from_millis(40), &mut waiter)
            .await
            .is_err());
        activity.send_replace(Instant::now());
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut waiter)
            .await
            .is_err());
        tokio::time::timeout(Duration::from_millis(80), &mut waiter)
            .await
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn oversized_idle_duration_is_safe_and_cancellable() {
        let (_activity, receiver) = watch::channel(Instant::now());
        assert!(tokio::time::timeout(
            Duration::from_millis(1),
            wait_for_idle_timeout(receiver, Some(Duration::MAX))
        )
        .await
        .is_err());
    }
}
