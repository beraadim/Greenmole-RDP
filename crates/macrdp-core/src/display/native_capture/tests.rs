use super::*;
use std::sync::mpsc;

struct Gate {
    entered: oneshot::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl Gate {
    fn wait(self) {
        let _ = self.entered.send(());
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

fn gate() -> (Gate, oneshot::Receiver<()>, mpsc::Sender<()>) {
    let (entered, wait) = oneshot::channel();
    let (release, receive) = mpsc::channel();
    (
        Gate {
            entered,
            release: receive,
        },
        wait,
        release,
    )
}

#[derive(Default)]
struct Observed {
    events: Mutex<Vec<String>>,
    configure_gate: Mutex<Option<Gate>>,
    replacement_gate: Mutex<Option<Gate>>,
    stop_gate: Mutex<Option<Gate>>,
    fail_stop_once: std::sync::atomic::AtomicBool,
    panic_configure: std::sync::atomic::AtomicBool,
    fail_configure_remaining: AtomicU32,
}

impl Observed {
    fn record(&self, event: impl Into<String>) {
        self.events.lock().unwrap().push(event.into());
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

struct Stream {
    observed: Arc<Observed>,
    fps: u32,
    generation: u32,
}

impl NativeStream for Stream {
    type Receiver = u32;
    type Configuration = u32;

    fn receiver(&self) -> u32 {
        self.generation
    }

    fn frame_rate(&self) -> u32 {
        self.fps
    }

    fn set_frame_rate(&mut self, fps: u32) -> Result<()> {
        self.observed.record(format!("configure-{fps}"));
        assert!(
            !self.observed.panic_configure.load(Ordering::Acquire),
            "simulated configuration panic"
        );
        if let Some(gate) = self.observed.configure_gate.lock().unwrap().take() {
            gate.wait();
        }
        if self
            .observed
            .fail_configure_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            anyhow::bail!("simulated native configuration failure");
        }
        self.fps = fps;
        Ok(())
    }

    fn replace(&mut self, generation: u32) -> Result<u32> {
        self.observed.record(format!("replace-{generation}"));
        if let Some(gate) = self.observed.replacement_gate.lock().unwrap().take() {
            gate.wait();
        }
        self.generation = generation;
        Ok(generation)
    }

    fn stop(&mut self) -> Result<()> {
        if self.observed.fail_stop_once.swap(false, Ordering::AcqRel) {
            self.observed.record("stop-failed");
            anyhow::bail!("simulated native stop failure");
        }
        self.observed.record("stop");
        if let Some(gate) = self.observed.stop_gate.lock().unwrap().take() {
            gate.wait();
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.observed.record("stream-dropped");
    }
}

struct Owner {
    observed: Arc<Observed>,
    retired: Option<oneshot::Sender<()>>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.observed.record("owner-released");
        let _ = self.retired.take().unwrap().send(());
    }
}

fn owned_stream() -> (Owner, Stream, Arc<Observed>, oneshot::Receiver<()>) {
    let observed = Arc::new(Observed::default());
    let (retired, done) = oneshot::channel();
    (
        Owner {
            observed: observed.clone(),
            retired: Some(retired),
        },
        Stream {
            observed: observed.clone(),
            fps: 30,
            generation: 1,
        },
        observed,
        done,
    )
}

async fn complete(receiver: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn cancelled_initial_start_retires_late_stream_without_blocking_runtime() {
    let (owner, stream, observed, retired) = owned_stream();
    let (start_gate, entered, release) = gate();
    let start = CaptureWorker::start(owner, move || {
        start_gate.wait();
        Ok((stream, ()))
    });
    assert!(tokio::time::timeout(Duration::from_millis(30), start)
        .await
        .is_err());
    complete(entered).await;
    assert!(observed.events().is_empty());
    // This send executes on the same current-thread runtime that cancelled
    // startup. A native Drop there would deadlock until Gate's guard timeout.
    release.send(()).unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        ["stop", "stream-dropped", "owner-released"]
    );
}

#[tokio::test]
async fn completed_start_failure_releases_owner_and_returns_the_failure() {
    let observed = Arc::new(Observed::default());
    let (retired, done) = oneshot::channel();
    let owner = Owner {
        observed: observed.clone(),
        retired: Some(retired),
    };
    let result = CaptureWorker::<Stream>::start(owner, || -> Result<(Stream, ())> {
        anyhow::bail!("start completion reported failure")
    })
    .await;
    match result {
        Ok(_) => panic!("failed startup must not produce a handle"),
        Err(error) => assert_eq!(error.to_string(), "start completion reported failure"),
    }
    complete(done).await;
    assert_eq!(observed.events(), ["owner-released"]);
}

#[tokio::test]
async fn pending_configuration_coalesces_rates_and_acknowledges_only_native_success() {
    let (owner, stream, observed, retired) = owned_stream();
    let (configure, entered, release) = gate();
    *observed.configure_gate.lock().unwrap() = Some(configure);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    capture.set_frame_rate(10);
    complete(entered).await;
    for fps in 11..=240 {
        capture.set_frame_rate(fps);
    }
    assert_eq!(capture.applied_frame_rate(), 30);
    assert_eq!(
        capture.stop_on_drop.0.pending.lock().unwrap().fps,
        Some(240)
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while capture.applied_frame_rate() != 240 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    capture.stop().await.unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        [
            "configure-10",
            "configure-240",
            "stop",
            "stream-dropped",
            "owner-released"
        ]
    );
}

#[tokio::test]
async fn stop_cancels_queued_work_and_waits_for_current_configuration_before_releasing_owner() {
    let (owner, stream, observed, retired) = owned_stream();
    let (configure, entered, release) = gate();
    *observed.configure_gate.lock().unwrap() = Some(configure);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    capture.set_frame_rate(10);
    complete(entered).await;
    for fps in 11..=240 {
        capture.set_frame_rate(fps);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(10), capture.replace(2))
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), capture.stop())
            .await
            .is_err()
    );
    assert_eq!(observed.events(), ["configure-10"]);
    release.send(()).unwrap();
    capture.stop().await.unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        ["configure-10", "stop", "stream-dropped", "owner-released"]
    );
}

#[tokio::test]
async fn cancelled_format_wait_resumes_one_replacement_and_adopts_its_actual_configuration() {
    let (owner, stream, observed, retired) = owned_stream();
    let (replacement, entered, release) = gate();
    *observed.replacement_gate.lock().unwrap() = Some(replacement);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), capture.replace(2))
            .await
            .is_err()
    );
    complete(entered).await;
    assert!(capture.has_pending_replacement());
    assert_eq!(capture.receiver, 1);
    release.send(()).unwrap();
    // A newer request must first adopt the in-flight operation. Its old frame
    // mailbox cannot be mistaken for the replacement's format or dimensions.
    assert_eq!(capture.replace(3).await.unwrap(), 2);
    assert_eq!(capture.receiver, 2);
    assert_eq!(capture.replace(3).await.unwrap(), 3);
    capture.stop().await.unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        [
            "replace-2",
            "replace-3",
            "stop",
            "stream-dropped",
            "owner-released"
        ]
    );
}

#[tokio::test]
async fn dropped_capture_retains_owner_until_pending_native_stop_finishes() {
    let (owner, stream, observed, retired) = owned_stream();
    let (stop, entered, release) = gate();
    *observed.stop_gate.lock().unwrap() = Some(stop);
    let (capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    drop(capture);
    complete(entered).await;
    assert_eq!(observed.events(), ["stop"]);
    release.send(()).unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        ["stop", "stream-dropped", "owner-released"]
    );
}

#[tokio::test]
async fn failed_stop_reports_error_but_retains_owner_until_a_confirmed_retry() {
    let (owner, stream, observed, retired) = owned_stream();
    observed.fail_stop_once.store(true, Ordering::Release);
    let (stop, entered, release) = gate();
    *observed.stop_gate.lock().unwrap() = Some(stop);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    assert!(capture
        .stop()
        .await
        .unwrap_err()
        .is::<NativeRetirementFailed>());
    assert!(capture
        .stop()
        .await
        .unwrap_err()
        .is::<NativeRetirementFailed>());
    drop(capture);
    complete(entered).await;
    assert_eq!(observed.events(), ["stop-failed", "stop"]);
    release.send(()).unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        ["stop-failed", "stop", "stream-dropped", "owner-released"]
    );
}

#[tokio::test]
async fn configuration_panic_still_retires_native_stream_before_releasing_owner() {
    let (owner, stream, observed, retired) = owned_stream();
    observed.panic_configure.store(true, Ordering::Release);
    let (stop, entered, release) = gate();
    *observed.stop_gate.lock().unwrap() = Some(stop);
    let (capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    capture.set_frame_rate(10);
    complete(entered).await;
    assert_eq!(observed.events(), ["configure-10", "stop"]);
    release.send(()).unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        ["configure-10", "stop", "stream-dropped", "owner-released"]
    );
}

async fn wait_for_status(
    capture: &CaptureWorker<Stream>,
    predicate: impl Fn(FrameRateStatus) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate(capture.frame_rate_status()) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_frame_rate_update_retries_and_reports_only_confirmed_application() {
    let (owner, stream, observed, retired) = owned_stream();
    observed
        .fail_configure_remaining
        .store(1, Ordering::Release);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    capture.set_frame_rate(15);
    wait_for_status(&capture, |status| status.failures == 1).await;
    let status = capture.frame_rate_status();
    assert_eq!(status.requested, 15);
    assert_eq!(status.applied, 30);
    assert!(!status.retry_exhausted);
    wait_for_status(&capture, |status| status.applied == 15).await;
    assert_eq!(capture.frame_rate_status().failures, 1);
    assert!(!capture.frame_rate_status().retry_exhausted);
    capture.stop().await.unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        [
            "configure-15",
            "configure-15",
            "stop",
            "stream-dropped",
            "owner-released"
        ]
    );
}

#[tokio::test]
async fn exhausted_frame_rate_retries_stay_bounded_and_a_new_request_can_recover() {
    let (owner, stream, observed, retired) = owned_stream();
    observed
        .fail_configure_remaining
        .store(100, Ordering::Release);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    capture.set_frame_rate(15);
    wait_for_status(&capture, |status| status.retry_exhausted).await;
    let status = capture.frame_rate_status();
    assert_eq!(status.failures, 4);
    assert_eq!(status.requested, 15);
    assert_eq!(status.applied, 30);
    // Longer than the final backoff: an exhausted target must remain idle.
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert_eq!(observed.events(), ["configure-15"; 4]);
    observed
        .fail_configure_remaining
        .store(0, Ordering::Release);
    capture.set_frame_rate(20);
    wait_for_status(&capture, |status| status.applied == 20).await;
    let status = capture.frame_rate_status();
    assert_eq!(status.requested, 20);
    assert_eq!(status.failures, 4);
    assert!(!status.retry_exhausted);
    capture.stop().await.unwrap();
    complete(retired).await;
}

#[tokio::test]
async fn newer_frame_rate_target_supersedes_a_failed_in_flight_request() {
    let (owner, stream, observed, retired) = owned_stream();
    let (configure, entered, release) = gate();
    *observed.configure_gate.lock().unwrap() = Some(configure);
    observed
        .fail_configure_remaining
        .store(1, Ordering::Release);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    capture.set_frame_rate(15);
    complete(entered).await;
    capture.set_frame_rate(20);
    release.send(()).unwrap();
    wait_for_status(&capture, |status| status.applied == 20).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(observed.events(), ["configure-15", "configure-20"]);
    assert_eq!(capture.frame_rate_status().requested, 20);
    assert!(!capture.frame_rate_status().retry_exhausted);
    capture.stop().await.unwrap();
    complete(retired).await;
}

#[tokio::test]
async fn stop_interrupts_frame_rate_retry_backoff() {
    let (owner, stream, observed, retired) = owned_stream();
    observed
        .fail_configure_remaining
        .store(100, Ordering::Release);
    let (mut capture, ()) = CaptureWorker::start(owner, move || Ok((stream, ())))
        .await
        .unwrap();
    capture.set_frame_rate(15);
    wait_for_status(&capture, |status| status.failures == 3).await;
    // The next retry is 400 ms away. Retirement should wake the condition
    // variable immediately instead of sleeping through that backoff.
    tokio::time::timeout(Duration::from_millis(200), capture.stop())
        .await
        .unwrap()
        .unwrap();
    complete(retired).await;
    assert_eq!(
        observed.events(),
        [
            "configure-15",
            "configure-15",
            "configure-15",
            "stop",
            "stream-dropped",
            "owner-released"
        ]
    );
}
