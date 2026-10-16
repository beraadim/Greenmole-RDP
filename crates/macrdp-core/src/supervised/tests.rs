//! Faults run through the real worker pipes and process retirement code. The
//! fixture never opens an RDP socket, captures a display, or sends native input.

#![cfg(unix)]

use std::process::Command as ProcessCommand;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::process::Worker;
use super::protocol::{Command, Event, Initialization, Snapshot};
use crate::ServerConfig;

const FIXTURE: &str = r#"
import json, os, signal, sys, time

def send(value):
    sys.stdout.write(json.dumps(value) + '\n')
    sys.stdout.flush()

mode = sys.argv[1]
snapshot = json.loads(sys.argv[2])
initialization = json.loads(sys.stdin.readline())
assert initialization['config']['username'] == 'fixture-user'
assert initialization['config']['password'] == 'fixture-password'
assert initialization['baseline_display_ids'] == [424242]
send({'event': 'Ready', 'port': 43210, 'certificate': {
    'fingerprint_sha256': 'fixture-fingerprint', 'expires_at': 'fixture-expiry',
    'not_before_unix': 0, 'not_after_unix': 1, 'server_names': ['localhost']
}})

if mode == 'crash':
    os._exit(73)
if mode == 'malformed':
    sys.stdout.write('{"event":"fixture-private-value"}\n')
    sys.stdout.flush()
    time.sleep(30)
if mode == 'oversized':
    sys.stdout.write('x' * (128 * 1024 + 1) + '\n')
    sys.stdout.flush()
    time.sleep(30)
if mode == 'grandchild':
    descendant = os.fork()
    if descendant == 0:
        sys.stdin.close()
        sys.stdout.close()
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        while True:
            time.sleep(1)
    send({'event': 'Diagnostic', 'record': json.dumps({'descendant_pid': descendant})})
if mode in ('hang', 'grandchild'):
    snapshot['watchdog']['owner_pid'] = os.getpid()
    send({'event': 'Snapshot', 'snapshot': snapshot})
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    while True:
        time.sleep(1)

for line in sys.stdin:
    command = json.loads(line)
    if command['command'] == 'Stop':
        send({'event': 'Stopped'})
        sys.exit(0)
    if command['command'] == 'Update':
        send({'event': 'Updated', 'request': command['request'], 'result': {'Ok': None}})
        send({'event': 'Diagnostic', 'record': json.dumps({'update': command['update']})})

# The command writer must close stdin when its last sender disappears.
send({'event': 'Stopped'})
"#;

fn fixture(mode: &str) -> Worker {
    let mut config = ServerConfig::default();
    config.username = Some("fixture-user".into());
    config.password = Some("fixture-password".into());
    let initialization = Initialization {
        config,
        perf_enabled: false,
        managed_tls_directory: None,
        baseline_display_ids: vec![424242],
    };
    let mut snapshot = Snapshot::default();
    snapshot.status.running = true;
    snapshot.status.state = "running".into();
    snapshot.watchdog.operation = Some("set_frame_rate".into());
    snapshot.watchdog.operation_id = 17;
    snapshot.watchdog.pending = true;
    snapshot.watchdog.elapsed_ms = 30_000;
    snapshot.watchdog.stop_requested = true;
    snapshot.watchdog.cleanup_pending = true;
    let mut command = ProcessCommand::new("python3");
    command.args(["-u", "-c", FIXTURE, mode]);
    command.arg(serde_json::to_string(&snapshot).unwrap());
    Worker::spawn_test_worker(initialization, command).unwrap()
}

fn event(worker: &Worker) -> Event {
    worker
        .events
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap()
}

fn ready(worker: &Worker) {
    match event(worker) {
        Event::Ready { port, certificate } => {
            assert_eq!(port, 43210);
            assert_eq!(certificate.fingerprint_sha256, "fixture-fingerprint");
        }
        _ => panic!("fixture initialization did not reach Ready"),
    }
}

#[test]
fn initialization_commands_and_clean_stop_cross_real_anonymous_pipes() {
    let worker = fixture("responsive");
    ready(&worker);
    worker
        .commands
        .send(Command::Update {
            request: 9,
            update: crate::ConfigUpdate::FrameRate(45),
        })
        .unwrap();
    assert!(matches!(
        event(&worker),
        Event::Updated {
            request: 9,
            result: Ok(())
        }
    ));
    match event(&worker) {
        Event::Diagnostic { record } => {
            let json: serde_json::Value = serde_json::from_str(&record).unwrap();
            assert_eq!(json["update"]["FrameRate"], 45);
        }
        _ => panic!("fixture did not receive the configuration command"),
    }
    worker.commands.send(Command::Stop).unwrap();
    assert!(matches!(event(&worker), Event::Stopped));
    worker.retire(Duration::ZERO).unwrap();
}

#[test]
fn child_death_surfaces_as_pipe_failure_without_native_capture() {
    let worker = fixture("crash");
    ready(&worker);
    assert!(worker
        .events
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .is_err());
    worker.retire(Duration::ZERO).unwrap();
}

#[test]
fn hung_native_phase_crosses_protocol_then_process_retirement_bounds_recovery() {
    let worker = fixture("hang");
    ready(&worker);
    let pid = worker.child.id();
    match event(&worker) {
        Event::Snapshot { snapshot } => {
            assert_eq!(snapshot.watchdog.owner_pid, pid);
            assert_eq!(
                snapshot.watchdog.operation.as_deref(),
                Some("set_frame_rate")
            );
            assert!(snapshot.watchdog.is_stalled());
            assert!(snapshot.watchdog.stop_requested && snapshot.watchdog.cleanup_pending);
        }
        _ => panic!("fixture did not publish its blocked native phase"),
    }
    let before = Instant::now();
    worker.retire(Duration::from_millis(50)).unwrap();
    assert!(before.elapsed() < Duration::from_secs(3));
    assert_eq!(
        unsafe { libc::kill(pid as i32, 0) },
        -1,
        "native worker survived retirement"
    );
}

#[test]
fn command_pipe_eof_reaches_the_worker_without_a_stop_message() {
    let mut worker = fixture("eof");
    ready(&worker);
    let (replacement, discarded) = mpsc::sync_channel(1);
    drop(discarded);
    drop(std::mem::replace(&mut worker.commands, replacement));
    assert!(matches!(event(&worker), Event::Stopped));
    worker.retire(Duration::ZERO).unwrap();
}

#[test]
fn malformed_or_oversized_child_output_fails_without_echoing_payloads() {
    for mode in ["malformed", "oversized"] {
        let worker = fixture(mode);
        ready(&worker);
        let error = worker
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .err()
            .expect("bad fixture message was accepted");
        assert!(!error.to_string().contains("fixture-private-value"));
        assert!(error.to_string().len() < 256);
        worker.retire(Duration::from_millis(20)).unwrap();
    }
}

#[test]
fn hung_worker_descendant_is_in_the_owned_group_and_cannot_survive_retirement() {
    let worker = fixture("grandchild");
    ready(&worker);
    let parent = worker.child.id();
    let descendant = match event(&worker) {
        Event::Diagnostic { record } => serde_json::from_str::<serde_json::Value>(&record).unwrap()
            ["descendant_pid"]
            .as_u64()
            .unwrap() as u32,
        _ => panic!("fixture did not identify its descendant"),
    };
    assert_ne!(unsafe { libc::getpgrp() }, parent as i32);
    assert_eq!(unsafe { libc::getpgid(descendant as i32) }, parent as i32);
    worker.retire(Duration::from_millis(50)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while unsafe { libc::kill(descendant as i32, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "owned helper descendant survived group retirement"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
