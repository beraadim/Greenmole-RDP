//! Reserve native cleanup capacity before creating a codec. A blocked destructor
//! cannot accumulate unbounded threads, sessions or retained input buffers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

type Cleanup = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
pub(crate) struct RetirementPool {
    active: Arc<AtomicUsize>,
    limit: usize,
}

impl RetirementPool {
    pub fn new(limit: usize) -> Self {
        Self {
            active: Arc::default(),
            limit,
        }
    }

    pub fn reserve(&self) -> anyhow::Result<RetirementWorker> {
        // fetch_update keeps compatibility with the workspace's older Rust toolchains.
        #[allow(deprecated)]
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .map_err(|_| anyhow::anyhow!("Previous hardware encoders are still retiring"))?;
        let lease = Arc::new(Lease(self.active.clone()));
        let worker_lease = lease.clone();
        let (sender, receiver) = mpsc::channel::<Cleanup>();
        std::thread::Builder::new()
            .name("encoder-retirement".into())
            .spawn(move || {
                let _lease = worker_lease;
                if let Ok(cleanup) = receiver.recv() {
                    cleanup();
                }
            })?;
        Ok(RetirementWorker { sender, lease })
    }
}

struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) struct RetirementWorker {
    sender: mpsc::Sender<Cleanup>,
    lease: Arc<Lease>,
}
impl RetirementWorker {
    pub fn retire(self, cleanup: impl FnOnce() + Send + 'static) {
        // The dedicated receiver has no work before this send, so it cannot
        // panic or exit while the sender lives. Preserve the payload rather
        // than run native teardown on this thread if that invariant is broken.
        let lease = self.lease;
        let work = Box::new(move || {
            let _lease = lease;
            cleanup();
        });
        if let Err(error) = self.sender.send(work) {
            tracing::error!(
                "Encoder cleanup worker unexpectedly unavailable; retaining native resources"
            );
            std::mem::forget(error.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn blocked_retirement_keeps_resources_and_capacity_until_cleanup_finishes() {
        let pool = RetirementPool::new(1);
        let worker = pool.reserve().unwrap();
        let (entered, ready) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let resource = Arc::new(());
        let retained = resource.clone();
        worker.retire(move || {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            drop(retained);
            finished.send(()).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(Arc::strong_count(&resource), 2);
        assert!(pool.reserve().is_err());
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while pool.active.load(Ordering::Acquire) != 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(Arc::strong_count(&resource), 1);
        assert!(pool.reserve().is_ok());
    }

    #[test]
    fn unused_reservation_releases_capacity_after_failed_initialization() {
        let pool = RetirementPool::new(1);
        drop(pool.reserve().unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while pool.active.load(Ordering::Acquire) != 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(pool.reserve().is_ok());
    }
}
