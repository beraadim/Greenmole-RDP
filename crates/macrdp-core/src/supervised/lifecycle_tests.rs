//! Startup cancellation must stop an already-spawned owner even when no public
//! handle was returned or its ordinary control queue can no longer accept work.

use super::{ParentCommand, Shared, StartupCancellation};
use crate::supervised::protocol::Snapshot;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

fn fixture(capacity: usize) -> (Arc<Shared>, tokio::sync::mpsc::Receiver<ParentCommand>) {
    let (commands, receiver) = tokio::sync::mpsc::channel(capacity);
    let shared = Arc::new(Shared {
        snapshot: Mutex::new(Snapshot::default()),
        terminal_error: Mutex::new(None),
        finished: AtomicBool::new(false),
        stopping: AtomicBool::new(false),
        worker_pid: AtomicU32::new(0),
        restart_count: AtomicU32::new(0),
        recovery_result: Mutex::new("none"),
        commands,
    });
    (shared, receiver)
}

#[test]
fn abandoning_startup_persists_stop_intent_and_wakes_the_supervisor() {
    let (shared, mut commands) = fixture(1);
    drop(StartupCancellation {
        shared: shared.clone(),
        armed: true,
    });
    assert!(shared.stopping.load(Ordering::Acquire));
    assert!(matches!(commands.try_recv(), Ok(ParentCommand::Stop)));
    // Cancellation requests cleanup; it must not falsely declare native
    // ownership already retired before the supervisor verifies it.
    assert!(!shared.finished.load(Ordering::Acquire));
}

#[test]
fn accepting_the_started_handle_disarms_startup_cancellation() {
    let (shared, mut commands) = fixture(1);
    drop(StartupCancellation {
        shared: shared.clone(),
        armed: false,
    });
    assert!(!shared.stopping.load(Ordering::Acquire));
    assert!(matches!(
        commands.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn saturated_control_queue_cannot_block_or_discard_startup_stop_intent() {
    let (shared, mut commands) = fixture(1);
    shared
        .commands
        .try_send(ParentCommand::Stop)
        .unwrap_or_else(|_| panic!("empty control queue rejected a request"));
    let (finished, waiter) = std::sync::mpsc::sync_channel(1);
    let cancellation = StartupCancellation {
        shared: shared.clone(),
        armed: true,
    };
    let worker = std::thread::spawn(move || {
        drop(cancellation);
        let _ = finished.send(());
    });
    // No command consumer drains capacity until cancellation has returned.
    // A blocking send in Drop would therefore make this regression fail.
    waiter
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("startup cancellation blocked on a full control queue");
    worker.join().unwrap();
    assert!(shared.stopping.load(Ordering::Acquire));
    assert!(matches!(commands.try_recv(), Ok(ParentCommand::Stop)));
    assert!(!shared.finished.load(Ordering::Acquire));
}

#[test]
fn disconnected_control_queue_still_records_startup_stop_intent() {
    let (shared, commands) = fixture(1);
    drop(commands);
    drop(StartupCancellation {
        shared: shared.clone(),
        armed: true,
    });
    assert!(shared.stopping.load(Ordering::Acquire));
    assert!(!shared.finished.load(Ordering::Acquire));
}
