use super::*;
use tracing_subscriber::prelude::*;

#[test]
fn transition_records_correlate_nested_work_and_exclude_steady_configuration() {
    // Tracing caches callsite interest process-wide. Other tests exercise
    // these same phases without a subscriber or change EnvFilter levels;
    // isolate this output assertion from those concurrent subscribers.
    const CHILD: &str = "MACRDP_TEST_TRANSITION_RECORDS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_watchdog::tests::transition_records_correlate_nested_work_and_exclude_steady_configuration",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    struct Records(Arc<Mutex<Vec<serde_json::Value>>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Records {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() == "macrdp_transition" {
                let bytes = crate::log_bridge::serialize_event(event).unwrap();
                self.0
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes).unwrap());
            }
        }
    }
    let records = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(Records(records.clone()));
    tracing::subscriber::with_default(subscriber, || {
        let tracker = Tracker::default();
        let parent = tracker.phase("stop");
        let stream_drop = tracker.diagnostic_phase("stream_drop");
        let state = tracker.snapshot_at(parent.started_at + STALL_TIMEOUT);
        assert_eq!(state.operation.as_deref(), Some("stop"));
        assert!(
            state.is_stalled(),
            "Extra timing detail must not reset the stop deadline"
        );
        stream_drop.finish("ok");
        parent.finish("recovered");
        drop(tracker.phase("set_frame_rate"));
        // New watchdog-only phases must not silently expand diagnostic rate.
        drop(tracker.phase("unrecognized_steady_operation"));
        assert!(!tracker.snapshot().pending);
    });
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 4, "{records:#?}");
    let parent = &records[0]["fields"];
    let child = &records[1]["fields"];
    assert_eq!(parent["state"], "begin");
    assert_eq!(child["parent_operation_id"], parent["operation_id"]);
    assert_eq!(child["owner_id"], parent["owner_id"]);
    assert_ne!(child["operation_id"], parent["operation_id"]);
    assert_eq!(records[2]["fields"]["outcome"], "ok");
    assert_eq!(records[3]["fields"]["outcome"], "recovered");
    assert!(records[3]["fields"]["elapsed_ms"].is_u64());
    assert_eq!(records[3]["fields"]["operation_id"], parent["operation_id"]);
}

#[test]
fn a_quiet_desktop_has_no_native_deadline() {
    let tracker = Tracker::default();
    let state = snapshot_states(
        [tracker.0.clone()],
        Instant::now() + Duration::from_secs(600),
    );
    assert!(!state.pending);
    assert!(!state.is_stalled());
}

#[test]
fn nested_phase_returns_to_parent_then_clears_after_completion() {
    let tracker = Tracker::default();
    let parent = tracker.phase("replace");
    let stop = tracker.phase("replace_stop");
    assert_eq!(
        tracker.snapshot().operation.as_deref(),
        Some("replace_stop")
    );
    drop(stop);
    let start = tracker.phase("replace_start");
    assert_eq!(
        tracker.snapshot().operation.as_deref(),
        Some("replace_start")
    );
    drop(start);
    assert_eq!(tracker.snapshot().operation.as_deref(), Some("replace"));
    drop(parent);
    assert!(!tracker.snapshot().pending);
    tracker.request_stop();
    tracker.set_cleanup_pending(true);
    tracker.cleanup_finished();
    let _reconnected = tracker.phase("start");
    let reconnected = tracker.snapshot();
    assert!(!reconnected.stop_requested && !reconnected.cleanup_pending);
}

#[test]
fn deadlines_use_monotonic_age_and_cannot_be_hidden_by_a_new_owner() {
    let old = Tracker::default();
    let operation = old.phase("set_frame_rate");
    let later = Tracker::default();
    let other_thread = later.clone();
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _later = other_thread.phase("start");
        entered.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    old.request_stop();
    old.set_cleanup_pending(true);
    let state = snapshot_states(
        [old.0.clone(), later.0.clone()],
        operation.started_at + STALL_TIMEOUT,
    );
    assert!(state.is_stalled());
    assert_eq!(state.operation.as_deref(), Some("set_frame_rate"));
    assert!(state.stop_requested && state.cleanup_pending);
    assert!(state.reason().unwrap().contains("30000 ms"));
    let json = serde_json::to_string(&state).unwrap();
    let decoded: Snapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.operation_id, state.operation_id);
    assert!(decoded.is_stalled());
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[test]
fn nested_display_restoration_is_more_specific_than_owner_release() {
    let capture = Tracker::default();
    let _owner_release = capture.phase("release_display_owner");
    let display = Tracker::default();
    let _restoration = display.phase("restore_display");
    let snapshot = snapshot_states([capture.0.clone(), display.0.clone()], Instant::now());
    assert_eq!(snapshot.operation.as_deref(), Some("restore_display"));
}
