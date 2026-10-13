use super::*;
use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncReadExt as _;

#[derive(Debug)]
struct RecordingClipboard(Arc<AtomicUsize>);
ironrdp_core::impl_as_any!(RecordingClipboard);

impl CliprdrBackend for RecordingClipboard {
    fn temporary_directory(&self) -> &str {
        "/tmp"
    }
    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }
    fn on_ready(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_request_format_list(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}
    fn on_remote_copy(&mut self, _: &[ClipboardFormat]) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_format_data_request(&mut self, _: FormatDataRequest) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_format_data_response(&mut self, _: FormatDataResponse<'_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_file_contents_request(&mut self, _: FileContentsRequest) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_lock(&mut self, _: LockDataId) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn on_unlock(&mut self, _: LockDataId) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct RecordingInput(Arc<AtomicUsize>);
impl RdpServerInputHandler for RecordingInput {
    fn keyboard(&mut self, _: crate::KeyboardEvent) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn mouse(&mut self, _: crate::MouseEvent) {}
}

fn server() -> (RdpServer, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let input = Arc::new(AtomicUsize::new(0));
    let clipboard = Arc::new(AtomicUsize::new(0));
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_input_handler(RecordingInput(input.clone()))
        .with_no_display()
        .build();
    server
        .static_channels
        .insert(CliprdrServer::new(Box::new(RecordingClipboard(
            clipboard.clone(),
        ))));
    server
        .static_channels
        .attach_channel_id(core::any::TypeId::of::<CliprdrServer>(), 1004);
    (server, input, clipboard)
}

fn clipboard_frame(bytes: &[u8]) -> Vec<u8> {
    encode_vec(&X224(SendDataRequest {
        initiator_id: 1002,
        channel_id: 1004,
        user_data: bytes.into(),
    }))
    .unwrap()
}

#[tokio::test]
async fn malformed_clipboard_fragment_quarantines_only_clipboard_and_ignores_late_fragments() {
    let (mut server, input, clipboard) = server();
    let (_session, _producer) = server.ev_sender.start_session();
    // The MCS frame is valid, but its SVC header is truncated. Only this optional
    // channel is unusable; the connection's outer framing remains trustworthy.
    let frame = clipboard_frame(&[1]);
    let (disconnect, output) = server.handle_x224(1003, 1002, &frame).await.unwrap();
    assert!(!disconnect);
    assert!(output.is_none());
    assert_eq!(
        server.ev_sender.clipboard_failure(),
        Some("clipboard protocol input failed")
    );
    assert!(!server.ev_sender.is_closed());

    for payload in [&[][..], &[0xff; 8][..]] {
        let (disconnect, output) = server
            .handle_x224(1003, 1002, &clipboard_frame(payload))
            .await
            .unwrap();
        assert!(!disconnect);
        assert!(output.is_none());
    }
    assert_eq!(
        clipboard.load(Ordering::Relaxed),
        0,
        "retired backend received late input"
    );
    let packet = [0x04, 0x04, 0x00, 0x1e]; // one Fast-Path key-down
    let (state, _) = server
        .dispatch_pdu(Action::FastPath, packet.as_slice().into(), 1003, 1002)
        .await
        .unwrap();
    assert_eq!(state, RunState::Continue);
    assert_eq!(input.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn direct_clipboard_failure_and_late_file_output_never_disconnect_or_restore_channel() {
    let (mut server, _, _) = server();
    let (_session, _producer) = server.ev_sender.start_session();
    let (state, output) = server
        .dispatch_server_event(
            ServerEvent::Clipboard(ClipboardMessage::Error(Box::new(std::io::Error::other(
                "producer failed",
            )))),
            1002,
        )
        .unwrap();
    assert_eq!(state, RunState::Continue);
    assert!(output.is_none());
    assert!(!server.ev_sender.is_closed());
    for event in [
        ServerEvent::Clipboard(ClipboardMessage::SendFormatData(
            FormatDataResponse::new_data(vec![1]),
        )),
        ServerEvent::ClipboardFileContents(FileContentsResponse::new_data_response(1, vec![1])),
    ] {
        let (state, output) = server.dispatch_server_event(event, 1002).unwrap();
        assert_eq!(state, RunState::Continue);
        assert!(output.is_none());
    }
    assert!(server.ev_sender.is_clipboard_closed());
}

struct ClipboardTestDisplay(Arc<Mutex<tokio::sync::mpsc::Receiver<DisplayUpdate>>>);

#[async_trait::async_trait]
impl RdpServerDisplay for ClipboardTestDisplay {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 64,
            height: 64,
        }
    }
    async fn updates(&mut self) -> Result<Box<dyn crate::RdpServerDisplayUpdates>> {
        Ok(Box::new(Self(self.0.clone())))
    }
}

#[async_trait::async_trait]
impl crate::RdpServerDisplayUpdates for ClipboardTestDisplay {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        Ok(self.0.lock().await.recv().await)
    }
}

#[tokio::test]
async fn quarantined_clipboard_reports_status_while_real_session_keeps_display_and_input_alive() {
    let input = Arc::new(AtomicUsize::new(0));
    let (updates, receive_updates) = tokio::sync::mpsc::channel(1);
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_input_handler(RecordingInput(input.clone()))
        .with_display_handler(ClipboardTestDisplay(Arc::new(Mutex::new(receive_updates))))
        .build();
    let (_guard, sender) = server.ev_sender.start_session();
    let gfx = server.gfx_state.clone();
    let (stream, mut peer) = tokio::io::duplex(1024);
    let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let size = DesktopSize {
        width: 64,
        height: 64,
    };
    let encoder =
        UpdateEncoder::new(size, CmdFlags::SET_SURFACE_BITS, UpdateEncoderCodecs::new()).unwrap();
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
            sender.quarantine_clipboard("synthetic clipboard timeout");
            tokio::time::timeout(Duration::from_secs(5), async {
                while !gfx.lock().unwrap().clipboard_disabled {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("clipboard status did not reach diagnostics");
            peer.write_all(&[0x04, 0x04, 0x00, 0x1e]).await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while input.load(Ordering::Relaxed) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("clipboard quarantine stopped input");
            updates
                .send(DisplayUpdate::Bitmap(crate::BitmapUpdate {
                    x: 0,
                    y: 0,
                    width: core::num::NonZeroU16::new(1).unwrap(),
                    height: core::num::NonZeroU16::new(1).unwrap(),
                    stride: core::num::NonZeroUsize::new(4).unwrap(),
                    format: crate::PixelFormat::BgrA32,
                    data: vec![1, 2, 3, 255].into(),
                    dirty_rects: Vec::new(),
                }))
                .await
                .unwrap();
            let mut packet = [0; 256];
            let count = tokio::time::timeout(Duration::from_secs(5), peer.read(&mut packet))
                .await
                .expect("clipboard quarantine stopped display")
                .unwrap();
            assert!(count > 4);
            assert_eq!(packet[0] & 3, 0, "expected a Fast-Path display update");
            sender
                .send(ServerEvent::Quit("test complete".into()))
                .unwrap();
        },
    );
    assert_eq!(result.unwrap(), RunState::Disconnect);
    assert!(gfx.lock().unwrap().clipboard_disabled);
    // A graphics reactivation stays in the same clipboard session.
    gfx.lock().unwrap().reset_connection_state(false);
    assert!(gfx.lock().unwrap().clipboard_disabled);
    // A fresh connection constructs fresh diagnostics and a fresh channel gate.
    *gfx.lock().unwrap() = GfxState::new(64, 64, false, false);
    let (_next_guard, fresh) = server.ev_sender.start_session();
    sender.quarantine_clipboard("stale worker completed");
    assert!(tokio::time::timeout(
        Duration::from_millis(10),
        clipboard_status::mirror(fresh, gfx.clone())
    )
    .await
    .is_err());
    assert!(!gfx.lock().unwrap().clipboard_disabled);
}
