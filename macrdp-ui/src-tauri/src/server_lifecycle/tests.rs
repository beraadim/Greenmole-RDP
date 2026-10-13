use super::cancellable_start;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[tokio::test]
async fn stop_before_native_start_is_polled_remains_pending_until_observed() {
    let (stop, mut receiver) = tokio::sync::watch::channel(0_u64);
    let _ = *receiver.borrow_and_update();
    stop.send_replace(1);
    let result = cancellable_start(&mut receiver, async {
        panic!("Cancelled initializer must never be polled");
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    })
    .await;
    assert!(result.unwrap_err().to_string().contains("cancelled"));
}

#[tokio::test]
async fn stop_drops_pending_startup_and_runs_its_cancellation_guard() {
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Guard(dropped.clone());
    let (stop, mut receiver) = tokio::sync::watch::channel(0_u64);
    let (entered, started) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        cancellable_start(&mut receiver, async move {
            let _guard = guard;
            entered.send(()).unwrap();
            std::future::pending::<anyhow::Result<()>>().await
        })
        .await
    });
    started.await.unwrap();
    stop.send_replace(1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(dropped.load(Ordering::Acquire));
}
