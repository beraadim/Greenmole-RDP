//! Process ownership and bounded shutdown waits, without native dependencies.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
use std::time::Duration;

#[derive(Default)]
struct Gate(AtomicBool);

pub(super) struct ServerOwner(Arc<Gate>);

fn process_gate() -> &'static Arc<Gate> {
    static GATE: OnceLock<Arc<Gate>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(Gate::default()))
}

pub(super) fn pending() -> bool {
    process_gate().0.load(Ordering::Acquire)
}

impl Gate {
    fn acquire(self: &Arc<Self>) -> anyhow::Result<Arc<ServerOwner>> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                anyhow::anyhow!(
                    "Previous server is still running or cleaning up; wait before restarting"
                )
            })?;
        Ok(Arc::new(ServerOwner(self.clone())))
    }
}

impl ServerOwner {
    pub(super) fn acquire() -> anyhow::Result<Arc<Self>> {
        process_gate().acquire()
    }
}

impl Drop for ServerOwner {
    fn drop(&mut self) {
        self.0 .0.store(false, Ordering::Release);
    }
}

/// A timeout retains ownership. The caller can repeat this wait, and the worker
/// will finish naturally if the native call eventually returns.
pub(super) async fn wait_for_cleanup(timeout: Duration, complete: impl Fn() -> bool) -> bool {
    tokio::time::timeout(timeout, async {
        while !complete() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stalled_worker_stop_is_bounded_and_late_completion_enables_restart() {
        let gate = Arc::new(Gate::default());
        let owner = gate.acquire().unwrap();
        let retained = owner.clone();
        let (release, blocked) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _owner = retained;
            blocked.recv().unwrap();
        });
        drop(owner);
        assert!(!wait_for_cleanup(Duration::from_millis(5), || thread.is_finished()).await);
        // Timeouts/repeated Stop cannot release a live owner's startup gate.
        for _ in 0..20 {
            assert!(gate.acquire().is_err());
        }
        assert!(!wait_for_cleanup(Duration::from_millis(5), || thread.is_finished()).await);
        release.send(()).unwrap();
        assert!(wait_for_cleanup(Duration::from_secs(2), || thread.is_finished()).await);
        thread.join().unwrap();
        assert!(gate.acquire().is_ok());
    }

    #[tokio::test]
    async fn finished_server_is_pending_until_deferred_native_owners_retire() {
        let pending = Arc::new(AtomicBool::new(true));
        assert!(
            !wait_for_cleanup(Duration::from_millis(5), || !pending
                .load(Ordering::Acquire))
            .await
        );
        pending.store(false, Ordering::Release);
        assert!(
            wait_for_cleanup(Duration::from_secs(1), || !pending.load(Ordering::Acquire)).await
        );
    }

    #[test]
    fn abandoned_startup_remains_owned_by_its_initialization_worker() {
        let gate = Arc::new(Gate::default());
        let request = gate.acquire().unwrap();
        let worker = request.clone();
        drop(request);
        assert!(gate.acquire().is_err());
        drop(worker);
        assert!(gate.acquire().is_ok());
    }
}
