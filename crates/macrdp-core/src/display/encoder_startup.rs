//! A bounded native initializer that never joins or destroys an abandoned
//! result on the server thread. Late results remain on their owning worker.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

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

struct Completed<T> {
    at: Instant,
    result: Result<T>,
}

pub(super) enum Poll<T> {
    Pending,
    Ready(Result<T>),
    TimedOut,
}

pub(super) struct Initializer<T> {
    result: Arc<Mutex<Option<Completed<T>>>>,
    release: Option<mpsc::Sender<()>>,
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
        let deadline = Instant::now() + timeout;
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
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests;
