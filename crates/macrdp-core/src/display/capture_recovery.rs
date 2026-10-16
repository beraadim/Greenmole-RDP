//! Single-flight native recovery with bounded retries and independently paced fallback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use macrdp_audio::SharedAudioTx;
use macrdp_capture::{AudioFrameSender, CaptureConfig};
use tokio::sync::oneshot;

#[derive(Debug)]
pub(super) struct NativeRetirementFailed;

impl std::fmt::Display for NativeRetirementFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Native capture stop could not be confirmed")
    }
}
impl std::error::Error for NativeRetirementFailed {}

pub(super) struct CaptureRecovery<T> {
    probe: Option<NativeProbe<T>>,
    cancelled: Arc<AtomicBool>,
    next_probe_at: Instant,
    next_fallback_at: Instant,
    failures: u32,
    started_at: Instant,
    warned_slow_probe: bool,
}

struct NativeProbe<T> {
    ready: oneshot::Receiver<()>,
    result: Arc<Mutex<Option<Result<Option<T>>>>>,
    release: Option<std::sync::mpsc::Sender<()>>,
    retired: oneshot::Receiver<Result<()>>,
}

impl<T> NativeProbe<T> {
    async fn take_result(&mut self) -> Result<Option<T>> {
        (&mut self.ready)
            .await
            .context("Native recovery worker ended unexpectedly")
            .context(NativeRetirementFailed)?;
        // No await between taking ownership and returning it to the display
        // loop. Cancellation can never strand a capture on an async task.
        let result = self
            .result
            .lock()
            .unwrap()
            .take()
            .expect("ready native probe has a result");
        self.release.take();
        result
    }

    async fn cancel(mut self) -> Result<()> {
        self.release.take();
        (&mut self.retired)
            .await
            .context("Native recovery cleanup ended unexpectedly")
            .context(NativeRetirementFailed)?
    }
}

impl<T: Send + 'static> CaptureRecovery<T> {
    pub fn new(now: Instant) -> Self {
        Self {
            probe: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            next_probe_at: now,
            next_fallback_at: now,
            failures: 0,
            started_at: now,
            warned_slow_probe: false,
        }
    }

    pub fn probe_due(&self, now: Instant) -> bool {
        self.probe.is_none() && now >= self.next_probe_at
    }

    pub fn probe_completed(&self) -> bool {
        self.probe
            .as_ref()
            .is_some_and(|probe| probe.result.lock().unwrap().is_some())
    }

    pub fn start_probe(
        &mut self,
        work: impl FnOnce(Arc<AtomicBool>) -> Result<Option<T>> + Send + 'static,
        cleanup: impl FnOnce(T) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        assert!(
            self.probe.is_none(),
            "capture recovery probes must never overlap"
        );
        let cancelled = self.cancelled.clone();
        self.started_at = Instant::now();
        self.warned_slow_probe = false;
        let result = Arc::new(Mutex::new(None));
        let shared = result.clone();
        let (ready_tx, ready) = oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (retired_tx, retired) = oneshot::channel();
        // A dedicated native thread is independent of the server's Tokio
        // blocking pool: a stalled Apple callback cannot hold runtime shutdown.
        // The process-wide capture lease bounds this to one native owner.
        std::thread::Builder::new()
            .name("capture-recovery".into())
            .spawn(move || {
                let completed = work(cancelled);
                *shared.lock().unwrap() = Some(completed);
                let _ = ready_tx.send(());
                let _ = released.recv();
                // The worker retains the result until it is adopted or cancelled.
                // Dropping an unpolled recovery therefore performs synchronous
                // native cleanup here, never on the server event-loop thread.
                let unclaimed = shared.lock().unwrap().take();
                let result = match unclaimed {
                    Some(Ok(Some(capture))) => cleanup(capture),
                    Some(Err(error)) if error.is::<NativeRetirementFailed>() => Err(error),
                    _ => Ok(()),
                };
                if let Err(error) = &result {
                    tracing::warn!(%error, "Native capture retirement failed");
                }
                let _ = retired_tx.send(result);
            })
            .context("Failed to start native capture recovery worker")?;
        self.probe = Some(NativeProbe {
            ready,
            result,
            release: Some(release),
            retired,
        });
        Ok(())
    }

    pub async fn wait_probe(&mut self) -> Result<Option<T>> {
        match &mut self.probe {
            Some(probe) => probe.take_result().await,
            None => std::future::pending().await,
        }
    }

    pub fn failed(&mut self, now: Instant) {
        self.probe = None;
        self.failures = self.failures.saturating_add(1);
        let delay = 250u64.saturating_mul(1u64 << self.failures.saturating_sub(1).min(3));
        self.next_probe_at = now + Duration::from_millis(delay);
    }

    pub fn warn_if_slow(&mut self, now: Instant) {
        if self.probe.is_some()
            && !self.warned_slow_probe
            && now.saturating_duration_since(self.started_at) >= Duration::from_secs(3)
        {
            self.warned_slow_probe = true;
            let native = crate::native_watchdog::snapshot();
            tracing::warn!(
                operation = native.operation.as_deref().unwrap_or("recovery_probe"),
                operation_id = native.operation_id,
                operation_elapsed_ms = native.elapsed_ms,
                recovery_elapsed_ms = now.saturating_duration_since(self.started_at).as_millis() as u64,
                stop_requested = native.stop_requested,
                cleanup_pending = native.cleanup_pending,
                "Native capture recovery is still pending; retaining one probe while fallback and controls remain active"
            );
        }
    }

    pub fn fallback_due(&self, now: Instant) -> bool {
        now >= self.next_fallback_at
    }

    pub fn fallback_attempted(&mut self, now: Instant, configured_interval: Duration) {
        self.next_fallback_at = now + configured_interval.max(Duration::from_millis(33));
    }

    pub fn next_wake(&self, now: Instant) -> Duration {
        let deadline = if self.probe.is_some() {
            self.next_fallback_at
        } else {
            self.next_probe_at.min(self.next_fallback_at)
        };
        deadline.saturating_duration_since(now)
    }

    /// Resize must await old native capture cleanup before changing display mode.
    pub async fn cancel(mut self) -> Result<()> {
        self.cancelled.store(true, Ordering::Release);
        if let Some(probe) = self.probe.take() {
            probe.cancel().await?;
        }
        Ok(())
    }
}

impl<T> Drop for CaptureRecovery<T> {
    fn drop(&mut self) {
        // A blocking native API cannot be interrupted. Its worker checks this
        // before creating/adopting a stream and retires any late result itself.
        // NativeProbe's sender drop releases that worker even after completion.
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Snapshot the active session sender when scheduling each attempt. A delayed
/// native worker must never pick up a later connection's replacement sender.
pub(super) fn create_with_current_audio<T>(
    config: CaptureConfig,
    shared_audio: &Option<SharedAudioTx>,
    factory: impl FnOnce(CaptureConfig, Option<AudioFrameSender>) -> Result<T>,
) -> Result<T> {
    let audio = shared_audio
        .as_ref()
        .and_then(|shared| shared.lock().unwrap().clone());
    factory(config, audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use macrdp_capture::{audio_channel, AudioFrame, CapturePixelFormat};

    fn config() -> CaptureConfig {
        CaptureConfig {
            display_id: 1,
            width: 16,
            height: 16,
            frame_rate: 30,
            pixel_format: CapturePixelFormat::Bgra,
            show_cursor: false,
        }
    }

    #[tokio::test]
    async fn suspend_recovery_keeps_the_current_audio_channel_without_native_capture() {
        let (old, mut old_receiver) = audio_channel(Arc::default());
        let shared = Some(Arc::new(std::sync::Mutex::new(Some(old))));
        let (current, mut receiver) = audio_channel(Arc::default());
        *shared.as_ref().unwrap().lock().unwrap() = Some(current);
        let (config, audio) =
            create_with_current_audio(config(), &shared, |config, audio| Ok((config, audio)))
                .unwrap();
        let (later, mut later_receiver) = audio_channel(Arc::default());
        *shared.as_ref().unwrap().lock().unwrap() = Some(later);
        let mut recovery = CaptureRecovery::new(Instant::now());
        recovery
            .start_probe(
                move |_| {
                    assert_eq!(config.display_id, 1);
                    audio
                        .expect("recovery must preserve enabled audio")
                        .send(AudioFrame {
                            data: vec![0.0; 1920],
                            sample_rate: 48000,
                            channels: 2,
                            num_samples: 960,
                            timestamp_ms: 1234,
                            captured_at: Instant::now(),
                        });
                    Ok(Some(()))
                },
                |_| Ok(()),
            )
            .unwrap();
        drop(shared);
        assert!(recovery.wait_probe().await.unwrap().is_some());
        assert_eq!(receiver.recv().await.unwrap().timestamp_ms, 1234);
        assert!(old_receiver.recv().await.is_none());
        assert!(later_receiver.recv().await.is_none());
    }

    #[test]
    fn retries_back_off_to_two_seconds_and_fallback_remains_independent() {
        let now = Instant::now();
        let mut recovery = CaptureRecovery::<()>::new(now);
        for delay in [250, 500, 1000, 2000, 2000] {
            recovery.failed(now);
            assert!(!recovery.probe_due(now + Duration::from_millis(delay - 1)));
            assert!(recovery.probe_due(now + Duration::from_millis(delay)));
            assert!(recovery.fallback_due(now));
        }
        recovery.fallback_attempted(now, Duration::from_millis(5));
        assert_eq!(recovery.next_wake(now), Duration::from_millis(33));
    }

    #[tokio::test]
    async fn pending_native_probe_yields_to_cursor_deadlines_without_starting_another() {
        let (release, hold) = std::sync::mpsc::channel();
        let mut recovery = CaptureRecovery::<()>::new(Instant::now());
        recovery
            .start_probe(
                move |_| {
                    hold.recv().unwrap();
                    Ok(Some(()))
                },
                |_| Ok(()),
            )
            .unwrap();
        assert!(!recovery.probe_due(Instant::now() + Duration::from_secs(100)));
        assert!(matches!(
            super::super::event_wait::next_event(recovery.wait_probe(), None, Some(Duration::ZERO))
                .await,
            super::super::event_wait::DisplayWake::Cursor
        ));
        release.send(()).unwrap();
        assert!(recovery.wait_probe().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn resize_cancellation_tolerates_failed_start_but_not_unconfirmed_stop() {
        let mut ordinary = CaptureRecovery::<()>::new(Instant::now());
        ordinary
            .start_probe(|_| anyhow::bail!("stream start unavailable"), |_| Ok(()))
            .unwrap();
        ordinary.cancel().await.unwrap();
        let mut fatal = CaptureRecovery::<()>::new(Instant::now());
        fatal
            .start_probe(
                |_| Err(anyhow::anyhow!("stop callback failed").context(NativeRetirementFailed)),
                |_| Ok(()),
            )
            .unwrap();
        assert!(fatal
            .cancel()
            .await
            .unwrap_err()
            .is::<NativeRetirementFailed>());
    }
}
