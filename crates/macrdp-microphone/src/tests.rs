use super::worker::{pump, DriverSession, DriverTransport};
use super::*;
use std::sync::Mutex;

#[test]
fn worker_admission_stays_quarantined_until_the_owner_exits() {
    let active = AtomicBool::new(false);
    let admission = Admission::acquire(&active).unwrap();
    assert!(Admission::acquire(&active).is_none());
    drop(admission);
    assert!(Admission::acquire(&active).is_some());
}

struct FakeDriver {
    operations: Arc<Mutex<Vec<(u32, usize)>>>,
    observed: mpsc::SyncSender<()>,
}

impl DriverTransport for FakeDriver {
    fn send(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let opcode = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        self.operations.lock().unwrap().push((opcode, bytes.len()));
        if opcode == wire::PCM {
            let _ = self.observed.try_send(());
        }
        Ok(())
    }
}

#[test]
fn stop_discards_queued_audio_and_ends_driver_session() {
    let operations = Arc::new(Mutex::new(Vec::new()));
    let (observed, received) = mpsc::sync_channel(1);
    let driver = FakeDriver {
        operations: Arc::clone(&operations),
        observed,
    };
    let state = Arc::new(WorkerState::default());
    let worker_state = Arc::clone(&state);
    let (sender, receiver) = mpsc::sync_channel(QUEUE_PACKETS);
    let thread = std::thread::spawn(move || {
        let mut session = DriverSession {
            driver,
            generation: 1,
        };
        session.send(wire::BEGIN, &[]).unwrap();
        let mut converter = convert::Converter::new(MicrophonePcmFormat {
            sample_rate: 48_000,
            channels: 2,
        })
        .unwrap();
        pump(receiver, &mut converter, &worker_state, &mut session).unwrap();
    });
    sender
        .send(Packet {
            sequence: 0,
            received_at: Instant::now(),
            pcm: vec![0; 1920],
        })
        .unwrap();
    received.recv_timeout(Duration::from_secs(2)).unwrap();
    state.cancelled.store(true, Ordering::Release);
    // Nothing queued after cancellation may become another driver's session.
    let _ = sender.try_send(Packet {
        sequence: 1,
        received_at: Instant::now(),
        pcm: vec![0; 1920],
    });
    drop(sender);
    thread.join().unwrap();
    let recorded = operations.lock().unwrap();
    assert_eq!(
        *recorded,
        vec![(wire::BEGIN, 24), (wire::PCM, 3864), (wire::END, 24)]
    );
}

#[test]
fn stale_packet_is_dropped_and_gap_resets_before_fresh_pcm() {
    let operations = Arc::new(Mutex::new(Vec::new()));
    let (observed, _) = mpsc::sync_channel(1);
    let driver = FakeDriver {
        operations: Arc::clone(&operations),
        observed,
    };
    let mut session = DriverSession {
        driver,
        generation: 3,
    };
    let (sender, receiver) = mpsc::sync_channel(QUEUE_PACKETS);
    sender
        .send(Packet {
            sequence: 0,
            received_at: Instant::now() - Duration::from_secs(1),
            pcm: vec![255; 1920],
        })
        .unwrap();
    sender
        .send(Packet {
            sequence: 1,
            received_at: Instant::now(),
            pcm: vec![0; 1920],
        })
        .unwrap();
    drop(sender);
    let mut converter = convert::Converter::new(MicrophonePcmFormat {
        sample_rate: 48_000,
        channels: 2,
    })
    .unwrap();
    pump(
        receiver,
        &mut converter,
        &WorkerState::default(),
        &mut session,
    )
    .unwrap();
    drop(session);
    assert_eq!(
        *operations.lock().unwrap(),
        vec![
            (wire::END, 24),
            (wire::BEGIN, 24),
            (wire::PCM, 3864),
            (wire::END, 24)
        ]
    );
}

#[test]
fn congestion_is_bounded_without_blocking_the_rdp_callback() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let (finished, completed) = mpsc::sync_channel(1);
    let state = Arc::new(WorkerState::default());
    let mut backend = Backend {
        worker: Some(Worker {
            sender,
            state: Arc::clone(&state),
            completed,
            packet_bytes: 1920,
            frame_bytes: 4,
            next_sequence: 0,
        }),
    };
    backend.audio_data(&vec![0; 1920]).unwrap();
    backend.audio_data(&vec![0; 1920]).unwrap();
    assert_eq!(state.dropped.load(Ordering::Relaxed), 1);
    assert_eq!(receiver.try_recv().unwrap().sequence, 0);
    assert!(receiver.try_recv().is_err());
    assert!(backend.audio_data(&[0, 1]).is_err());
    finished.send(()).unwrap();
    backend.stop();
    assert!(state.cancelled.load(Ordering::Acquire));
}

#[test]
fn capture_resumes_from_partial_packets_after_a_pause() {
    let operations = Arc::new(Mutex::new(Vec::new()));
    let (observed, received) = mpsc::sync_channel(1);
    let driver = FakeDriver {
        operations: Arc::clone(&operations),
        observed,
    };
    let (sender, receiver) = mpsc::sync_channel(QUEUE_PACKETS);
    let thread = std::thread::spawn(move || {
        let mut session = DriverSession {
            driver,
            generation: 7,
        };
        let mut converter = convert::Converter::new(MicrophonePcmFormat {
            sample_rate: 48_000,
            channels: 2,
        })
        .unwrap();
        pump(
            receiver,
            &mut converter,
            &WorkerState::default(),
            &mut session,
        )
        .unwrap();
    });
    sender
        .send(Packet {
            sequence: 0,
            received_at: Instant::now(),
            pcm: vec![0; 1920],
        })
        .unwrap();
    received.recv_timeout(Duration::from_secs(2)).unwrap();
    std::thread::sleep(MAX_PACKET_AGE + Duration::from_millis(30));
    for sequence in 1..=2 {
        sender
            .send(Packet {
                sequence,
                received_at: Instant::now(),
                pcm: vec![0; 960],
            })
            .unwrap();
    }
    received.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(sender);
    thread.join().unwrap();
    let recorded = operations.lock().unwrap();
    assert_eq!(
        recorded
            .iter()
            .filter(|(opcode, _)| *opcode == wire::PCM)
            .count(),
        2
    );
    assert_eq!(
        recorded
            .iter()
            .filter(|(opcode, _)| *opcode == wire::BEGIN)
            .count(),
        1
    );
}
