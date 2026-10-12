use super::*;
use crate::{KeyboardEvent, MouseEvent, RdpServerDisplayUpdates};
use std::sync::Mutex as StdMutex;

#[tokio::test(start_paused = true)]
async fn silent_peer_cannot_hold_the_listener_indefinitely() {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (client, accepted) = tokio::join!(TcpStream::connect(address), listener.accept());
    let _client = client.unwrap();
    let (stream, _) = accepted.unwrap();
    let mut server = RdpServer::builder()
        .with_addr(address)
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build();
    server.set_handshake_timeout(Duration::from_millis(25));
    let error = tokio::time::timeout(Duration::from_secs(1), server.run_connection(stream))
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("RDP negotiation timed out"));
}

#[tokio::test]
async fn pending_authentication_services_address_requests_and_quit() {
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build();
    server.local_addr = Some(([127, 0, 0, 1], 3389).into());
    let (reply, address) = oneshot::channel();
    let events = server.event_sender().clone();
    events.send(ServerEvent::GetLocalAddr(reply)).unwrap();
    let expected_address = server.local_addr;
    let request_shutdown = async {
        assert_eq!(address.await.unwrap(), expected_address);
        events
            .send(ServerEvent::Quit("test shutdown".into()))
            .unwrap();
    };
    let (result, ()) = tokio::join!(
        server.await_connection_stage("test authentication", std::future::pending::<Result<()>>()),
        request_shutdown,
    );
    let error = result.unwrap_err();
    assert!(error.to_string().contains("server stopped"));
    assert!(server.quit_requested);
}

#[tokio::test]
async fn channel_startup_preserves_queued_protocol_messages() {
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build();
    let (_session, sender) = server.ev_sender.start_session();
    sender
        .send(ServerEvent::Rdpsnd(RdpsndServerMessage::Close))
        .unwrap();
    server
        .await_connection_stage("channel startup", async { Ok(()) })
        .await
        .unwrap();
    assert!(matches!(
        server.ev_receiver.lock().await.try_recv(),
        Ok(ServerEvent::Rdpsnd(RdpsndServerMessage::Close))
    ));
}

#[tokio::test(start_paused = true)]
async fn inactive_client_loop_expires_even_with_capture_waiting() {
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build();
    server.set_idle_timeout(Some(Duration::from_millis(25)));
    let (stream, _peer) = tokio::io::duplex(1024);
    let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let size = DesktopSize {
        width: 800,
        height: 600,
    };
    let encoder = UpdateEncoder::new(size, CmdFlags::empty(), UpdateEncoderCodecs::new()).unwrap();
    let state = server
        .client_loop(
            &mut reader,
            &mut writer,
            1003,
            1002,
            encoder,
            size,
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(state, RunState::Disconnect);
}

#[tokio::test]
async fn session_quit_stops_the_listener_after_disconnect() {
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build();
    let (stream, _peer) = tokio::io::duplex(1024);
    let (_, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let mut events = vec![ServerEvent::Quit("test shutdown".into())];
    assert_eq!(
        server
            .dispatch_server_events(&mut events, &mut writer, 1002)
            .await
            .unwrap(),
        RunState::Disconnect
    );
    assert!(server.quit_requested);
}

#[tokio::test]
async fn prebound_listener_is_served_without_rebinding() {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let mut server = RdpServer::builder()
        .with_addr(address)
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build();
    server
        .event_sender()
        .send(ServerEvent::Quit("test".into()))
        .unwrap();
    server.run_with_listener(listener).await.unwrap();
    assert_eq!(server.local_addr, Some(address));
}

struct ResizeOutput;

#[async_trait::async_trait]
impl RdpServerDisplay for ResizeOutput {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 800,
            height: 600,
        }
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Self))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for ResizeOutput {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        Ok(Some(DisplayUpdate::Resize(DesktopSize {
            width: 1024,
            height: 768,
        })))
    }
}

#[tokio::test(start_paused = true)]
async fn query_and_quit_interrupt_a_stalled_display_write() {
    use tokio::io::AsyncReadExt as _;
    let mut server = resize_test_server(Box::new(ResizeOutput));
    let sender = server.event_sender().clone();
    server.local_addr = Some(([127, 0, 0, 1], 3389).into());
    let expected_address = server.local_addr;
    let (stream, mut peer) = tokio::io::duplex(1);
    let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let size = DesktopSize {
        width: 800,
        height: 600,
    };
    let encoder = UpdateEncoder::new(size, CmdFlags::empty(), UpdateEncoderCodecs::new()).unwrap();
    let controls = async {
        // Receiving a prefix proves the resize PDU write started. Leaving the
        // rest unread keeps that write blocked while the controls arrive.
        let mut prefix = [0];
        peer.read_exact(&mut prefix).await.unwrap();
        let (query, reply) = oneshot::channel();
        sender.send(ServerEvent::GetLocalAddr(query)).unwrap();
        assert_eq!(reply.await.unwrap(), expected_address);
        sender
            .send(ServerEvent::Quit("test shutdown".into()))
            .unwrap();
    };
    let started = tokio::time::Instant::now();
    let (state, ()) = tokio::join!(
        server.client_loop(
            &mut reader,
            &mut writer,
            1003,
            1002,
            encoder,
            size,
            Vec::new()
        ),
        controls,
    );
    assert_eq!(state.unwrap(), RunState::Disconnect);
    assert!(server.quit_requested);
    assert!(started.elapsed() < DEFAULT_WRITE_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn output_queue_failure_interrupts_a_stalled_display_write() {
    use tokio::io::AsyncReadExt as _;
    let mut server = resize_test_server(Box::new(ResizeOutput));
    let (_session, sender) = server.ev_sender.start_session();
    let (stream, mut peer) = tokio::io::duplex(1);
    let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let size = DesktopSize {
        width: 800,
        height: 600,
    };
    let encoder = UpdateEncoder::new(size, CmdFlags::empty(), UpdateEncoderCodecs::new()).unwrap();
    let congest = async {
        let mut prefix = [0];
        peer.read_exact(&mut prefix).await.unwrap();
        // A synchronous native callback burst outruns a blocked dispatcher.
        let mut rejected = false;
        for _ in 0..300 {
            if sender
                .send(ServerEvent::Rdpsnd(RdpsndServerMessage::Close))
                .is_err()
            {
                rejected = true;
                break;
            }
        }
        assert!(rejected);
    };
    let started = tokio::time::Instant::now();
    let (state, ()) = tokio::join!(
        server.client_loop(
            &mut reader,
            &mut writer,
            1003,
            1002,
            encoder,
            size,
            Vec::new()
        ),
        congest,
    );
    assert!(state
        .unwrap_err()
        .to_string()
        .contains("output event queue"));
    assert!(started.elapsed() < DEFAULT_WRITE_TIMEOUT);
    assert!(!server.quit_requested);
}

#[derive(Default)]
struct InputLifecycle {
    contexts: Vec<String>,
    connections: usize,
    disconnections: usize,
}

struct RecordingInput(Arc<StdMutex<InputLifecycle>>);

impl RdpServerInputHandler for RecordingInput {
    fn update_client_context(&mut self, context: ClientInputContext) {
        self.0.lock().unwrap().contexts.push(context.client_name);
    }

    fn on_connect(&mut self, _: ClientInputContext, _: Option<String>) {
        self.0.lock().unwrap().connections += 1;
    }

    fn on_disconnect(&mut self) {
        self.0.lock().unwrap().disconnections += 1;
    }

    fn keyboard(&mut self, _: KeyboardEvent) {}
    fn mouse(&mut self, _: MouseEvent) {}
}

struct CaptureUnavailable;

#[async_trait::async_trait]
impl RdpServerDisplay for CaptureUnavailable {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 1920,
            height: 1080,
        }
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        // Stop the activation deterministically after its input lifecycle
        // callbacks, without requiring a real client or platform capture API.
        bail!("capture deliberately unavailable")
    }
}

#[tokio::test]
async fn resize_reactivation_refreshes_context_without_starting_a_new_session() {
    let lifecycle = Arc::new(StdMutex::new(InputLifecycle::default()));
    let mut server = RdpServer::new(
        RdpServerOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            security: RdpServerSecurity::None,
            codecs: BitmapCodecs(Vec::new()),
        },
        Box::new(RecordingInput(Arc::clone(&lifecycle))),
        Box::new(CaptureUnavailable),
        None,
        None,
    );
    for (reactivation, client_name) in [(false, "initial"), (true, "resized")] {
        let (stream, _peer) = tokio::io::duplex(1024);
        let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let result = AcceptorResult {
            static_channels: StaticChannelSet::new(),
            capabilities: Vec::new(),
            input_events: Vec::new(),
            user_channel_id: 1002,
            io_channel_id: 1003,
            reactivation,
            client_name: client_name.to_owned(),
            keyboard_layout: 0x0409,
            client_desktop_size: None,
        };
        let error = server
            .client_accepted(
                &mut reader,
                &mut writer,
                result,
                DesktopSize {
                    width: 1280,
                    height: 720,
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("capture deliberately unavailable"));
    }

    let lifecycle = lifecycle.lock().unwrap();
    assert_eq!(lifecycle.contexts, ["initial", "resized"]);
    assert_eq!(lifecycle.connections, 1);
    assert_eq!(lifecycle.disconnections, 0);
}

struct RequestedSizeDisplay(mpsc::UnboundedSender<DesktopSize>);

#[async_trait::async_trait]
impl RdpServerDisplay for RequestedSizeDisplay {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 1920,
            height: 1080,
        }
    }

    fn request_resize(&mut self, width: u16, height: u16) {
        self.0.send(DesktopSize { width, height }).unwrap();
    }

    fn request_layout(&mut self, layout: ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout) {
        let (width, height) = layout.monitors()[0].dimensions();
        self.request_resize(width as u16, height as u16);
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        bail!("capture deliberately unavailable")
    }
}

fn resize_test_server(display: Box<dyn RdpServerDisplay>) -> RdpServer {
    RdpServer::new(
        RdpServerOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            security: RdpServerSecurity::None,
            codecs: BitmapCodecs(Vec::new()),
        },
        Box::new(RecordingInput(Arc::new(StdMutex::new(
            InputLifecycle::default(),
        )))),
        display,
        None,
        None,
    )
}

#[tokio::test]
async fn freerdp_confirm_active_echo_does_not_replace_the_initial_viewport() {
    let (requests, mut sizes) = mpsc::unbounded_channel();
    let mut server = resize_test_server(Box::new(RequestedSizeDisplay(requests)));
    let requested = DesktopSize {
        width: 1537,
        height: 865,
    };
    let advertised = DesktopSize {
        width: 1920,
        height: 1080,
    };
    for reactivation in [false, true] {
        let (stream, _peer) = tokio::io::duplex(1024);
        let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let result = AcceptorResult {
            static_channels: StaticChannelSet::new(),
            // FreeRDP echoes the server's size, not its original viewport.
            capabilities: capabilities::capabilities(&server.opts, advertised),
            input_events: Vec::new(),
            user_channel_id: 1002,
            io_channel_id: 1003,
            reactivation,
            client_name: "Guacamole RDP".into(),
            keyboard_layout: 0x0409,
            client_desktop_size: Some(requested),
        };
        let error = server
            .client_accepted(&mut reader, &mut writer, result, advertised)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("capture deliberately unavailable"));
        if !reactivation {
            assert_eq!(sizes.try_recv().unwrap(), requested);
        }
        // Reactivation must not replay the original browser size over later
        // Display Control changes, which use this same connection.
        assert!(matches!(
            sizes.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }
}

fn decode_dynamic_responses(messages: Vec<SvcMessage>) -> Vec<dvc::pdu::DrdynvcServerPdu> {
    ironrdp_svc::StaticVirtualChannel::chunkify(messages)
        .unwrap()
        .into_iter()
        .map(|chunk| {
            // Every test message fits in one SVC chunk. Strip its 8-byte Channel
            // PDU Header before decoding the actual DRDYNVC wire response.
            decode(&chunk.filled()[8..]).unwrap()
        })
        .collect()
}

#[tokio::test]
async fn bitmap_only_session_negotiates_display_control_and_receives_resize() {
    verify_display_control_resize(false).await;
}

#[tokio::test]
async fn gfx_session_negotiates_display_control_and_receives_resize() {
    verify_display_control_resize(true).await;
}

async fn verify_display_control_resize(gfx_enabled: bool) {
    use dvc::pdu::{
        CapabilitiesResponsePdu, CapsVersion, CreateResponsePdu, CreationStatus, DataPdu,
        DrdynvcClientPdu, DrdynvcDataPdu, DrdynvcServerPdu,
    };
    use ironrdp_displaycontrol::pdu::{DisplayControlMonitorLayout, DisplayControlPdu};
    let (requests, mut sizes) = mpsc::unbounded_channel();
    let mut server = resize_test_server(Box::new(RequestedSizeDisplay(requests)));
    server.set_gfx_enabled(gfx_enabled);
    let mut channels = server.dynamic_channels();
    let caps = encode_vec(&DrdynvcClientPdu::Capabilities(
        CapabilitiesResponsePdu::new(CapsVersion::V1),
    ))
    .unwrap();
    let creates = decode_dynamic_responses(channels.process(&caps).unwrap());
    assert_eq!(creates.len(), if gfx_enabled { 3 } else { 1 });
    let DrdynvcServerPdu::Create(create) = &creates[0] else {
        panic!("expected Display Control create request")
    };
    assert_eq!(create.channel_name(), ironrdp_displaycontrol::CHANNEL_NAME);
    let channel_id = create.channel_id();
    let create = encode_vec(&DrdynvcClientPdu::Create(CreateResponsePdu::new(
        channel_id,
        CreationStatus::OK,
    )))
    .unwrap();
    let response = decode_dynamic_responses(channels.process(&create).unwrap());
    let [DrdynvcServerPdu::Data(DrdynvcDataPdu::Data(data))] = response.as_slice() else {
        panic!("expected Display Control capabilities")
    };
    let DisplayControlPdu::Caps(caps) = decode(data.data()).unwrap() else {
        panic!("expected Display Control capabilities")
    };
    assert!(caps.max_monitor_area() >= 3840 * 2160);
    let layout: DisplayControlPdu =
        DisplayControlMonitorLayout::new_single_primary_monitor(1600, 900, None, None)
            .unwrap()
            .into();
    let request = encode_vec(&DrdynvcClientPdu::Data(DrdynvcDataPdu::Data(DataPdu::new(
        channel_id,
        encode_vec(&layout).unwrap(),
    ))))
    .unwrap();
    assert!(channels.process(&request).unwrap().is_empty());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), sizes.recv())
            .await
            .unwrap(),
        Some(DesktopSize {
            width: 1600,
            height: 900
        })
    );
}
