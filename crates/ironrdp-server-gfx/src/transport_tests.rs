//! Exercise the real session driver while its outbound event socket is stalled.

use super::*;
use crate::{KeyboardEvent, MouseEvent, RdpServerDisplayUpdates};
use ironrdp_cliprdr::pdu::FileContentsFlags;
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc;

struct KeyboardSignal(mpsc::UnboundedSender<KeyboardEvent>);

impl RdpServerInputHandler for KeyboardSignal {
    fn keyboard(&mut self, event: KeyboardEvent) {
        let _ = self.0.send(event);
    }

    fn mouse(&mut self, _: MouseEvent) {}
}

#[tokio::test(start_paused = true)]
async fn keyboard_input_is_dispatched_while_clipboard_output_waits_for_peer() {
    let (keyboard, mut keys) = mpsc::unbounded_channel();
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_input_handler(KeyboardSignal(keyboard))
        .with_no_display()
        .build();
    // File-content requests require the negotiated channel ID, without invoking
    // a clipboard backend. Install that negotiation result for this wire test.
    server
        .static_channels
        .attach_channel_id(std::any::TypeId::of::<CliprdrServer>(), 1005);
    let (_session, sender) = server.ev_sender.start_session();
    sender
        .send(ServerEvent::ClipboardFileContentsRequest(
            FileContentsRequest {
                stream_id: 1,
                index: 0,
                flags: FileContentsFlags::SIZE,
                position: 0,
                requested_size: 8,
                data_id: None,
            },
        ))
        .unwrap();
    let (stream, mut peer) = tokio::io::duplex(1);
    let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let size = DesktopSize {
        width: 800,
        height: 600,
    };
    let encoder = UpdateEncoder::new(size, CmdFlags::empty(), UpdateEncoderCodecs::new()).unwrap();
    let (result, ()) = tokio::join!(
        server.client_loop(
            &mut reader,
            &mut writer,
            1003,
            1002,
            encoder,
            size,
            Vec::new()
        ),
        async {
            let mut prefix = [0];
            peer.read_exact(&mut prefix).await.unwrap();
            assert_eq!(prefix[0], 3, "clipboard output must have begun");
            // One event, four-byte Fast-Path input packet: scan-code A down.
            // The client reads no more output until after we observe delivery.
            peer.write_all(&[0x04, 0x04, 0x00, 0x1e]).await.unwrap();
            assert!(tokio::time::timeout(Duration::from_millis(50), keys.recv())
                .await
                .expect("keyboard input was blocked by clipboard output")
                .is_some());
            sender
                .send(ServerEvent::Quit("test complete".into()))
                .unwrap();
        },
    );
    assert_eq!(result.unwrap(), RunState::Disconnect);
    assert!(server.quit_requested);
}

struct ControlledDisplay {
    updates: Arc<Mutex<mpsc::Receiver<DisplayUpdate>>>,
    resize_started: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl RdpServerDisplay for ControlledDisplay {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 800,
            height: 600,
        }
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Self {
            updates: self.updates.clone(),
            resize_started: self.resize_started.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for ControlledDisplay {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        let update = self.updates.lock().await.recv().await;
        if matches!(update, Some(DisplayUpdate::Resize(_))) {
            self.resize_started.notify_one();
        }
        Ok(update)
    }
}

fn file_request(stream_id: u32) -> ServerEvent {
    ServerEvent::ClipboardFileContentsRequest(FileContentsRequest {
        stream_id,
        index: 0,
        flags: FileContentsFlags::SIZE,
        position: 0,
        requested_size: 8,
        data_id: None,
    })
}

async fn read_tpkt(peer: &mut tokio::io::DuplexStream, first: Option<u8>) -> Vec<u8> {
    let mut header = [0; 4];
    if let Some(first) = first {
        header[0] = first;
        peer.read_exact(&mut header[1..]).await.unwrap();
    } else {
        peer.read_exact(&mut header).await.unwrap();
    }
    assert_eq!(header[0], 3);
    let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let mut data = vec![0; length];
    data[..4].copy_from_slice(&header);
    peer.read_exact(&mut data[4..]).await.unwrap();
    data
}

fn file_request_stream_id(data: &[u8]) -> u32 {
    let mcs: X224<SendDataIndication<'_>> = decode(data).unwrap();
    assert_eq!(mcs.0.channel_id, 1005);
    let ClipboardPdu::FileContentsRequest(request) = decode(&mcs.0.user_data[8..]).unwrap() else {
        panic!("expected clipboard file request");
    };
    request.stream_id
}

#[tokio::test(start_paused = true)]
async fn resize_drains_prepared_output_and_replays_later_input_and_events_once() {
    let (keyboard, mut keys) = mpsc::unbounded_channel();
    let (updates, receive_updates) = mpsc::channel(1);
    let resize_started = Arc::new(tokio::sync::Notify::new());
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_input_handler(KeyboardSignal(keyboard))
        .with_display_handler(ControlledDisplay {
            updates: Arc::new(Mutex::new(receive_updates)),
            resize_started: resize_started.clone(),
        })
        .build();
    server
        .static_channels
        .attach_channel_id(std::any::TypeId::of::<CliprdrServer>(), 1005);
    let (_session, sender) = server.ev_sender.start_session();
    sender.send(file_request(1)).unwrap();
    let (stream, mut peer) = tokio::io::duplex(1);
    let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let size = DesktopSize {
        width: 800,
        height: 600,
    };
    let resized = DesktopSize {
        width: 1024,
        height: 768,
    };
    let encoder = UpdateEncoder::new(size, CmdFlags::empty(), UpdateEncoderCodecs::new()).unwrap();
    let (first, ()) = tokio::join!(
        server.client_loop(
            &mut reader,
            &mut writer,
            1003,
            1002,
            encoder,
            size,
            Vec::new()
        ),
        async {
            let mut prefix = [0];
            peer.read_exact(&mut prefix).await.unwrap();
            updates.send(DisplayUpdate::Resize(resized)).await.unwrap();
            resize_started.notified().await;
            // Resize now waits for the admitted first clipboard message. Both
            // a consumed input PDU and an event arriving behind that barrier
            // must survive cancellation of this activation's dispatcher.
            peer.write_all(&[0x04, 0x04, 0x00, 0x1e]).await.unwrap();
            sender.send(file_request(2)).unwrap();
            let request = read_tpkt(&mut peer, Some(prefix[0])).await;
            assert_eq!(file_request_stream_id(&request), 1);
            let deactivate = read_tpkt(&mut peer, None).await;
            let mcs: X224<SendDataIndication<'_>> = decode(&deactivate).unwrap();
            assert_eq!(mcs.0.channel_id, 1003);
        },
    );
    assert_eq!(
        first.unwrap(),
        RunState::DeactivationReactivation {
            desktop_size: resized
        }
    );
    assert!(
        !server.deferred_input.is_empty(),
        "input must survive the resize barrier"
    );
    assert!(matches!(
        keys.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    // The acceptor handshake is not needed to test replay; re-enter the same
    // session driver with the retained static-channel state and socket.
    let encoder =
        UpdateEncoder::new(resized, CmdFlags::empty(), UpdateEncoderCodecs::new()).unwrap();
    let (second, ()) = tokio::join!(
        server.client_loop(
            &mut reader,
            &mut writer,
            1003,
            1002,
            encoder,
            resized,
            Vec::new()
        ),
        async {
            assert!(keys.recv().await.is_some());
            let request = read_tpkt(&mut peer, None).await;
            assert_eq!(file_request_stream_id(&request), 2);
            assert!(matches!(
                keys.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            sender
                .send(ServerEvent::Quit("test complete".into()))
                .unwrap();
        },
    );
    assert_eq!(second.unwrap(), RunState::Disconnect);
    assert!(server.deferred_input.is_empty());
}
