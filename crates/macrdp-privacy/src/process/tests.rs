use super::*;
use crate::policy::LockState;
use std::thread;

fn synthetic_failure(_pid: u32, error: anyhow::Error) -> ! {
    // Only this hook is injected; the real parent monitor and its clocks,
    // pipes and child-exit observation run unchanged. No lock API is called.
    panic!("synthetic private-session recovery: {error}")
}

struct Fixture {
    commands: mpsc::SyncSender<Control>,
    replies: mpsc::SyncSender<Result<Message>>,
    healthy: Arc<AtomicBool>,
    worker: thread::JoinHandle<()>,
    child_pid: u32,
    output: Option<std::process::ChildStdout>,
}

fn fixture() -> Fixture {
    fixture_with_initial(LockState::Unknown)
}

fn fixture_with_initial(initial: LockState) -> Fixture {
    // cat is a harmless anonymous-pipe peer. It cannot activate any native
    // privacy function and exits once the monitor drops its input pipe.
    let mut child = Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let child_pid = child.id();
    let (reply_tx, replies) = mpsc::sync_channel(8);
    let (commands, command_rx) = mpsc::sync_channel(1);
    let (ready_tx, ready) = mpsc::sync_channel(1);
    let healthy = Arc::new(AtomicBool::new(false));
    let monitored = Arc::clone(&healthy);
    reply_tx.send(Ok(Message::Ready { initial })).unwrap();
    let worker = thread::spawn(move || {
        monitor(
            ReapOnDrop(Some(child)),
            input,
            replies,
            command_rx,
            ready_tx,
            monitored,
            42,
            synthetic_failure,
        )
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    assert!(healthy.load(Ordering::Acquire));
    Fixture {
        commands,
        replies: reply_tx,
        healthy,
        worker,
        child_pid,
        output: Some(output),
    }
}

fn reap_fixture(pid: u32) {
    // The monitor may already have reaped this child with try_wait. Confirm
    // child ownership first; never signal a numerical PID after it was reaped.
    unsafe {
        let state = libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG);
        if state != 0 {
            return;
        }
        // The live child remains unreaped, so this PID cannot yet be reused.
        libc::kill(pid as i32, libc::SIGKILL);
        libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
    }
}

#[test]
fn monitor_idle_eof_and_malformed_reply_trigger_reciprocal_recovery() {
    for message in ["fixture EOF", "fixture malformed frame"] {
        let fixture = fixture();
        fixture.replies.send(Err(anyhow::anyhow!(message))).unwrap();
        assert!(fixture.worker.join().is_err());
        assert!(!fixture.healthy.load(Ordering::Acquire));
        reap_fixture(fixture.child_pid);
    }
}

#[test]
fn monitor_idle_helper_process_death_is_detected_without_remote_input() {
    let fixture = fixture();
    unsafe {
        libc::kill(fixture.child_pid as i32, libc::SIGKILL);
    }
    assert!(fixture.worker.join().is_err());
    assert!(!fixture.healthy.load(Ordering::Acquire));
    reap_fixture(fixture.child_pid);
}

#[test]
fn monitor_finite_ack_lease_expires_even_when_helper_process_remains_alive() {
    let fixture = fixture();
    let started = Instant::now();
    assert!(fixture.worker.join().is_err());
    assert!(started.elapsed() >= LEASE && started.elapsed() < LEASE + Duration::from_secs(2));
    assert!(!fixture.healthy.load(Ordering::Acquire));
    reap_fixture(fixture.child_pid);
}

#[test]
fn monitor_finish_uses_real_pipe_and_waits_for_cleanup_acknowledgement() {
    let mut fixture = fixture();
    let output = fixture.output.take().unwrap();
    let replies = fixture.replies.clone();
    let peer = thread::spawn(move || {
        let mut output = output;
        loop {
            match read_message(&mut output).unwrap().unwrap() {
                Message::Heartbeat => replies.send(Ok(Message::Alive)).unwrap(),
                Message::Finish { force_lock: true } => {
                    replies
                        .send(Ok(Message::Finished { locked: true }))
                        .unwrap();
                    return;
                }
                message => panic!("unexpected monitor command: {message:?}"),
            }
        }
    });
    let (reply, done) = mpsc::sync_channel(1);
    fixture
        .commands
        .send(Control::Finish {
            force_lock: true,
            reply,
        })
        .unwrap();
    done.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    fixture.worker.join().unwrap();
    peer.join().unwrap();
    assert!(!fixture.healthy.load(Ordering::Acquire));
}

#[test]
fn monitor_rejects_unverified_required_lock_in_finished_ack() {
    for (initial, force_lock, accepted) in [
        (LockState::Unknown, false, false),
        (LockState::Locked, false, false),
        (LockState::Unlocked, true, false),
        (LockState::Unlocked, false, true),
    ] {
        let mut fixture = fixture_with_initial(initial);
        let mut output = fixture.output.take().unwrap();
        let replies = fixture.replies.clone();
        let peer = thread::spawn(move || loop {
            match read_message(&mut output).unwrap().unwrap() {
                Message::Heartbeat => replies.send(Ok(Message::Alive)).unwrap(),
                Message::Finish { .. } => {
                    replies
                        .send(Ok(Message::Finished { locked: false }))
                        .unwrap();
                    return;
                }
                message => panic!("unexpected test command: {message:?}"),
            }
        });
        let (reply, done) = mpsc::sync_channel(1);
        fixture
            .commands
            .send(Control::Finish { force_lock, reply })
            .unwrap();
        let acknowledged = done.recv_timeout(Duration::from_secs(1));
        assert_eq!(matches!(acknowledged, Ok(Ok(()))), accepted);
        assert_eq!(fixture.worker.join().is_ok(), accepted);
        peer.join().unwrap();
        assert!(!fixture.healthy.load(Ordering::Acquire));
        reap_fixture(fixture.child_pid);
    }
}
