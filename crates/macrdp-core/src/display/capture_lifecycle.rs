//! Process-wide native capture ownership, including late recovery and restoration.

use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use macrdp_display::DisplayController;

type Restore = Box<dyn FnOnce() -> Result<()> + Send>;

struct Owner {
    restore: Option<Restore>,
    restore_requested: bool,
    finishing: bool,
}

#[derive(Default)]
struct Lifecycle {
    owner: Mutex<Option<Owner>>,
}

#[derive(Debug)]
struct NativeCaptureBusy;

impl std::fmt::Display for NativeCaptureBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Previous native capture is still retiring; retry the connection shortly")
    }
}
impl std::error::Error for NativeCaptureBusy {}

fn lifecycle() -> &'static Arc<Lifecycle> {
    static LIFECYCLE: OnceLock<Arc<Lifecycle>> = OnceLock::new();
    LIFECYCLE.get_or_init(|| Arc::new(Lifecycle::default()))
}

/// The native owner drops its stream before this lease. Async handles only
/// signal that owner, so disconnect cannot release ownership while a pending
/// start, configuration, late result, or native retirement survives.
pub(super) struct NativeCaptureLease(Arc<Lifecycle>);

impl Lifecycle {
    fn acquire(self: &Arc<Self>, restore: Restore) -> Result<NativeCaptureLease> {
        let mut owner = self.owner.lock().unwrap();
        anyhow::ensure!(owner.is_none(), NativeCaptureBusy);
        *owner = Some(Owner {
            restore: Some(restore),
            restore_requested: false,
            finishing: false,
        });
        Ok(NativeCaptureLease(self.clone()))
    }

    fn request_restore(owner: &mut Option<Owner>) -> bool {
        if let Some(owner) = owner {
            // Once final restoration starts, further requests are covered by
            // that same barrier. No waiter threads or cleanup queue accumulate.
            if !owner.finishing {
                owner.restore_requested = true;
            }
            true
        } else {
            false
        }
    }
}

pub(super) fn acquire(display: &DisplayController) -> Result<NativeCaptureLease> {
    let display = display.clone();
    lifecycle().acquire(Box::new(move || display.end_remote_session()))
}

impl Drop for NativeCaptureLease {
    fn drop(&mut self) {
        let (restore, requested) = {
            let mut owner = self.0.owner.lock().unwrap();
            let current = owner.as_mut().expect("native capture lease owns lifecycle");
            let requested = current.restore_requested;
            let restore = current.restore.take();
            if requested {
                current.finishing = true;
            } else {
                // Native capture has already retired. Release atomically with
                // the final pending-request check: a later disconnect must
                // enqueue its restoration, not mistake this for a barrier.
                *owner = None;
            }
            (restore, requested)
        };
        if let Some(restore) = restore {
            if requested {
                if let Err(error) = restore() {
                    tracing::warn!(%error, "Deferred physical display restoration failed");
                }
            } else {
                // The active session still retains its controller. Shutdown
                // registers restoration before releasing those handles.
                drop(restore);
            }
        }
        if requested {
            *self.0.owner.lock().unwrap() = None;
        }
    }
}

/// Preserve normal disconnect queue ordering, but defer restoration when a
/// native stream or a late recovery result still owns the display.
pub(crate) fn queue_restoration(display: &DisplayController) -> Result<()> {
    let mut owner = lifecycle().owner.lock().unwrap();
    if Lifecycle::request_restore(&mut owner) {
        return Ok(());
    }
    display.end_remote_session_async()
}

/// Called from the server's existing blocking shutdown cleanup. Recovery owns
/// its own native thread, so pending retirement never blocks server shutdown.
pub(crate) fn restore_after_capture(display: DisplayController) -> Result<()> {
    let lease = {
        let mut owner = lifecycle().owner.lock().unwrap();
        if Lifecycle::request_restore(&mut owner) {
            return Ok(());
        }
        let retained = display.clone();
        *owner = Some(Owner {
            restore: Some(Box::new(move || retained.end_remote_session())),
            restore_requested: false,
            finishing: false,
        });
        NativeCaptureLease(lifecycle().clone())
    };
    let result = display.end_remote_session();
    drop(display);
    drop(lease);
    result
}

#[cfg(test)]
mod tests {
    use super::super::capture_recovery::CaptureRecovery;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct NativeStub {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        release: Option<std::sync::mpsc::Receiver<()>>,
        retired: Arc<AtomicUsize>,
    }

    impl Drop for NativeStub {
        fn drop(&mut self) {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            if let Some(release) = self.release.take() {
                release.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            self.retired.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Match RecoveredCapture's ownership: stop/drop first, lease second.
    struct NativeResult {
        _capture: NativeStub,
        _lease: NativeCaptureLease,
    }

    #[test]
    fn ownership_and_restoration_remain_bounded_across_reconnects() {
        let lifecycle = Arc::new(Lifecycle::default());
        let restored = Arc::new(AtomicUsize::new(0));
        let count = restored.clone();
        let lease = lifecycle
            .acquire(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))
            .unwrap();
        for _ in 0..100 {
            assert!(lifecycle.acquire(Box::new(|| Ok(()))).is_err());
            assert!(Lifecycle::request_restore(
                &mut lifecycle.owner.lock().unwrap()
            ));
        }
        assert_eq!(restored.load(Ordering::SeqCst), 0);
        drop(lease);
        assert_eq!(restored.load(Ordering::SeqCst), 1);
        assert!(lifecycle.acquire(Box::new(|| Ok(()))).is_ok());
    }

    #[tokio::test]
    async fn unpolled_result_retires_on_worker_before_restoration_and_reactivation() {
        let lifecycle = Arc::new(Lifecycle::default());
        let retired = Arc::new(AtomicUsize::new(0));
        let count = retired.clone();
        let (restored_tx, restored) = tokio::sync::oneshot::channel();
        let lease = lifecycle
            .acquire(Box::new(move || {
                assert_eq!(count.load(Ordering::SeqCst), 1);
                let _ = restored_tx.send(());
                Ok(())
            }))
            .unwrap();
        let (started_tx, started) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let mut recovery = CaptureRecovery::new(Instant::now());
        recovery
            .start_probe(
                move |_| {
                    Ok(Some(NativeResult {
                        _capture: NativeStub {
                            started: Some(started_tx),
                            release: Some(wait),
                            retired,
                        },
                        _lease: lease,
                    }))
                },
                |capture| {
                    drop(capture);
                    Ok(())
                },
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !recovery.probe_completed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(Lifecycle::request_restore(
            &mut lifecycle.owner.lock().unwrap()
        ));
        drop(recovery);
        // If Drop ran native cleanup on this current-thread runtime, its
        // blocking receiver would time out before this release could execute.
        tokio::time::timeout(Duration::from_secs(5), started)
            .await
            .unwrap()
            .unwrap();
        assert!(lifecycle.acquire(Box::new(|| Ok(()))).is_err());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), restored)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if lifecycle.acquire(Box::new(|| Ok(()))).is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn pending_probe_does_not_hold_server_runtime_shutdown_or_release_native_ownership() {
        let lifecycle = Arc::new(Lifecycle::default());
        let retired = Arc::new(AtomicUsize::new(0));
        let count = retired.clone();
        let (restored_tx, restored) = std::sync::mpsc::channel();
        let lease = lifecycle
            .acquire(Box::new(move || {
                assert_eq!(count.load(Ordering::SeqCst), 1);
                restored_tx.send(()).unwrap();
                Ok(())
            }))
            .unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let (stopped_tx, stopped) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut recovery = CaptureRecovery::new(Instant::now());
                recovery
                    .start_probe(
                        move |_| {
                            wait.recv_timeout(Duration::from_secs(5)).unwrap();
                            Ok(Some(NativeResult {
                                _capture: NativeStub {
                                    started: None,
                                    release: None,
                                    retired,
                                },
                                _lease: lease,
                            }))
                        },
                        |capture| {
                            drop(capture);
                            Ok(())
                        },
                    )
                    .unwrap();
                drop(recovery);
            });
            drop(runtime);
            stopped_tx.send(()).unwrap();
        });
        stopped.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(lifecycle.acquire(Box::new(|| Ok(()))).is_err());
        assert!(Lifecycle::request_restore(
            &mut lifecycle.owner.lock().unwrap()
        ));
        release.send(()).unwrap();
        restored.recv_timeout(Duration::from_secs(5)).unwrap();
        server.join().unwrap();
    }
}
