use super::*;
use crate::{KeyboardEvent, MouseEvent, RdpServerDisplayUpdates};
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;

fn admission_test_server() -> RdpServer {
    RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build()
}

#[tokio::test]
async fn lossless_policy_keeps_resize_and_input_channels_without_advertising_gfx_or_remotefx() {
    use dvc::pdu::{CapabilitiesResponsePdu, CapsVersion, DrdynvcClientPdu, DrdynvcServerPdu};
    let server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .with_bitmap_quality(crate::BitmapQuality::Lossless)
        .build();
    assert!(!server.opts.has_remote_fx());
    assert!(!server.opts.has_image_remote_fx());
    assert!(!server.gfx_channel_enabled());
    let mut channels = server.dynamic_channels();
    let caps = encode_vec(&DrdynvcClientPdu::Capabilities(
        CapabilitiesResponsePdu::new(CapsVersion::V1),
    ))
    .unwrap();
    let creates = decode_dynamic_responses(channels.process(&caps).unwrap());
    let names: Vec<_> = creates
        .iter()
        .map(|pdu| match pdu {
            DrdynvcServerPdu::Create(create) => create.channel_name(),
            _ => panic!("expected dynamic channel create request"),
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ironrdp_displaycontrol::CHANNEL_NAME,
            ironrdp_ainput::CHANNEL_NAME
        ]
    );
}

#[derive(Debug, PartialEq, Eq)]
enum OrderedInputEvent {
    Mouse(MouseEvent),
    Key(bool, u8),
}

struct OrderedInput(Arc<StdMutex<Vec<OrderedInputEvent>>>);

impl RdpServerInputHandler for OrderedInput {
    fn keyboard(&mut self, event: KeyboardEvent) {
        let key = match event {
            KeyboardEvent::Pressed { code, .. } => Some((true, code)),
            KeyboardEvent::Released { code, .. } => Some((false, code)),
            _ => None,
        };
        if let Some((pressed, code)) = key {
            self.0
                .lock()
                .unwrap()
                .push(OrderedInputEvent::Key(pressed, code));
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        self.0.lock().unwrap().push(OrderedInputEvent::Mouse(event));
    }
}

fn ordered_input_server() -> (RdpServer, Arc<StdMutex<Vec<OrderedInputEvent>>>) {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_input_handler(OrderedInput(Arc::clone(&events)))
        .with_no_display()
        .build();
    (server, events)
}

#[tokio::test]
async fn fastpath_relative_wire_packets_preserve_motion_and_every_button() {
    use ironrdp_pdu::input::{mouse_rel::PointerRelFlags as F, MouseRelPdu};
    let (mut server, events) = ordered_input_server();
    for flags in [F::DOWN | F::BUTTON1 | F::XBUTTON2, F::BUTTON1 | F::XBUTTON2] {
        let relative = MouseRelPdu {
            flags,
            x_delta: -10,
            y_delta: 20,
        };
        let payload = encode_vec(&relative).unwrap();
        // One event, FASTPATH_INPUT_EVENT_MOUSEREL (5), followed by the
        // protocol encoder's pointer flags and signed relative deltas.
        let mut packet = vec![0x04, (3 + payload.len()) as u8, 0xa0];
        packet.extend(payload);
        server
            .dispatch_pdu(Action::FastPath, packet.as_slice().into(), 1003, 1002)
            .await
            .unwrap();
    }
    assert_eq!(
        *events.lock().unwrap(),
        [
            OrderedInputEvent::Mouse(MouseEvent::RelMove { x: -10, y: 20 }),
            OrderedInputEvent::Mouse(MouseEvent::LeftPressed),
            OrderedInputEvent::Mouse(MouseEvent::Button5Pressed),
            OrderedInputEvent::Mouse(MouseEvent::RelMove { x: -10, y: 20 }),
            OrderedInputEvent::Mouse(MouseEvent::LeftReleased),
            OrderedInputEvent::Mouse(MouseEvent::Button5Released),
        ]
    );
}

#[tokio::test]
async fn additional_mouse_input_cannot_be_overtaken_by_fastpath_keyboard() {
    use ironrdp_ainput::{ClientPdu, MouseEventFlags as F, MousePdu};
    use ironrdp_dvc::DvcProcessor as _;
    let (mut server, events) = ordered_input_server();
    let mut additional = AInputHandler::new(
        server.additional_input.clone(),
        server.input_activity.clone(),
    );
    let press = encode_vec(&ClientPdu::Mouse(MousePdu {
        time: 0,
        flags: F::BUTTON1 | F::DOWN,
        x: 10,
        y: 20,
    }))
    .unwrap();
    additional.process(7, &press).unwrap();
    // Intentionally do not yield to any worker between the two wire events.
    server
        .dispatch_pdu(
            Action::FastPath,
            [0x04, 0x04, 0x00, 0x1e].as_slice().into(),
            1003,
            1002,
        )
        .await
        .unwrap();
    let release = encode_vec(&ClientPdu::Mouse(MousePdu {
        time: 0,
        flags: F::BUTTON1,
        x: 30,
        y: 40,
    }))
    .unwrap();
    additional.process(7, &release).unwrap();
    server
        .dispatch_pdu(
            Action::FastPath,
            [0x04, 0x04, 0x01, 0x1e].as_slice().into(),
            1003,
            1002,
        )
        .await
        .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            OrderedInputEvent::Mouse(MouseEvent::Move { x: 10, y: 20 }),
            OrderedInputEvent::Mouse(MouseEvent::LeftPressed),
            OrderedInputEvent::Key(true, 0x1e),
            OrderedInputEvent::Mouse(MouseEvent::Move { x: 30, y: 40 }),
            OrderedInputEvent::Mouse(MouseEvent::LeftReleased),
            OrderedInputEvent::Key(false, 0x1e),
        ]
    );
    assert!(server.additional_input.pop().is_none());
}

#[tokio::test]
async fn resize_barrier_waits_for_ordered_mouse_and_keyboard_injection() {
    use ironrdp_ainput::{ClientPdu, MouseEventFlags as F, MousePdu};
    use ironrdp_dvc::DvcProcessor as _;
    use std::future::Future as _;
    let (mut server, events) = ordered_input_server();
    let mut additional = AInputHandler::new(
        server.additional_input.clone(),
        server.input_activity.clone(),
    );
    let packet = encode_vec(&ClientPdu::Mouse(MousePdu {
        time: 0,
        flags: F::BUTTON1 | F::DOWN,
        x: 10,
        y: 20,
    }))
    .unwrap();
    additional.process(7, &packet).unwrap();
    let handler = server.handler.clone();
    let held_handler = handler.lock().await;
    let server = Mutex::new(&mut server);
    let (stream, _peer) = tokio::io::duplex(64);
    let (_, mut native_writer) = split_tokio_framed(TokioFramed::new(stream));
    let mut writer = SharedWriter::new(&mut native_writer, Duration::from_secs(10));
    let mut barrier_writer = writer.clone();
    let permit = writer.try_begin_message().unwrap();
    let dispatch = RdpServer::dispatch_input_pdu(
        &server,
        Action::FastPath,
        [0x04, 0x04, 0x00, 0x1e].as_slice().into(),
        &mut writer,
        permit,
        1003,
        1002,
    );
    let barrier = barrier_writer.write_barrier(b"deactivate");
    tokio::pin!(dispatch, barrier);
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(dispatch.as_mut().poll(cx).is_pending())
        })
        .await
    );
    assert!(
        std::future::poll_fn(|cx| {
            std::task::Poll::Ready(barrier.as_mut().poll(cx).is_pending())
        })
        .await
    );
    assert!(events.lock().unwrap().is_empty());
    drop(held_handler);
    dispatch.await.unwrap();
    barrier.await.unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            OrderedInputEvent::Mouse(MouseEvent::Move { x: 10, y: 20 }),
            OrderedInputEvent::Mouse(MouseEvent::LeftPressed),
            OrderedInputEvent::Key(true, 0x1e),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn stage_success_does_not_refresh_total_handshake_budget() {
    let mut server = admission_test_server();
    server.set_handshake_timeout(Duration::from_millis(100));
    server.set_total_handshake_timeout(Duration::from_millis(150));
    let begin = tokio::time::Instant::now();
    server.handshake_deadline = Some(begin + server.total_handshake_timeout);
    server
        .await_connection_stage("RDP negotiation", async {
            tokio::time::sleep(Duration::from_millis(90)).await;
            Ok(())
        })
        .await
        .unwrap();
    let error = server
        .await_connection_stage("TLS handshake", async {
            tokio::time::sleep(Duration::from_millis(90)).await;
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("overall connection handshake deadline"));
    assert_eq!(
        tokio::time::Instant::now() - begin,
        Duration::from_millis(150)
    );
    assert_eq!(
        server.gfx_state.lock().unwrap().connection_outcome,
        Some(ConnectionOutcome::HandshakeTimeout)
    );
}

#[tokio::test(start_paused = true)]
async fn individual_stage_timeout_still_applies_before_total_budget() {
    let mut server = admission_test_server();
    server.set_handshake_timeout(Duration::from_millis(20));
    server.handshake_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(1));
    let error = server
        .await_connection_stage("TLS handshake", std::future::pending::<Result<()>>())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "TLS handshake timed out");
}

#[tokio::test(start_paused = true)]
async fn global_failure_backoff_keeps_controls_responsive() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let mut server = admission_test_server();
    let begin = tokio::time::Instant::now();
    for _ in 0..20 {
        server.admission.failed(address.ip(), begin);
    }
    assert!(server.admission.global_retry_at() > begin);
    let sender = server.event_sender().clone();
    let (reply, received) = oneshot::channel();
    sender.send(ServerEvent::GetLocalAddr(reply)).unwrap();
    let (served, ()) = tokio::join!(server.run_with_listener(listener), async {
        assert_eq!(received.await.unwrap(), Some(address));
        sender
            .send(ServerEvent::Quit("stop during cooldown".into()))
            .unwrap();
    });
    served.unwrap();
    assert_eq!(
        tokio::time::Instant::now(),
        begin,
        "controls should not wait for the admission timer"
    );
}

#[tokio::test]
async fn credential_change_during_authentication_has_structured_nonfailure_outcome() {
    let mut server = admission_test_server();
    server
        .event_sender()
        .send(ServerEvent::SetCredentials(Credentials {
            username: "replacement".into(),
            password: "not-logged".into(),
            domain: None,
        }))
        .unwrap();
    assert!(server
        .await_connection_stage(
            "CredSSP authentication",
            std::future::pending::<Result<()>>()
        )
        .await
        .is_err());
    assert_eq!(
        server.gfx_state.lock().unwrap().connection_outcome,
        Some(ConnectionOutcome::CredentialsChanged)
    );
}

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
    assert_eq!(
        server
            .dispatch_server_event(ServerEvent::Quit("test shutdown".into()), 1002)
            .unwrap()
            .0,
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

struct CaptureUpdateFailure(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl RdpServerDisplay for CaptureUpdateFailure {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 800,
            height: 600,
        }
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Self(Arc::clone(&self.0))))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for CaptureUpdateFailure {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        // A retry must fail this regression immediately instead of hanging in
        // the old busy loop, which could also starve an async timeout.
        assert_eq!(self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed), 0);
        bail!("terminal capture failure")
    }
}

#[tokio::test]
async fn capture_initialization_and_terminal_update_failures_end_the_session() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let displays: Vec<Box<dyn RdpServerDisplay>> = vec![
        Box::new(CaptureUnavailable),
        Box::new(CaptureUpdateFailure(Arc::clone(&calls))),
    ];
    for display in displays {
        let mut server = resize_test_server(display);
        let (stream, _peer) = tokio::io::duplex(1024);
        let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let size = DesktopSize {
            width: 800,
            height: 600,
        };
        let encoder =
            UpdateEncoder::new(size, CmdFlags::empty(), UpdateEncoderCodecs::new()).unwrap();
        let error = server
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
            .unwrap_err();
        assert!(format!("{error:#}").contains("capture"));
        assert_eq!(
            server.gfx_state.lock().unwrap().connection_outcome,
            Some(ConnectionOutcome::CaptureError)
        );
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
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

#[test]
fn quality_controls_restore_future_capabilities_without_expanding_current_negotiation() {
    let mut server = admission_test_server();
    server.set_bitmap_quality(crate::BitmapQuality::Lossless);
    server.activation_opts = server.opts.clone();
    server.connection_gfx_advertised = false;
    assert!(!server.activation_opts.has_remote_fx());
    server.apply_bitmap_quality(crate::BitmapQuality::HighQuality);
    assert!(server.opts.has_remote_fx());
    assert!(server.gfx_channel_enabled());
    assert!(!server.activation_opts.has_remote_fx());
    assert!(!server.connection_gfx_advertised);
    assert_eq!(
        server.gfx_state.lock().unwrap().requested_bitmap_quality(),
        Some(crate::BitmapQuality::HighQuality)
    );
}

#[test]
fn dequeued_graphics_policy_cannot_overwrite_a_more_recent_user_choice() {
    let mut server = admission_test_server();
    server
        .event_sender()
        .send(ServerEvent::SetBitmapQuality(
            crate::BitmapQuality::HighQuality,
        ))
        .unwrap();
    server.apply_bitmap_quality(crate::BitmapQuality::Lossless);
    assert_eq!(server.bitmap_quality, crate::BitmapQuality::HighQuality);
    let state = server.gfx_state.lock().unwrap();
    assert_eq!(
        state.requested_bitmap_quality(),
        Some(crate::BitmapQuality::HighQuality)
    );
}
