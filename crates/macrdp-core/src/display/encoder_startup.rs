//! A bounded native initializer that never joins or destroys an abandoned
//! result on the server thread. Late results remain on their owning worker.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

// Cold VideoToolbox service activation can succeed well after five seconds.
// These are asynchronous admission bounds, never a wait on the RDP thread.
pub(super) const SLOW_START_NOTICE: Duration = Duration::from_secs(5);
pub(super) const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Default)]
struct Capacity(Arc<AtomicUsize>);

struct Lease(Capacity);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn capacity() -> &'static Capacity {
    static CAPACITY: OnceLock<Capacity> = OnceLock::new();
    CAPACITY.get_or_init(Capacity::default)
}

pub(crate) fn pending() -> bool {
    capacity().0.load(Ordering::Acquire) != 0
}

/// Tests exercising the real admission slot must not compete with each other.
/// Tests of independent capacity instances need no serialization.
#[cfg(test)]
pub(super) fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static TEST_GUARD: Mutex<()> = Mutex::new(());
    TEST_GUARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Completed<T> {
    at: Instant,
    result: Result<T>,
}

pub(super) enum Poll<T> {
    Pending,
    SlowStart,
    Ready(Result<T>),
    TimedOut,
}

pub(super) struct Initializer<T> {
    result: Arc<Mutex<Option<Completed<T>>>>,
    release: Option<mpsc::Sender<()>>,
    slow_deadline: Instant,
    slow_reported: bool,
    deadline: Instant,
}

impl<T: Send + 'static> Initializer<T> {
    pub fn start(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<Self> {
        Self::start_in(capacity().clone(), STARTUP_TIMEOUT, work)
    }

    fn start_in(
        capacity: Capacity,
        timeout: Duration,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<Self> {
        capacity
            .0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("Previous encoder initialization is still pending"))?;
        let lease = Lease(capacity);
        let result = Arc::new(Mutex::new(None));
        let completed = result.clone();
        let (release, released) = mpsc::channel();
        let started = Instant::now();
        let deadline = started + timeout;
        std::thread::Builder::new()
            .name("encoder-startup".into())
            .spawn(move || {
                let _lease = lease;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Encoder initialization panicked")));
                *completed.lock().unwrap() = Some(Completed {
                    at: Instant::now(),
                    result,
                });
                // Closing the sender means either adoption or cancellation. Only
                // successful adoption takes the value; otherwise drop it here.
                let _ = released.recv();
                let abandoned = completed.lock().unwrap().take();
                drop(abandoned);
            })
            .context("Failed to start encoder initialization worker")?;
        Ok(Self {
            result,
            release: Some(release),
            slow_deadline: started + SLOW_START_NOTICE.min(timeout),
            slow_reported: false,
            deadline,
        })
    }

    pub fn poll(&mut self, now: Instant) -> Poll<T> {
        let mut result = self.result.lock().unwrap();
        if result
            .as_ref()
            .is_some_and(|completed| completed.at <= self.deadline)
        {
            let result = result.take().expect("completed result checked").result;
            self.release.take();
            Poll::Ready(result)
        } else if now >= self.deadline {
            Poll::TimedOut
        } else if !self.slow_reported && now >= self.slow_deadline {
            self.slow_reported = true;
            Poll::SlowStart
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests;
