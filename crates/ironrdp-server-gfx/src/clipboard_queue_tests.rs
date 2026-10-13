use super::*;
use ironrdp_cliprdr::pdu::{FileContentsResponse, FormatDataResponse};

fn clipboard(bytes: usize) -> ServerEvent {
    ServerEvent::Clipboard(ClipboardMessage::SendFormatData(
        FormatDataResponse::new_data(vec![0; bytes]),
    ))
}

#[tokio::test]
async fn clipboard_error_invalidates_queued_and_admitted_output_but_keeps_audio_and_controls() {
    let (sender, mut receiver) = channel();
    let (_guard, producer) = sender.start_session();
    producer.send(clipboard(8)).unwrap();
    let (_, admission) = receiver.data.recv().await.unwrap().into_parts();
    producer
        .send(ServerEvent::ClipboardFileContents(
            FileContentsResponse::new_data_response(1, vec![1, 2, 3]),
        ))
        .unwrap();
    assert!(producer
        .send(ServerEvent::Clipboard(ClipboardMessage::Error(Box::new(
            std::io::Error::other("fake blocked file producer"),
        ))))
        .is_err());
    assert!(!admission.is_deliverable_at(Instant::now()));
    assert!(
        receiver.try_recv().is_err(),
        "previously queued file data escaped quarantine"
    );
    assert!(
        producer.send(clipboard(8)).is_err(),
        "late producer was revived"
    );
    assert!(!producer.is_closed());
    assert!(producer.is_clipboard_closed());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), producer.failure())
            .await
            .is_err()
    );
    assert!(producer
        .send_audio_wave(vec![0; 1920], 0, Duration::from_millis(10), Instant::now())
        .unwrap());
    assert!(matches!(receiver.try_recv(), Ok(ServerEvent::Rdpsnd(_))));
    let (query, reply) = tokio::sync::oneshot::channel();
    producer.send(ServerEvent::GetLocalAddr(query)).unwrap();
    let ServerEvent::GetLocalAddr(query) = receiver.try_recv().unwrap() else {
        panic!("control request was disabled with clipboard");
    };
    query.send(None).unwrap();
    assert!(reply.await.unwrap().is_none());
    drop(admission);
    assert_eq!(sender.budget.available_permits(), DATA_BYTES);
}

#[test]
fn clipboard_queue_and_byte_exhaustion_quarantine_only_that_channel() {
    for oversized in [false, true] {
        let (sender, mut receiver) = channel_with_limits(1, 1024);
        if oversized {
            assert!(sender.send(clipboard(1024)).is_err());
        } else {
            sender.send(clipboard(1)).unwrap();
            assert!(sender.send(clipboard(1)).is_err());
        }
        assert!(sender.is_clipboard_closed());
        assert!(!sender.is_closed());
        assert!(receiver.try_recv().is_err());
        assert_eq!(sender.budget.available_permits(), 1024);
        assert!(sender
            .send_audio_wave(vec![1], 0, Duration::from_millis(1), Instant::now())
            .unwrap());
        assert!(matches!(receiver.try_recv(), Ok(ServerEvent::Rdpsnd(_))));
    }
}

#[test]
fn reconnect_resets_clipboard_but_stale_producer_cannot_quarantine_new_connection() {
    let (sender, mut receiver) = channel();
    let (guard, stale) = sender.start_session();
    stale.quarantine_clipboard("old worker stalled");
    drop(guard);
    let (_guard, fresh) = sender.start_session();
    stale.quarantine_clipboard("old worker finished late");
    assert!(stale.send(clipboard(1)).is_err());
    assert!(fresh.clipboard_failure().is_none());
    assert!(!fresh.is_clipboard_closed());
    fresh.send(clipboard(1)).unwrap();
    assert!(matches!(receiver.try_recv(), Ok(ServerEvent::Clipboard(_))));
}

#[test]
fn quarantine_never_waits_for_native_publication_and_completion_cannot_reopen_it() {
    let (sender, _receiver) = channel();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let publisher = sender.clone();
    let publication = std::thread::spawn(move || {
        publisher.with_clipboard_active(|| {
            entered_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            123
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (quarantined_tx, quarantined_rx) = std::sync::mpsc::channel();
    let watchdog = sender.clone();
    let quarantine = std::thread::spawn(move || {
        watchdog.quarantine_clipboard("test timeout");
        quarantined_tx.send(()).unwrap();
    });
    quarantined_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        sender.is_clipboard_closed(),
        "status access waited on native publication"
    );
    assert_eq!(
        sender.with_clipboard_active(|| panic!("late native publication")),
        None::<()>
    );
    release_tx.send(()).unwrap();
    assert_eq!(publication.join().unwrap(), Some(123));
    quarantine.join().unwrap();
    assert_eq!(
        sender.with_clipboard_active(|| panic!("late native publication")),
        None::<()>
    );
    assert!(!sender.is_closed());
}

#[test]
fn publication_panic_never_reopens_clipboard_or_closes_the_session() {
    let (sender, _receiver) = channel();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sender.with_clipboard_active(|| panic!("synthetic native publication panic"));
    }));
    assert!(result.is_err());
    assert!(sender.is_clipboard_closed());
    assert_eq!(
        sender.with_clipboard_active(|| panic!("poisoned gate resumed")),
        None::<()>
    );
    assert!(!sender.is_closed());
}

#[test]
fn session_close_and_failure_permanently_close_publication_admission() {
    for failed in [false, true] {
        let (sender, _receiver) = channel();
        let session = sender.session();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let publisher = sender.clone();
        let publication = std::thread::spawn(move || {
            publisher.with_clipboard_active(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if failed {
            session.fail("synthetic output failure");
        } else {
            session.close();
        }
        assert_eq!(session.clipboard.load(Ordering::Acquire), CLIPBOARD_CLOSED);
        assert!(
            sender.clipboard_failure().is_none(),
            "normal session closure is not a clipboard failure"
        );
        release_tx.send(()).unwrap();
        assert_eq!(publication.join().unwrap(), Some(()));
        assert_eq!(session.clipboard.load(Ordering::Acquire), CLIPBOARD_CLOSED);
        assert_eq!(
            sender.with_clipboard_active(|| panic!("stale native publication")),
            None::<()>
        );
    }
}
