//! One native owner for start, configuration, replacement, and final retirement.
//!
//! Apple APIs may wait indefinitely. The async handle only reads a bounded
//! frame mailbox and signals this dedicated thread. The process-wide lease
//! stays on that thread until stream cleanup and display restoration complete.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use macrdp_capture::{
    AudioFrameSender, CaptureConfig, CaptureDeliveryStats, CaptureEvent, CaptureReceiver,
    CapturedFrame, ScreenCapturer,
};
use tokio::sync::oneshot;

use crate::native_watchdog::Tracker;

use super::capture_recovery::NativeRetirementFailed;

mod frame_rate;
use frame_rate::FrameRateUpdate;

#[derive(Clone, Copy, Debug)]
pub(super) struct FrameRateStatus {
    pub requested: u32,
    pub applied: u32,
    pub failures: u64,
    pub retry_exhausted: bool,
}

pub(super) trait NativeStream: Send + 'static {
    type Receiver: Send + 'static;
    type Configuration: Clone + Send + 'static;

    fn receiver(&self) -> Self::Receiver;
    fn frame_rate(&self) -> u32;
    fn set_frame_rate(&mut self, fps: u32) -> Result<()>;
    fn replace(
        &mut self,
        configuration: Self::Configuration,
        tracker: &Tracker,
    ) -> Result<Self::Receiver>;
    fn stop(&mut self) -> Result<()>;
}

impl NativeStream for ScreenCapturer {
    type Receiver = CaptureReceiver;
    type Configuration = (CaptureConfig, Option<AudioFrameSender>);

    fn receiver(&self) -> Self::Receiver {
        self.receiver()
    }

    fn frame_rate(&self) -> u32 {
        self.frame_rate()
    }

    fn set_frame_rate(&mut self, fps: u32) -> Result<()> {
        self.set_frame_rate(fps)
    }

    fn replace(
        &mut self,
        (config, audio): Self::Configuration,
        tracker: &Tracker,
    ) -> Result<Self::Receiver> {
        // Retire the old stream before starting its replacement. All callbacks
        // and the native lease remain owned here if either operation stalls.
        {
            let _operation = tracker.phase("replace_stop");
            self.stop().context(NativeRetirementFailed)?;
        }
        {
            let _operation = tracker.phase("replace_start");
            *self = Self::new_blocking(config, audio)?;
        }
        Ok(self.receiver())
    }

    fn stop(&mut self) -> Result<()> {
        self.stop()
    }
}

type ReplacementResult<S> = Result<(
    <S as NativeStream>::Receiver,
    <S as NativeStream>::Configuration,
)>;

struct Replacement<S: NativeStream> {
    configuration: S::Configuration,
    ready: oneshot::Sender<ReplacementResult<S>>,
}

struct Requested<S: NativeStream> {
    stop: bool,
    fps: Option<u32>,
    replacement: Option<Replacement<S>>,
}

impl<S: NativeStream> Default for Requested<S> {
    fn default() -> Self {
        Self {
            stop: false,
            fps: None,
            replacement: None,
        }
    }
}

struct Commands<S: NativeStream> {
    watchdog: Tracker,
    pending: Mutex<Requested<S>>,
    changed: Condvar,
    applied_fps: AtomicU32,
    requested_fps: AtomicU32,
    fps_failures: AtomicU64,
    fps_retry_exhausted: AtomicBool,
}

impl<S: NativeStream> Commands<S> {
    fn request_stop(&self) {
        self.watchdog.request_stop();
        self.pending.lock().unwrap().stop = true;
        self.changed.notify_one();
    }

    fn control_stream(&self, stream: &mut S) {
        let mut retry: Option<FrameRateUpdate> = None;
        loop {
            let mut pending = self.pending.lock().unwrap();
            while !pending.stop && pending.replacement.is_none() && pending.fps.is_none() {
                pending = if let Some(update) = &retry {
                    let delay = update.remaining_delay();
                    if delay.is_zero() {
                        break;
                    }
                    // Unlike sleeping, this releases the mailbox and wakes for
                    // shutdown, replacement, or a newer adaptive FPS target.
                    self.changed.wait_timeout(pending, delay).unwrap().0
                } else {
                    self.changed.wait(pending).unwrap()
                };
            }
            if pending.stop {
                return;
            }
            if let Some(replacement) = pending.replacement.take() {
                drop(pending);
                let configuration = replacement.configuration;
                let _operation = self.watchdog.phase("replace");
                let result = stream
                    .replace(configuration.clone(), &self.watchdog)
                    .map(|receiver| (receiver, configuration));
                self.applied_fps
                    .store(stream.frame_rate(), Ordering::Release);
                if result.is_ok() {
                    // A format replacement may have started with an older
                    // configuration. Preserve the latest target on the new stream.
                    let _pending = self.pending.lock().unwrap();
                    self.fps_retry_exhausted.store(false, Ordering::Release);
                    retry = Some(FrameRateUpdate::new(
                        self.requested_fps.load(Ordering::Acquire),
                    ));
                }
                let _ = replacement.ready.send(result);
                continue;
            }
            if let Some(fps) = pending.fps.take() {
                retry = Some(FrameRateUpdate::new(fps));
            }
            drop(pending);
            let Some(mut update) = retry.take() else {
                continue;
            };
            if update.fps == stream.frame_rate() {
                continue;
            }
            let _operation = self.watchdog.phase("set_frame_rate");
            match stream.set_frame_rate(update.fps) {
                Ok(()) => {
                    self.applied_fps
                        .store(stream.frame_rate(), Ordering::Release);
                    if update.attempt() > 1 {
                        tracing::info!(
                            fps = update.fps,
                            attempt = update.attempt(),
                            "Native capture frame rate update recovered"
                        );
                    }
                }
                Err(error) => {
                    self.fps_failures.fetch_add(1, Ordering::Relaxed);
                    let attempt = update.attempt();
                    let retrying = update.failed();
                    // A failed old request must not label a newer target as
                    // exhausted. The producer clears this under the same lock.
                    let pending = self.pending.lock().unwrap();
                    if pending.fps.is_none() {
                        self.fps_retry_exhausted.store(!retrying, Ordering::Release);
                    }
                    drop(pending);
                    tracing::warn!(
                        %error,
                        fps = update.fps,
                        attempt,
                        retrying,
                        "Native capture frame rate update failed"
                    );
                    if retrying {
                        retry = Some(update);
                    }
                }
            }
        }
    }
}

/// Signals cleanup even when initial activation is cancelled before a handle
/// exists. This guard never waits for the native owner.
struct StopOnDrop<S: NativeStream>(Arc<Commands<S>>);

impl<S: NativeStream> Drop for StopOnDrop<S> {
    fn drop(&mut self) {
        self.0.request_stop();
    }
}

pub(super) type NativeCapture = CaptureWorker<ScreenCapturer>;

pub(super) struct CaptureWorker<S: NativeStream> {
    receiver: S::Receiver,
    stop_on_drop: StopOnDrop<S>,
    retired: oneshot::Receiver<Result<()>>,
    stopped: bool,
    retirement_failed: bool,
    replacement: Option<oneshot::Receiver<ReplacementResult<S>>>,
}

impl<S: NativeStream> CaptureWorker<S> {
    pub async fn start<M: Send + 'static>(
        owner: impl Send + 'static,
        start: impl FnOnce() -> Result<(S, M)> + Send + 'static,
    ) -> Result<(Self, M)> {
        Self::start_tracked(owner, start, Tracker::default()).await
    }

    async fn start_tracked<M: Send + 'static>(
        owner: impl Send + 'static,
        start: impl FnOnce() -> Result<(S, M)> + Send + 'static,
        watchdog: Tracker,
    ) -> Result<(Self, M)> {
        let commands: Arc<Commands<S>> = Arc::new(Commands {
            watchdog,
            pending: Mutex::new(Requested::default()),
            changed: Condvar::new(),
            applied_fps: AtomicU32::new(0),
            requested_fps: AtomicU32::new(0),
            fps_failures: AtomicU64::new(0),
            fps_retry_exhausted: AtomicBool::new(false),
        });
        let stop_on_drop = StopOnDrop(commands.clone());
        let (ready_tx, ready) = oneshot::channel();
        let (retired_tx, retired) = oneshot::channel();
        std::thread::Builder::new()
            .name("native-capture".into())
            .spawn(move || {
                // Declaration order also protects unwinding: native stream
                // locals retire before their owner's lease is released.
                let owner = owner;
                let started = {
                    let _operation = commands.watchdog.phase("start");
                    start()
                };
                let mut stream = match started {
                    Ok((stream, metadata)) => {
                        commands
                            .applied_fps
                            .store(stream.frame_rate(), Ordering::Release);
                        commands
                            .requested_fps
                            .store(stream.frame_rate(), Ordering::Release);
                        if ready_tx.send(Ok((stream.receiver(), metadata))).is_err() {
                            commands.request_stop();
                        }
                        stream
                    }
                    Err(error) => {
                        let _operation = commands.watchdog.phase("start_failure_cleanup");
                        drop(owner);
                        let _ = ready_tx.send(Err(error));
                        let _ = retired_tx.send(Ok(()));
                        return;
                    }
                };

                let controls = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    commands.control_stream(&mut stream);
                }));
                if controls.is_err() {
                    tracing::error!("Native capture control panicked; retiring stream before releasing ownership");
                }

                let mut acknowledgement = Some(retired_tx);
                let mut delay = Duration::from_millis(250);
                // One phase includes repeated failed stop attempts: an Apple
                // callback that never confirms retirement is also a stalled
                // owner, even when each failed attempt returns promptly.
                let retirement = commands.watchdog.phase("stop");
                loop {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| stream.stop()))
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Native capture stop panicked")));
                    match result {
                        Ok(()) => break,
                        Err(error) => {
                            // Report the failure, but never release exclusive
                            // ownership while a live stream may still exist.
                            // One retiring worker retries; reconnects cannot
                            // create replacement streams or cleanup threads.
                            if let Some(acknowledgement) = acknowledgement.take() {
                                tracing::warn!(%error, "Native capture stop failed; retaining ownership until retirement succeeds");
                                let _ = acknowledgement.send(Err(error.context(NativeRetirementFailed)));
                            }
                            std::thread::sleep(delay);
                            delay = (delay * 2).min(Duration::from_secs(2));
                        }
                    }
                }
                drop(stream);
                drop(retirement);
                let _operation = commands.watchdog.phase("release_display_owner");
                drop(owner);
                // A successful acknowledgement proves that old capture and its
                // lease are gone before resize can mutate display mode.
                if let Some(acknowledgement) = acknowledgement {
                    let _ = acknowledgement.send(Ok(()));
                }
            })
            .context("Failed to start native capture owner")?;
        let (receiver, metadata) = ready
            .await
            .context("Native capture owner ended during start")??;
        Ok((
            Self {
                receiver,
                stop_on_drop,
                retired,
                stopped: false,
                retirement_failed: false,
                replacement: None,
            },
            metadata,
        ))
    }

    pub fn applied_frame_rate(&self) -> u32 {
        self.stop_on_drop.0.applied_fps.load(Ordering::Acquire)
    }

    pub fn frame_rate_status(&self) -> FrameRateStatus {
        let commands = &self.stop_on_drop.0;
        // Keep requested target and its exhausted flag in one generation.
        // Native calls never hold this lock.
        let _pending = commands.pending.lock().unwrap();
        FrameRateStatus {
            requested: commands.requested_fps.load(Ordering::Acquire),
            applied: self.applied_frame_rate(),
            failures: commands.fps_failures.load(Ordering::Relaxed),
            retry_exhausted: commands.fps_retry_exhausted.load(Ordering::Acquire),
        }
    }

    pub fn set_frame_rate(&self, fps: u32) {
        let fps = fps.max(1);
        let mut pending = self.stop_on_drop.0.pending.lock().unwrap();
        if !pending.stop {
            // Only the latest desired rate survives a slow configuration call.
            pending.fps = Some(fps);
            self.stop_on_drop
                .0
                .requested_fps
                .store(fps, Ordering::Release);
            self.stop_on_drop
                .0
                .fps_retry_exhausted
                .store(false, Ordering::Release);
            self.stop_on_drop.0.changed.notify_one();
        }
    }

    pub fn has_pending_replacement(&self) -> bool {
        self.replacement.is_some()
    }

    /// The in-flight receiver lives on the handle, not this future. A cursor,
    /// resize, or server control cancelling an await cannot enqueue duplicates.
    pub async fn replace(&mut self, configuration: S::Configuration) -> Result<S::Configuration> {
        if self.replacement.is_none() {
            let (ready, result) = oneshot::channel();
            let mut pending = self.stop_on_drop.0.pending.lock().unwrap();
            anyhow::ensure!(!pending.stop, "Native capture is stopping");
            anyhow::ensure!(
                pending.replacement.is_none(),
                "Native replacement already queued"
            );
            pending.replacement = Some(Replacement {
                configuration,
                ready,
            });
            self.replacement = Some(result);
            self.stop_on_drop.0.changed.notify_one();
        }
        let result = self
            .replacement
            .as_mut()
            .expect("replacement receiver installed")
            .await
            .context("Native capture owner ended during replacement")?;
        self.replacement = None;
        let (receiver, configuration) = result?;
        self.receiver = receiver;
        Ok(configuration)
    }

    pub async fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return if self.retirement_failed {
                Err(NativeRetirementFailed.into())
            } else {
                Ok(())
            };
        }
        self.stop_on_drop.0.request_stop();
        let result = (&mut self.retired)
            .await
            .context("Native capture owner ended during retirement")
            .context(NativeRetirementFailed)
            .and_then(|result| result);
        self.stopped = true;
        self.retirement_failed = result.is_err();
        result
    }
}

#[cfg(test)]
mod tests;

impl NativeCapture {
    pub async fn next_frame(&mut self) -> Option<CaptureEvent> {
        self.receiver.next_frame().await
    }

    pub fn try_latest_image(&mut self) -> Option<CapturedFrame> {
        self.receiver.try_latest_image()
    }

    pub fn last_delivery_stats(&self) -> CaptureDeliveryStats {
        self.receiver.last_delivery_stats()
    }
}
