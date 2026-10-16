use super::*;
use ironrdp_tokio::{split_tokio_framed, TokioFramed};
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

fn server() -> RdpServer {
    RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build()
}

#[tokio::test(start_paused = true)]
async fn full_startup_queue_pauses_reads_and_replays_every_packet_in_order() {
    let mut server = server();
    let (stream, mut peer) = tokio::io::duplex(4096);
    let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
    let mut pending = PendingInput::default();
    let mut expected = Vec::new();
    // Simulate packets already retained across resize, sharing the same budget.
    for code in [250, 251] {
        let bytes = vec![0, 3, code];
        pending
            .push(Action::FastPath, bytes.as_slice().into())
            .unwrap();
        expected.push(bytes);
    }
    for code in 0..140 {
        let bytes = vec![0, 3, code];
        peer.write_all(&bytes).await.unwrap();
        expected.push(bytes);
    }
    let ready = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(())
    };
    let (_, mut pending) = server
        .await_capture_ready(&mut reader, 1003, pending, ready)
        .await
        .unwrap()
        .unwrap();
    assert!(!pending.can_read());
    assert_eq!(pending.len(), 128);
    assert_eq!(pending.bytes(), 128 * 3);
    let mut received: Vec<_> = std::iter::from_fn(|| pending.pop())
        .map(|(_, bytes)| bytes.to_vec())
        .collect();
    while received.len() < expected.len() {
        received.push(reader.read_pdu().await.unwrap().1.to_vec());
    }
    assert_eq!(received, expected);
}

#[tokio::test(start_paused = true)]
async fn saturated_startup_still_honors_shutdown_revocation_idle_and_deadline() {
    for mode in 0..4 {
        let mut server = server();
        server.set_handshake_timeout(Duration::from_millis(25));
        if mode == 3 {
            server.set_idle_timeout(Some(Duration::from_millis(15)));
        }
        let (stream, _peer) = tokio::io::duplex(1024);
        let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
        let mut pending = PendingInput::default();
        while pending.can_read() {
            pending
                .push(Action::FastPath, (&[0, 2][..]).into())
                .unwrap();
        }
        let sender = server.event_sender().clone();
        let (result, ()) = tokio::join!(
            server.await_capture_ready(
                &mut reader,
                1003,
                pending,
                std::future::pending::<Result<()>>()
            ),
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                match mode {
                    0 => sender.send(ServerEvent::Quit("test".into())).unwrap(),
                    1 => sender
                        .send(ServerEvent::SetCredentials(super::super::Credentials {
                            username: "fixture".into(),
                            password: "fixture-secret".into(),
                            domain: None,
                        }))
                        .unwrap(),
                    _ => {}
                }
            }
        );
        if mode == 2 {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("capture activation timed out"));
        } else {
            assert!(result.unwrap().is_none());
        }
        let expected = [
            ConnectionOutcome::ServerShutdown,
            ConnectionOutcome::CredentialsChanged,
            ConnectionOutcome::CaptureError,
            ConnectionOutcome::IdleTimeout,
        ][mode];
        assert_eq!(
            server.gfx_state.lock().unwrap().connection_outcome,
            Some(expected)
        );
    }
}

#[tokio::test]
async fn pending_start_services_controls_and_cancels_without_waiting() {
    let mut server = server();
    let (stream, _peer) = tokio::io::duplex(1024);
    let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
    let sender = server.event_sender().clone();
    let (reply, received) = tokio::sync::oneshot::channel();
    sender.send(ServerEvent::GetLocalAddr(reply)).unwrap();
    let (result, ()) = tokio::join!(
        server.await_capture_ready(
            &mut reader,
            1003,
            PendingInput::default(),
            std::future::pending::<Result<()>>()
        ),
        async {
            received.await.unwrap();
            sender.send(ServerEvent::Quit("test".into())).unwrap();
        }
    );
    assert!(result.unwrap().is_none());
    assert!(server.quit_requested);
}

#[tokio::test]
async fn credential_revocation_cancels_pending_start() {
    let mut server = server();
    let (stream, _peer) = tokio::io::duplex(1024);
    let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
    server
        .event_sender()
        .send(ServerEvent::SetCredentials(super::super::Credentials {
            username: "fixture".into(),
            password: "fixture-secret".into(),
            domain: None,
        }))
        .unwrap();
    assert!(server
        .await_capture_ready(
            &mut reader,
            1003,
            PendingInput::default(),
            std::future::pending::<Result<()>>()
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        server.gfx_state.lock().unwrap().connection_outcome,
        Some(ConnectionOutcome::CredentialsChanged)
    );
}

#[tokio::test]
async fn peer_close_cancels_pending_start() {
    let mut server = server();
    let (stream, peer) = tokio::io::duplex(1024);
    let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
    drop(peer);
    assert!(server
        .await_capture_ready(
            &mut reader,
            1003,
            PendingInput::default(),
            std::future::pending::<Result<()>>()
        )
        .await
        .is_err());
    assert_eq!(
        server.gfx_state.lock().unwrap().connection_outcome,
        Some(ConnectionOutcome::PeerEof)
    );
}

#[tokio::test(start_paused = true)]
async fn pending_start_has_a_fixed_deadline() {
    let mut server = server();
    server.set_handshake_timeout(Duration::from_millis(25));
    let (stream, _peer) = tokio::io::duplex(1024);
    let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
    let begin = tokio::time::Instant::now();
    let error = server
        .await_capture_ready(
            &mut reader,
            1003,
            PendingInput::default(),
            std::future::pending::<Result<()>>(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("capture activation timed out"));
    assert_eq!(
        tokio::time::Instant::now() - begin,
        Duration::from_millis(25)
    );
}

#[tokio::test]
async fn capture_readiness_preserves_buffered_pdu_order() {
    let mut server = server();
    let (stream, mut peer) = tokio::io::duplex(4096);
    let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
    peer.write_all(&[0, 3, 1, 0, 3, 2]).await.unwrap();
    let ready = async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        Ok(42)
    };
    let (value, mut pdus) = server
        .await_capture_ready(&mut reader, 1003, PendingInput::default(), ready)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value, 42);
    assert_eq!(
        std::iter::from_fn(|| pdus.pop())
            .map(|(_, bytes)| bytes.to_vec())
            .collect::<Vec<_>>(),
        vec![&[0, 3, 1][..], &[0, 3, 2][..]]
    );
}

fn slow_path(pdu: ShareDataPdu, channel_id: u16) -> Vec<u8> {
    use ironrdp_core::encode_vec;
    use ironrdp_pdu::rdp::{
        client_info::CompressionType,
        headers::{CompressionFlags, ShareDataHeader, StreamPriority},
    };
    let control = ShareControlHeader {
        share_id: 0,
        pdu_source: 1002,
        share_control_pdu: ShareControlPdu::Data(ShareDataHeader {
            share_data_pdu: pdu,
            stream_priority: StreamPriority::Undefined,
            compression_flags: CompressionFlags::empty(),
            compression_type: CompressionType::K8,
        }),
    };
    encode_vec(&X224(mcs::SendDataRequest {
        initiator_id: 1002,
        channel_id,
        user_data: encode_vec(&control).unwrap().into(),
    }))
    .unwrap()
}

fn pointer_input(channel_id: u16) -> Vec<u8> {
    use ironrdp_pdu::input::{mouse::PointerFlags, InputEventPdu, MousePdu};
    slow_path(
        ShareDataPdu::Input(InputEventPdu(vec![InputEvent::Mouse(MousePdu {
            flags: PointerFlags::MOVE,
            number_of_wheel_rotation_units: 0,
            x_position: 10,
            y_position: 20,
        })])),
        channel_id,
    )
}

#[derive(Clone)]
struct RecordingInput(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl crate::RdpServerInputHandler for RecordingInput {
    fn keyboard(&mut self, _: crate::KeyboardEvent) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn mouse(&mut self, _: crate::MouseEvent) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[tokio::test(start_paused = true)]
async fn buffered_valid_input_refreshes_idle_without_native_injection() {
    // FastPath scan-code press, followed by an independently tested
    // io-channel slow-path pointer event. Each arrives near idle expiry.
    for input in [vec![0x04, 0x04, 0x00, 0x1e], pointer_input(1003)] {
        let recorded = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut server = RdpServer::builder()
            .with_addr(([127, 0, 0, 1], 0))
            .with_no_security()
            .with_input_handler(RecordingInput(recorded.clone()))
            .with_no_display()
            .build();
        server.set_idle_timeout(Some(Duration::from_millis(30)));
        let begin = tokio::time::Instant::now();
        let (stream, mut peer) = tokio::io::duplex(4096);
        let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
        let expected = input.clone();
        let ready = async {
            tokio::time::sleep(Duration::from_millis(40)).await;
            Ok(())
        };
        let (ready, ()) = tokio::join!(
            server.await_capture_ready(&mut reader, 1003, PendingInput::default(), ready),
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                peer.write_all(&input).await.unwrap();
                // Retain the peer until readiness; EOF is an independent
                // cancellation reason and must not win this test.
                tokio::time::sleep(Duration::from_millis(30)).await;
            },
        );
        let (_, mut buffered) = ready
            .unwrap()
            .expect("active input must postpone idle expiry");
        assert_eq!(buffered.len(), 1);
        assert_eq!(buffered.pop().unwrap().1.as_ref(), expected.as_slice());
        assert_eq!(
            *server.input_activity.borrow(),
            begin + Duration::from_millis(20)
        );
        assert_eq!(recorded.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn buffered_noninput_wrong_channel_and_malformed_pdus_do_not_extend_idle() {
    let packets = [
        vec![0x04, 0x07, 0xc0, 0, 0, 0, 0], // FastPath QoE timestamp only
        vec![0x04, 0x03, 0x00],             // Framed packet missing the scan code
        pointer_input(1004),                // Input-shaped payload on a different channel
        slow_path(ShareDataPdu::ShutdownRequest, 1003),
    ];
    assert!(
        decode::<FastPathInput>(&packets[0]).is_ok(),
        "QoE fixture must be valid input framing"
    );
    for packet in packets {
        let mut server = server();
        server.set_idle_timeout(Some(Duration::from_millis(30)));
        let begin = tokio::time::Instant::now();
        let (stream, mut peer) = tokio::io::duplex(4096);
        let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
        let (result, ()) = tokio::join!(
            server.await_capture_ready(
                &mut reader,
                1003,
                PendingInput::default(),
                std::future::pending::<Result<()>>()
            ),
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                peer.write_all(&packet).await.unwrap();
                tokio::time::sleep(Duration::from_millis(30)).await;
            },
        );
        assert!(result.unwrap().is_none());
        assert_eq!(*server.input_activity.borrow(), begin);
        assert_eq!(
            server.gfx_state.lock().unwrap().connection_outcome,
            Some(ConnectionOutcome::IdleTimeout)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn oversized_activation_timeout_returns_an_error_without_panicking() {
    let mut server = server();
    server.set_handshake_timeout(Duration::MAX);
    let (stream, _peer) = tokio::io::duplex(1024);
    let (mut reader, _writer) = split_tokio_framed(TokioFramed::new(stream));
    let error = server
        .await_capture_ready(
            &mut reader,
            1003,
            PendingInput::default(),
            std::future::pending::<Result<()>>(),
        )
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("exceeds the clock's supported range"));
}
