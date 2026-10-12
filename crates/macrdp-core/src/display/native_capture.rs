//! One native owner for start, configuration, replacement, and final retirement.
//!
//! Apple APIs may wait indefinitely. The async handle only reads a bounded
//! frame mailbox and signals this dedicated thread. The process-wide lease
//! stays on that thread until stream cleanup and display restoration complete.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use macrdp_capture::{
    AudioFrameSender, CaptureConfig, CaptureDeliveryStats, CaptureEvent, CaptureReceiver,
    CapturedFrame, ScreenCapturer,
};
use tokio::sync::oneshot;

use super::capture_recovery::NativeRetirementFailed;

pub(super) trait NativeStream: Send + 'static {
    type Receiver: Send + 'static;
    type Configuration: Clone + Send + 'static;

    fn receiver(&self) -> Self::Receiver;
    fn frame_rate(&self) -> u32;
    fn set_frame_rate(&mut self, fps: u32) -> Result<()>;
    fn replace(&mut self, configuration: Self::Configuration) -> Result<Self::Receiver>;
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

    fn replace(&mut self, (config, audio): Self::Configuration) -> Result<Self::Receiver> {
        // Retire the old stream before starting its replacement. All callbacks
        // and the native lease remain owned here if either operation stalls.
        self.stop().context(NativeRetirementFailed)?;
        *self = Self::new_blocking(config, audio)?;
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
    pending: Mutex<Requested<S>>,
    changed: Condvar,
    applied_fps: AtomicU32,
}

impl<S: NativeStream> Commands<S> {
    fn request_stop(&self) {
        self.pending.lock().unwrap().stop = true;
        self.changed.notify_one();
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
        let commands: Arc<Commands<S>> = Arc::new(Commands {
            pending: Mutex::new(Requested::default()),
            changed: Condvar::new(),
            applied_fps: AtomicU32::new(0),
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
                let mut stream = match start() {
                    Ok((stream, metadata)) => {
                        commands
                            .applied_fps
                            .store(stream.frame_rate(), Ordering::Release);
                        if ready_tx.send(Ok((stream.receiver(), metadata))).is_err() {
                            commands.request_stop();
                        }
                        stream
                    }
                    Err(error) => {
                        drop(owner);
                        let _ = ready_tx.send(Err(error));
                        let _ = retired_tx.send(Ok(()));
                        return;
                    }
                };

                let controls = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                    let mut pending = commands.pending.lock().unwrap();
                    while !pending.stop && pending.replacement.is_none() && pending.fps.is_none() {
                        pending = commands.changed.wait(pending).unwrap();
                    }
                    if pending.stop {
                        break;
                    }
                    if let Some(replacement) = pending.replacement.take() {
                        drop(pending);
                        let configuration = replacement.configuration;
                        let result = stream
                            .replace(configuration.clone())
                            .map(|receiver| (receiver, configuration));
                        commands
                            .applied_fps
                            .store(stream.frame_rate(), Ordering::Release);
                        let _ = replacement.ready.send(result);
                    } else if let Some(fps) = pending.fps.take() {
                        drop(pending);
                        if fps != stream.frame_rate() {
                            match stream.set_frame_rate(fps) {
                                Ok(()) => commands.applied_fps.store(fps, Ordering::Release),
                                Err(error) => tracing::warn!(%error, fps, "Native capture frame rate update failed"),
                            }
                        }
                    }
                }));
                if controls.is_err() {
                    tracing::error!("Native capture control panicked; retiring stream before releasing ownership");
                }

                let mut acknowledgement = Some(retired_tx);
                let mut delay = Duration::from_millis(250);
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

    pub fn set_frame_rate(&self, fps: u32) {
        let mut pending = self.stop_on_drop.0.pending.lock().unwrap();
        if !pending.stop {
            // Only the latest desired rate survives a slow configuration call.
            pending.fps = Some(fps);
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
