use core::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use ironrdp_acceptor::{Acceptor, AcceptorResult, BeginResult, DesktopSize};
use ironrdp_async::Framed;
use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_cliprdr::pdu::{ClipboardPdu, FileContentsRequest, OwnedFileContentsResponse};
use ironrdp_cliprdr::CliprdrServer;
use ironrdp_core::{decode, encode_vec, impl_as_any};
use ironrdp_pdu::input::fast_path::{FastPathInput, FastPathInputEvent};
use ironrdp_pdu::input::InputEventPdu;
use ironrdp_pdu::mcs::{SendDataIndication, SendDataRequest};
use ironrdp_pdu::rdp::capability_sets::{
    BitmapCodecs, CapabilitySet, CmdFlags, CodecProperty, GeneralExtraFlags,
};
pub use ironrdp_pdu::rdp::client_info::Credentials;
use ironrdp_pdu::rdp::headers::{ServerDeactivateAll, ShareControlPdu};
use ironrdp_pdu::x224::X224;
use ironrdp_pdu::{decode_err, mcs, nego, rdp, Action, PduResult};
use ironrdp_svc::{
    server_encode_svc_messages, ChannelFlags, StaticChannelId, StaticChannelSet, SvcMessage,
    SvcProcessor,
};
use ironrdp_tokio::{
    split_tokio_framed, unsplit_tokio_framed, FramedRead, FramedWrite, TokioFramed,
};
use rdpsnd::server::{RdpsndServer, RdpsndServerMessage};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio::task;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, trace, warn};
use {ironrdp_dvc as dvc, ironrdp_rdpsnd as rdpsnd};

use crate::clipboard::CliprdrServerFactory;
use crate::display::{DisplayUpdate, RdpServerDisplay};
use crate::display_control::DisplayControlChannel;
use crate::dynamic_channels::DynamicChannels;
use crate::encoder::{UpdateEncoder, UpdateEncoderCodecs};
use crate::gfx::{GfxHandler, GfxState};
use crate::handler::{ClientInputContext, RdpServerInputHandler};
use crate::{builder, capabilities, SoundServerFactory};

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

#[path = "connection_control.rs"]
mod connection_control;
use connection_control::{wait_for_idle_timeout, DEFAULT_HANDSHAKE_TIMEOUT};

#[path = "event_queue.rs"]
mod event_queue;
pub use event_queue::{ServerEventRx, ServerEventSendError, ServerEventTx};
#[path = "output.rs"]
mod output;
use output::{SharedWriter, DEFAULT_WRITE_TIMEOUT};

#[derive(Clone)]
pub struct RdpServerOptions {
    pub addr: SocketAddr,
    pub security: RdpServerSecurity,
    pub codecs: BitmapCodecs,
}

impl RdpServerOptions {
    fn has_image_remote_fx(&self) -> bool {
        self.codecs
            .0
            .iter()
            .any(|codec| matches!(codec.property, CodecProperty::ImageRemoteFx(_)))
    }

    fn has_remote_fx(&self) -> bool {
        self.codecs
            .0
            .iter()
            .any(|codec| matches!(codec.property, CodecProperty::RemoteFx(_)))
    }

    #[cfg(feature = "qoi")]
    fn has_qoi(&self) -> bool {
        self.codecs
            .0
            .iter()
            .any(|codec| matches!(codec.property, CodecProperty::Qoi))
    }

    #[cfg(feature = "qoiz")]
    fn has_qoiz(&self) -> bool {
        self.codecs
            .0
            .iter()
            .any(|codec| matches!(codec.property, CodecProperty::QoiZ))
    }
}

#[derive(Clone)]
pub enum RdpServerSecurity {
    None,
    Tls(TlsAcceptor),
    /// Used for both hybrid + hybrid-ex.
    Hybrid((TlsAcceptor, Vec<u8>)),
}

impl RdpServerSecurity {
    pub fn flag(&self) -> nego::SecurityProtocol {
        match self {
            RdpServerSecurity::None => nego::SecurityProtocol::empty(),
            RdpServerSecurity::Tls(_) => nego::SecurityProtocol::SSL,
            // Advertise HYBRID + HYBRID_EX + SSL so the acceptor can negotiate
            // down to TLS-only if the client doesn't support NLA
            RdpServerSecurity::Hybrid(_) => {
                nego::SecurityProtocol::HYBRID
                    | nego::SecurityProtocol::HYBRID_EX
                    | nego::SecurityProtocol::SSL
            }
        }
    }
}

struct AInputHandler {
    pending: mpsc::Sender<crate::MouseEvent>,
    worker: task::JoinHandle<()>,
    activity: watch::Sender<tokio::time::Instant>,
}

impl AInputHandler {
    fn new(
        handler: Arc<Mutex<Box<dyn RdpServerInputHandler>>>,
        activity: watch::Sender<tokio::time::Instant>,
    ) -> Self {
        // A single ordered worker replaces one blocking task per peer packet.
        // Overflow terminates the channel/session instead of retaining an
        // unbounded queue or silently dropping a button release.
        let (pending, mut receiver) = mpsc::channel(128);
        let worker = task::spawn(async move {
            while let Some(event) = receiver.recv().await {
                handler.lock().await.mouse(event);
            }
        });
        Self {
            pending,
            worker,
            activity,
        }
    }
}

impl Drop for AInputHandler {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl_as_any!(AInputHandler);

impl dvc::DvcProcessor for AInputHandler {
    fn channel_name(&self) -> &str {
        ironrdp_ainput::CHANNEL_NAME
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<dvc::DvcMessage>> {
        use ironrdp_ainput::{ServerPdu, VersionPdu};

        let pdu = ServerPdu::Version(VersionPdu::default());

        Ok(vec![Box::new(pdu)])
    }

    fn close(&mut self, _channel_id: u32) {}

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<dvc::DvcMessage>> {
        use ironrdp_ainput::ClientPdu;

        match decode(payload).map_err(|e| decode_err!(e))? {
            ClientPdu::Mouse(pdu) => {
                self.pending.try_send(pdu.into()).map_err(|_| {
                    decode_err!(std::io::Error::other(
                        "additional input queue is full or closed"
                    ))
                })?;
                self.activity.send_replace(tokio::time::Instant::now());
            }
        }

        Ok(Vec::new())
    }
}

impl dvc::DvcServerProcessor for AInputHandler {}

/// RDP Server
///
/// A server is created to listen for connections.
/// After the connection sequence is finalized using the provided security mechanism, the server can:
///  - receive display updates from a [`RdpServerDisplay`] and forward them to the client
///  - receive input events from a client and forward them to an [`RdpServerInputHandler`]
///
/// # Example
///
/// ```
/// use ironrdp_server::{RdpServer, RdpServerInputHandler, RdpServerDisplay, RdpServerDisplayUpdates};
///
///# use anyhow::Result;
///# use ironrdp_server::{DisplayUpdate, DesktopSize, KeyboardEvent, MouseEvent};
///# use tokio_rustls::TlsAcceptor;
///# struct NoopInputHandler;
///# impl RdpServerInputHandler for NoopInputHandler {
///#     fn keyboard(&mut self, _: KeyboardEvent) {}
///#     fn mouse(&mut self, _: MouseEvent) {}
///# }
///# struct NoopDisplay;
///# #[async_trait::async_trait]
///# impl RdpServerDisplay for NoopDisplay {
///#     async fn size(&mut self) -> DesktopSize {
///#         todo!()
///#     }
///#     async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
///#         todo!()
///#     }
///# }
///# async fn stub() -> Result<()> {
/// fn make_tls_acceptor() -> TlsAcceptor {
///    /* snip */
///#    todo!()
/// }
///
/// fn make_input_handler() -> impl RdpServerInputHandler {
///    /* snip */
///#    NoopInputHandler
/// }
///
/// fn make_display_handler() -> impl RdpServerDisplay {
///    /* snip */
///#    NoopDisplay
/// }
///
/// let tls_acceptor = make_tls_acceptor();
/// let input_handler = make_input_handler();
/// let display_handler = make_display_handler();
///
/// let mut server = RdpServer::builder()
///     .with_addr(([127, 0, 0, 1], 3389))
///     .with_tls(tls_acceptor)
///     .with_input_handler(input_handler)
///     .with_display_handler(display_handler)
///     .build();
///
/// server.run().await;
/// Ok(())
///# }
/// ```
pub struct RdpServer {
    opts: RdpServerOptions,
    // FIXME: replace with a channel and poll/process the handler?
    handler: Arc<Mutex<Box<dyn RdpServerInputHandler>>>,
    display: Arc<Mutex<Box<dyn RdpServerDisplay>>>,
    static_channels: StaticChannelSet,
    sound_factory: Option<Box<dyn SoundServerFactory>>,
    cliprdr_factory: Option<Box<dyn CliprdrServerFactory>>,
    ev_sender: ServerEventTx,
    ev_receiver: Arc<Mutex<ServerEventRx>>,
    creds: Option<Credentials>,
    local_addr: Option<SocketAddr>,
    gfx_state: Arc<std::sync::Mutex<GfxState>>,
    gfx_enabled: bool,
    handshake_timeout: Duration,
    idle_timeout: Option<Duration>,
    write_timeout: Duration,
    input_activity: watch::Sender<tokio::time::Instant>,
    quit_requested: bool,
}

#[derive(Debug)]
pub enum ServerEvent {
    Quit(String),
    Clipboard(ClipboardMessage),
    ClipboardFileContents(OwnedFileContentsResponse),
    ClipboardFileContentsRequest(FileContentsRequest),
    Rdpsnd(RdpsndServerMessage),
    SetCredentials(Credentials),
    GetLocalAddr(oneshot::Sender<Option<SocketAddr>>),
}

pub trait ServerEventSender {
    fn set_sender(&mut self, sender: ServerEventTx);
}

impl ServerEvent {
    pub fn create_channel() -> (ServerEventTx, ServerEventRx) {
        event_queue::channel()
    }
}

#[derive(Debug, PartialEq)]
enum RunState {
    Continue,
    Disconnect,
    DeactivationReactivation { desktop_size: DesktopSize },
}

fn is_benign_disconnect_error_text(err: &str) -> bool {
    err.contains("reset by peer")
        || err.contains("Broken pipe")
        // Guacamole teardown can leave the TCP stream ending mid-PDU read,
        // which currently bubbles up as a decode "not enough bytes" error
        // even though the client intentionally closed the session.
        || err.contains("not enough bytes")
}

impl RdpServer {
    pub fn new(
        opts: RdpServerOptions,
        handler: Box<dyn RdpServerInputHandler>,
        display: Box<dyn RdpServerDisplay>,
        mut sound_factory: Option<Box<dyn SoundServerFactory>>,
        mut cliprdr_factory: Option<Box<dyn CliprdrServerFactory>>,
    ) -> Self {
        let (ev_sender, ev_receiver) = ServerEvent::create_channel();
        ev_sender.end_session();
        if let Some(cliprdr) = cliprdr_factory.as_mut() {
            cliprdr.set_sender(ev_sender.clone());
        }
        if let Some(snd) = sound_factory.as_mut() {
            snd.set_sender(ev_sender.clone());
        }
        Self {
            opts,
            handler: Arc::new(Mutex::new(handler)),
            display: Arc::new(Mutex::new(display)),
            static_channels: StaticChannelSet::new(),
            sound_factory,
            cliprdr_factory,
            ev_sender,
            ev_receiver: Arc::new(Mutex::new(ev_receiver)),
            creds: None,
            local_addr: None,
            gfx_state: Arc::new(std::sync::Mutex::new(GfxState::new(0, 0, false, false))),
            gfx_enabled: true,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            idle_timeout: None,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
            input_activity: watch::channel(tokio::time::Instant::now()).0,
            quit_requested: false,
        }
    }

    /// Set the GFX state (call before run() to share state with display)
    pub fn set_gfx_state(&mut self, state: Arc<std::sync::Mutex<GfxState>>) {
        self.gfx_state = state;
    }

    /// Get a reference to the GFX state
    pub fn gfx_state(&self) -> &Arc<std::sync::Mutex<GfxState>> {
        &self.gfx_state
    }

    /// Enable or disable GFX H.264 channel
    pub fn set_gfx_enabled(&mut self, enabled: bool) {
        self.gfx_enabled = enabled;
    }

    pub fn builder() -> builder::RdpServerBuilder<builder::WantsAddr> {
        builder::RdpServerBuilder::new()
    }

    pub fn event_sender(&self) -> &ServerEventTx {
        &self.ev_sender
    }

    /// Bound each serialized socket write, including time waiting for another
    /// channel's write. Values are clamped to 1 ms..5 minutes; default 10 s.
    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout.clamp(Duration::from_millis(1), Duration::from_secs(300));
    }

    fn attach_channels(&mut self, acceptor: &mut Acceptor) {
        if let Some(cliprdr_factory) = self.cliprdr_factory.as_deref() {
            let backend = cliprdr_factory.build_cliprdr_backend();

            let cliprdr = CliprdrServer::new(backend);

            acceptor.attach_static_channel(cliprdr);
        }

        if let Some(factory) = self.sound_factory.as_deref() {
            let backend = factory.build_backend();

            acceptor.attach_static_channel(RdpsndServer::new(backend));
        }

        acceptor.attach_static_channel(self.dynamic_channels());
    }

    fn dynamic_channels(&self) -> DynamicChannels {
        // Display Control is independent of the graphics codec. Bitmap-only
        // Guacamole sessions still need this channel for browser resizing.
        let mut dvc = dvc::DrdynvcServer::new()
            .with_dynamic_channel(DisplayControlChannel::new(Arc::clone(&self.display)));
        if self.gfx_enabled {
            dvc = dvc.with_dynamic_channel(AInputHandler::new(
                Arc::clone(&self.handler),
                self.input_activity.clone(),
            ));
            let gfx_handler = GfxHandler::new(Arc::clone(&self.gfx_state));
            dvc = dvc.with_dynamic_channel(gfx_handler);
            debug!("GFX channel registered");
        }
        info!(
            gfx_enabled = self.gfx_enabled,
            "Registered Display Control channel for client viewport updates"
        );
        DynamicChannels(dvc)
    }

    pub async fn run_connection(&mut self, stream: TcpStream) -> Result<()> {
        let (_session_guard, sender) = self.ev_sender.start_session();
        self.ev_receiver.lock().await.data.discard_pending();
        if let Some(factory) = self.cliprdr_factory.as_mut() {
            factory.set_sender(sender.clone());
        }
        if let Some(factory) = self.sound_factory.as_mut() {
            factory.set_sender(sender);
        }
        self.input_activity
            .send_replace(tokio::time::Instant::now());
        let peer_ip = stream.peer_addr().map(|addr| addr.ip()).ok();
        let framed = TokioFramed::new(stream);

        // Reset GFX state for new connection, preserving peer IP and hot-updated resolution
        let pending_resolution = {
            let mut gs = self.gfx_state.lock().unwrap();
            let w = gs.width;
            let h = gs.height;
            let avc444_enabled = gs.avc444_enabled;
            let res = gs.resolution.take();
            let gfx_validation_mode = gs.gfx_validation_mode;
            *gs = GfxState::new(w, h, avc444_enabled, gfx_validation_mode);
            gs.peer_addr = peer_ip;
            res
        };

        // Apply hot-updated resolution for this new connection
        if let Some(ref res) = pending_resolution {
            if let Some((w, h)) = res
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?)))
            {
                info!(w, h, "Applying hot-updated resolution for new connection");
                // Use set_size (bypasses fixed_resolution check, since this is server-initiated)
                self.display.lock().await.set_size(w, h);
                let mut gs = self.gfx_state.lock().unwrap();
                gs.width = w;
                gs.height = h;
            }
        }

        let size = self.display.lock().await.size().await;
        let capabilities = capabilities::capabilities(&self.opts, size);
        let mut acceptor = Acceptor::new(
            self.opts.security.flag(),
            size,
            capabilities,
            self.creds.clone(),
        );

        self.attach_channels(&mut acceptor);

        let res = self
            .await_connection_stage("RDP negotiation", async {
                ironrdp_acceptor::accept_begin(framed, &mut acceptor)
                    .await
                    .map_err(Into::into)
            })
            .await?;

        match res {
            BeginResult::ShouldUpgrade(stream) => {
                let tls_acceptor = match &self.opts.security {
                    RdpServerSecurity::Tls(acceptor) => acceptor.clone(),
                    RdpServerSecurity::Hybrid((acceptor, _)) => acceptor.clone(),
                    RdpServerSecurity::None => unreachable!(),
                };
                let accept = self
                    .await_connection_stage("TLS handshake", async {
                        tls_acceptor.accept(stream).await.map_err(Into::into)
                    })
                    .await?;
                let mut framed = TokioFramed::new(accept);

                acceptor.mark_security_upgrade_as_done();

                if let RdpServerSecurity::Hybrid((_, pub_key)) = self.opts.security.clone() {
                    // how to get the client name?
                    // doesn't seem to matter yet
                    let client_name = framed.get_inner().0.get_ref().0.peer_addr()?.to_string();

                    self.await_connection_stage("CredSSP authentication", async {
                        ironrdp_acceptor::accept_credssp(
                            &mut framed,
                            &mut acceptor,
                            &mut ironrdp_tokio::reqwest::ReqwestNetworkClient::new(),
                            client_name.into(),
                            pub_key,
                            None,
                        )
                        .await
                        .map_err(Into::into)
                    })
                    .await?;
                }

                let framed = self.accept_finalize(framed, acceptor, size).await?;
                debug!("Shutting down TLS connection");
                let (mut tls_stream, _) = framed.into_inner();
                match tokio::time::timeout(Duration::from_secs(1), tls_stream.shutdown()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => debug!(?error, "TLS shutdown error"),
                    Err(_) => debug!("TLS shutdown timed out"),
                }
            }

            BeginResult::Continue(framed) => {
                self.accept_finalize(framed, acceptor, size).await?;
            }
        };

        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        let listener = TcpListener::bind(self.opts.addr).await?;
        self.run_with_listener(listener).await
    }

    /// Serve a listener already bound by the application's startup path.
    /// Keeping ownership of the socket avoids a release/rebind readiness race.
    pub async fn run_with_listener(&mut self, listener: TcpListener) -> Result<()> {
        let local_addr = listener.local_addr()?;

        debug!("Listening for connections on {local_addr}");
        self.local_addr = Some(local_addr);

        while !self.quit_requested {
            let ev_receiver = Arc::clone(&self.ev_receiver);
            let mut ev_receiver = ev_receiver.lock().await;
            tokio::select! {
                biased;
                Some(event) = ev_receiver.recv() => {
                    match event {
                        ServerEvent::Quit(reason) => {
                            debug!("Got quit event {reason}");
                            break;
                        }
                        ServerEvent::GetLocalAddr(tx) => {
                            let _ = tx.send(self.local_addr);
                        }
                        ServerEvent::SetCredentials(creds) => {
                            self.set_credentials(Some(creds));
                        }
                        ev => {
                            debug!("Unexpected event {:?}", ev);
                        }
                    }
                },
                accepted = listener.accept() => {
                    let (stream, peer) = accepted.context("Failed to accept RDP connection")?;
                    debug!(?peer, "Received connection");
                    drop(ev_receiver);
                    if let Err(error) = self.run_connection(stream).await {
                        let err_str = format!("{error:#}");
                        if is_benign_disconnect_error_text(&err_str) {
                            warn!("Client disconnected: {err_str}");
                        } else {
                            error!(?error, "Connection error");
                        }
                    }
                    info!("Ready for next connection");
                    self.static_channels = StaticChannelSet::new();
                }
                else => break,
            }
        }

        Ok(())
    }

    pub fn get_svc_processor<T: SvcProcessor + 'static>(&mut self) -> Option<&mut T> {
        self.static_channels
            .get_by_type_mut::<T>()
            .and_then(|svc| svc.channel_processor_downcast_mut())
    }

    pub fn get_channel_id_by_type<T: SvcProcessor + 'static>(&self) -> Option<StaticChannelId> {
        self.static_channels.get_channel_id_by_type::<T>()
    }

    async fn dispatch_pdu(
        &mut self,
        action: Action,
        bytes: bytes::BytesMut,
        writer: &mut impl FramedWrite,
        io_channel_id: u16,
        user_channel_id: u16,
    ) -> Result<RunState> {
        match action {
            Action::FastPath => {
                let input = decode(&bytes)?;
                self.handle_fastpath(input).await;
            }

            Action::X224 => {
                if self
                    .handle_x224(writer, io_channel_id, user_channel_id, &bytes)
                    .await
                    .context("X224 input error")?
                {
                    debug!("Got disconnect request");
                    return Ok(RunState::Disconnect);
                }
            }
        }

        Ok(RunState::Continue)
    }

    async fn dispatch_display_update(
        update: DisplayUpdate,
        writer: &mut impl FramedWrite,
        user_channel_id: u16,
        io_channel_id: u16,
        buffer: &mut Vec<u8>,
        mut encoder: UpdateEncoder,
        gfx_state: &Arc<std::sync::Mutex<GfxState>>,
        drdynvc_channel_id: Option<StaticChannelId>,
    ) -> Result<(RunState, UpdateEncoder)> {
        let is_bitmap_update = matches!(&update, DisplayUpdate::Bitmap(_));

        if let DisplayUpdate::Resize(desktop_size) = update {
            debug!(?desktop_size, "Display resize");
            encoder.set_desktop_size(desktop_size);
            deactivate_all(io_channel_id, user_channel_id, writer).await?;
            return Ok((RunState::DeactivationReactivation { desktop_size }, encoder));
        }

        // Handle GFX frames through the DVC channel
        if let DisplayUpdate::GfxFrame(ref gfx_frame) = update {
            let mut state = gfx_state.lock().unwrap();
            if state.is_ready() {
                if let Some(drdynvc_id) = drdynvc_channel_id {
                    let channel_id = state.channel_id.unwrap();
                    // All frame PDUs combined into a single ZGFX-wrapped buffer
                    let pdu_data = GfxHandler::create_frame_pdu(&mut state, gfx_frame);
                    let frame_id = state.frame_id;
                    drop(state); // release lock before async

                    // Send as a single DVC message (auto-fragmented by encode_dvc_messages)
                    let dvc_messages: Vec<dvc::DvcMessage> =
                        vec![Box::new(crate::gfx::RawGfxPdu(pdu_data))];
                    let svc_messages = dvc::encode_dvc_messages(
                        channel_id,
                        dvc_messages,
                        ChannelFlags::SHOW_PROTOCOL,
                    )
                    .context("Failed to encode DVC messages")?;

                    let data = server_encode_svc_messages(
                        svc_messages.into(),
                        drdynvc_id,
                        user_channel_id,
                    )?;
                    let write_started = std::time::Instant::now();
                    writer
                        .write_all(&data)
                        .await
                        .context("failed to write GFX frame")?;
                    let mut state = gfx_state.lock().unwrap();
                    state.frame_written(frame_id);
                    state.record_display_write(
                        data.len(),
                        write_started.elapsed().as_secs_f64() * 1000.0,
                    );

                    return Ok((RunState::Continue, encoder));
                }
            } else {
                drop(state);
                // GFX not ready yet, fall through to bitmap encoding
                trace!("GFX not ready, falling back to bitmap path");
            }
        }

        // Handle GFX uncompressed dirty rect updates through the DVC channel
        if let DisplayUpdate::GfxUncompressed(ref uncompressed) = update {
            let mut state = gfx_state.lock().unwrap();
            if state.is_ready() {
                if let Some(drdynvc_id) = drdynvc_channel_id {
                    let channel_id = state.channel_id.unwrap();
                    let pdu_data = GfxHandler::create_uncompressed_pdu(&mut state, uncompressed);
                    let frame_id = state.frame_id;
                    drop(state);

                    let dvc_messages: Vec<dvc::DvcMessage> =
                        vec![Box::new(crate::gfx::RawGfxPdu(pdu_data))];
                    let svc_messages = dvc::encode_dvc_messages(
                        channel_id,
                        dvc_messages,
                        ChannelFlags::SHOW_PROTOCOL,
                    )
                    .context("Failed to encode DVC messages")?;

                    let data = server_encode_svc_messages(
                        svc_messages.into(),
                        drdynvc_id,
                        user_channel_id,
                    )?;
                    let write_started = std::time::Instant::now();
                    writer
                        .write_all(&data)
                        .await
                        .context("failed to write GFX uncompressed frame")?;
                    let mut state = gfx_state.lock().unwrap();
                    state.frame_written(frame_id);
                    state.record_display_write(
                        data.len(),
                        write_started.elapsed().as_secs_f64() * 1000.0,
                    );

                    return Ok((RunState::Continue, encoder));
                }
            } else {
                drop(state);
                trace!("GFX not ready for uncompressed, falling back to bitmap path");
            }
        }

        let mut encoder_iter = encoder.update(update);
        let mut bytes_written = 0usize;
        let mut write_ms = 0.0;
        loop {
            let Some(fragmenter) = encoder_iter.next().await else {
                break;
            };

            let mut fragmenter = fragmenter.context("error while encoding")?;
            if fragmenter.size_hint() > buffer.len() {
                buffer.resize(fragmenter.size_hint(), 0);
            }

            while let Some(len) = fragmenter.next(buffer) {
                let write_started = std::time::Instant::now();
                writer
                    .write_all(&buffer[..len])
                    .await
                    .context("failed to write display update")?;
                bytes_written = bytes_written.saturating_add(len);
                write_ms += write_started.elapsed().as_secs_f64() * 1000.0;
            }
        }

        if is_bitmap_update {
            let mut state = gfx_state.lock().unwrap();
            state.bitmap_wire_bytes = state.bitmap_wire_bytes.saturating_add(bytes_written as u64);
            if bytes_written > 0 {
                state.record_display_write(bytes_written, write_ms);
            }
        }

        Ok((RunState::Continue, encoder))
    }

    async fn dispatch_server_events(
        &mut self,
        events: &mut Vec<ServerEvent>,
        writer: &mut impl FramedWrite,
        user_channel_id: u16,
    ) -> Result<RunState> {
        for event in events.drain(..) {
            match event {
                ServerEvent::Quit(reason) => {
                    debug!("Got quit event: {reason}");
                    self.quit_requested = true;
                    return Ok(RunState::Disconnect);
                }
                ServerEvent::GetLocalAddr(tx) => {
                    let _ = tx.send(self.local_addr);
                }
                ServerEvent::SetCredentials(creds) => {
                    self.set_credentials(Some(creds));
                }
                ServerEvent::Rdpsnd(s) => {
                    let Some(rdpsnd) = self.get_svc_processor::<RdpsndServer>() else {
                        warn!("No rdpsnd channel, dropping event");
                        continue;
                    };
                    let msgs = match s {
                        RdpsndServerMessage::Wave(data, ts) => {
                            match rdpsnd.wave(data, ts) {
                                Ok(msgs) => msgs,
                                Err(e) => {
                                    // Wave before handshake complete — drop silently
                                    debug!("Dropping wave: {e}");
                                    continue;
                                }
                            }
                        }
                        RdpsndServerMessage::SetVolume { left, right } => rdpsnd
                            .set_volume(left, right)
                            .context("failed to send rdpsnd event")?,
                        RdpsndServerMessage::Close => {
                            rdpsnd.close().context("failed to send rdpsnd event")?
                        }
                        RdpsndServerMessage::Error(error) => {
                            error!(?error, "Handling rdpsnd event");
                            continue;
                        }
                    };
                    let channel_id = self
                        .get_channel_id_by_type::<RdpsndServer>()
                        .ok_or_else(|| anyhow!("SVC channel not found"))?;
                    let data =
                        server_encode_svc_messages(msgs.into(), channel_id, user_channel_id)?;
                    writer.write_all(&data).await?;
                }
                ServerEvent::Clipboard(c) => {
                    let Some(cliprdr) = self.get_svc_processor::<CliprdrServer>() else {
                        warn!("No clipboard channel, dropping event");
                        continue;
                    };
                    let msgs = match c {
                        ClipboardMessage::SendInitiateCopy(formats) => {
                            cliprdr.initiate_copy(&formats)
                        }
                        ClipboardMessage::SendFormatData(data) => cliprdr.submit_format_data(data),
                        ClipboardMessage::SendInitiatePaste(format) => {
                            cliprdr.initiate_paste(format)
                        }
                        ClipboardMessage::Error(error) => {
                            error!(?error, "Handling clipboard event");
                            continue;
                        }
                    }
                    .context("failed to send clipboard event")?;
                    let channel_id = self
                        .get_channel_id_by_type::<CliprdrServer>()
                        .ok_or_else(|| anyhow!("SVC channel not found"))?;
                    let data =
                        server_encode_svc_messages(msgs.into(), channel_id, user_channel_id)?;
                    writer.write_all(&data).await?;
                }
                ServerEvent::ClipboardFileContents(response) => {
                    let Some(cliprdr) = self.get_svc_processor::<CliprdrServer>() else {
                        warn!("No clipboard channel, dropping file contents response");
                        continue;
                    };
                    let msgs = cliprdr
                        .submit_file_contents(response)
                        .context("failed to submit file contents")?;
                    let channel_id = self
                        .get_channel_id_by_type::<CliprdrServer>()
                        .ok_or_else(|| anyhow!("SVC channel not found"))?;
                    let data =
                        server_encode_svc_messages(msgs.into(), channel_id, user_channel_id)?;
                    writer.write_all(&data).await?;
                }
                ServerEvent::ClipboardFileContentsRequest(request) => {
                    let channel_id = self
                        .get_channel_id_by_type::<CliprdrServer>()
                        .ok_or_else(|| anyhow!("SVC channel not found"))?;
                    let pdu = ClipboardPdu::FileContentsRequest(request);
                    let svc_msg = encode_cliprdr_pdu(pdu);
                    let data = server_encode_svc_messages(
                        vec![svc_msg].into(),
                        channel_id,
                        user_channel_id,
                    )?;
                    writer.write_all(&data).await?;
                }
            }
        }

        Ok(RunState::Continue)
    }

    async fn client_loop<R, W>(
        &mut self,
        reader: &mut Framed<R>,
        writer: &mut Framed<W>,
        io_channel_id: u16,
        user_channel_id: u16,
        mut encoder: UpdateEncoder,
        desktop_size: DesktopSize,
        input_events: Vec<Vec<u8>>,
    ) -> Result<RunState>
    where
        R: FramedRead,
        W: FramedWrite,
    {
        debug!("Starting client loop");
        let mut display_updates = {
            let mut display = self.display.lock().await;
            display.activate_size(desktop_size);
            display.updates().await?
        };
        let mut writer = SharedWriter::new(writer, self.write_timeout);
        let mut display_writer = writer.clone();
        let mut event_writer = writer.clone();
        let ev_receiver = Arc::clone(&self.ev_receiver);
        let gfx_state = Arc::clone(&self.gfx_state);
        let idle_timeout = self.idle_timeout;
        let activity = self.input_activity.subscribe();
        let event_sender = self.ev_sender.clone();
        let local_addr = self.local_addr;

        // Update GFX state with current desktop size
        {
            let mut gs = self.gfx_state.lock().unwrap();
            gs.width = desktop_size.width;
            gs.height = desktop_size.height;
        }

        // Get DRDYNVC channel ID for sending GFX frames
        let drdynvc_channel_id = self.get_channel_id_by_type::<DynamicChannels>();

        let s = Rc::new(Mutex::new(self));
        let quit_received = Rc::new(std::cell::Cell::new(false));
        let changed_credentials = Rc::new(std::cell::RefCell::new(None));

        let this = Rc::clone(&s);
        let dispatch_pdu = async move {
            // Capture creation commits the input transform. Replay activation
            // input afterward, inside the cancellable session driver.
            if !input_events.is_empty() {
                this.lock()
                    .await
                    .handle_input_backlog(&mut writer, io_channel_id, user_channel_id, input_events)
                    .await?;
            }
            loop {
                let (action, bytes) = reader.read_pdu().await?;
                let mut this = this.lock().await;
                match this
                    .dispatch_pdu(action, bytes, &mut writer, io_channel_id, user_channel_id)
                    .await?
                {
                    RunState::Continue => continue,
                    state => break Ok(state),
                }
            }
        };

        let dispatch_display = async move {
            let mut buffer = vec![0u8; 4096];

            loop {
                match display_updates.next_update().await {
                    Ok(Some(update)) => {
                        match Self::dispatch_display_update(
                            update,
                            &mut display_writer,
                            user_channel_id,
                            io_channel_id,
                            &mut buffer,
                            encoder,
                            &gfx_state,
                            drdynvc_channel_id,
                        )
                        .await?
                        {
                            (RunState::Continue, enc) => {
                                encoder = enc;
                                continue;
                            }
                            (state, _) => {
                                break Ok(state);
                            }
                        }
                    }
                    Ok(None) => {
                        break Ok(RunState::Disconnect);
                    }
                    Err(error) => {
                        warn!(error = format!("{error:#}"), "next_updated failed");
                    }
                }
            }
        };

        let this = Rc::clone(&s);
        let event_quit_received = Rc::clone(&quit_received);
        let event_credentials = Rc::clone(&changed_credentials);
        let mut ev_receiver = ev_receiver.lock().await;
        let ServerEventRx { controls, data } = &mut *ev_receiver;
        let dispatch_controls = async move {
            loop {
                match controls.recv().await {
                    Some(ServerEvent::Quit(_)) | None => {
                        event_quit_received.set(true);
                        break Ok(RunState::Disconnect);
                    }
                    Some(ServerEvent::GetLocalAddr(reply)) => {
                        let _ = reply.send(local_addr);
                    }
                    Some(ServerEvent::SetCredentials(credentials)) => {
                        // Apply after the other futures drop their server lock.
                        // Revoked credentials must not retain an active session.
                        *event_credentials.borrow_mut() = Some(credentials);
                        break Ok(RunState::Disconnect);
                    }
                    Some(_) => unreachable!("only server controls use this lane"),
                }
            }
        };
        let dispatch_events = async move {
            let mut events = Vec::with_capacity(1);
            loop {
                let Some(event) = data.recv().await else {
                    break Ok(RunState::Disconnect);
                };
                events.push(event);
                let mut this = this.lock().await;
                match this
                    .dispatch_server_events(&mut events, &mut event_writer, user_channel_id)
                    .await?
                {
                    RunState::Continue => continue,
                    state => break Ok(state),
                }
            }
        };

        let (source, state) = tokio::select!(
            biased;
            state = dispatch_controls => ("control", state),
            reason = event_sender.failure() => ("output-queue", Err(anyhow!(reason))),
            _ = wait_for_idle_timeout(activity, idle_timeout) => {
                info!(?idle_timeout, "Disconnecting inactive session");
                ("idle-timeout", Ok(RunState::Disconnect))
            },
            state = dispatch_pdu => ("pdu", state),
            state = dispatch_display => ("display", state),
            state = dispatch_events => ("events", state),
        );

        if quit_received.get() {
            s.lock().await.quit_requested = true;
        }
        if let Some(credentials) = changed_credentials.borrow_mut().take() {
            s.lock().await.set_credentials(Some(credentials));
        }

        match &state {
            Ok(run_state) => {
                info!(source, ?run_state, "Client loop ended");
            }
            Err(error) => {
                let err_str = format!("{error:#}");
                if is_benign_disconnect_error_text(&err_str) {
                    info!(
                        source,
                        error = err_str,
                        "Client loop ended after client disconnect"
                    );
                } else {
                    warn!(source, error = err_str, "Client loop failed");
                }
            }
        }

        state
    }

    async fn client_accepted<R, W>(
        &mut self,
        reader: &mut Framed<R>,
        writer: &mut Framed<W>,
        result: AcceptorResult,
        activation_size: DesktopSize,
    ) -> Result<RunState>
    where
        R: FramedRead,
        W: FramedWrite,
    {
        debug!("Client accepted");

        if !result.reactivation {
            // Authentication time is not part of a connected user's idle
            // allowance. Reactivation retains the existing session deadline.
            self.input_activity
                .send_replace(tokio::time::Instant::now());
        }

        {
            let mut handler = self.handler.lock().await;
            let context = ClientInputContext {
                client_name: result.client_name.clone(),
                keyboard_layout: result.keyboard_layout,
            };
            handler.update_client_context(context.clone());
            // Reactivation is a resize within the same connection. Preserve
            // session state and the original connected-at timestamp across it.
            if !result.reactivation {
                handler.on_connect(
                    context,
                    self.gfx_state
                        .lock()
                        .unwrap()
                        .peer_addr
                        .map(|addr| addr.to_string()),
                );
            }
        }

        self.static_channels = result.static_channels;
        {
            let mut gfx_state = self.gfx_state.lock().unwrap();
            let session_uses_gfx =
                self.gfx_enabled && self.get_channel_id_by_type::<DynamicChannels>().is_some();
            if !result.reactivation {
                gfx_state.reset_connection_state(session_uses_gfx);
            } else {
                // Dynamic channels and their negotiated codecs survive RDP
                // reactivation. Recreate graphics surfaces at the new size
                // without waiting for a second channel-capability exchange.
                gfx_state.invalidate_surface();
                gfx_state.pending_acks = 0;
                gfx_state.frame_send_times.clear();
            }
            info!(
                session_uses_gfx = gfx_state.session_uses_gfx,
                "Connection graphics path negotiated"
            );
        }
        if !result.reactivation {
            let mut responses = Vec::new();
            for (_type_id, channel, channel_id) in self.static_channels.iter_mut() {
                debug!(?channel, ?channel_id, "Start");
                let Some(channel_id) = channel_id else {
                    continue;
                };
                let svc_responses = channel.start()?;
                let response =
                    server_encode_svc_messages(svc_responses, channel_id, result.user_channel_id)?;
                responses.push(response);
            }
            let write_timeout = self.write_timeout;
            self.await_connection_stage("channel startup", async {
                let mut writer = SharedWriter::new(writer, write_timeout);
                for response in responses {
                    writer.write_all(&response).await?;
                }
                Ok(())
            })
            .await?;
        }

        if !result.reactivation {
            let confirmed_size =
                result
                    .capabilities
                    .iter()
                    .find_map(|capability| match capability {
                        CapabilitySet::Bitmap(bitmap) => Some(DesktopSize {
                            width: bitmap.desktop_width,
                            height: bitmap.desktop_height,
                        }),
                        _ => None,
                    });
            // FreeRDP applies Demand Active before sending Confirm Active, so
            // its bitmap capability often echoes our old size. Preserve the
            // actual browser/client request from the initial GCC exchange.
            let requested_size = result.client_desktop_size.or(confirmed_size);
            info!(
                ?requested_size,
                ?confirmed_size,
                ?activation_size,
                "Initial client desktop dimensions"
            );
            if let Some(requested_size) = requested_size {
                // The display owns fixed-resolution policy and normalization;
                // its resize watch negotiates the resulting size before the
                // next capture or pointer transform adopts it.
                self.display
                    .lock()
                    .await
                    .request_resize(requested_size.width, requested_size.height);
            }
        }

        let mut update_codecs = UpdateEncoderCodecs::new();
        let mut surface_flags = CmdFlags::empty();
        for c in result.capabilities {
            match c {
                CapabilitySet::General(c) => {
                    let fastpath = c
                        .extra_flags
                        .contains(GeneralExtraFlags::FASTPATH_OUTPUT_SUPPORTED);
                    if !fastpath {
                        bail!("Fastpath output not supported!");
                    }
                }
                CapabilitySet::SurfaceCommands(c) => {
                    surface_flags = c.flags;
                }
                CapabilitySet::BitmapCodecs(BitmapCodecs(codecs)) => {
                    for codec in codecs {
                        match codec.property {
                            // FIXME: The encoder operates in image mode only.
                            //
                            // See [MS-RDPRFX] 3.1.1.1 "State Machine" for
                            // implementation of the video mode. which allows to
                            // skip sending Header for each image.
                            //
                            // We should distinguish parameters for both modes,
                            // and somehow choose the "best", instead of picking
                            // the last parsed here.
                            CodecProperty::RemoteFx(
                                rdp::capability_sets::RemoteFxContainer::ClientContainer(c),
                            ) if self.opts.has_remote_fx() => {
                                for caps in c.caps_data.0 .0 {
                                    update_codecs.set_remotefx(Some((caps.entropy_bits, codec.id)));
                                }
                            }
                            CodecProperty::ImageRemoteFx(
                                rdp::capability_sets::RemoteFxContainer::ClientContainer(c),
                            ) if self.opts.has_image_remote_fx() => {
                                for caps in c.caps_data.0 .0 {
                                    update_codecs.set_remotefx(Some((caps.entropy_bits, codec.id)));
                                }
                            }
                            CodecProperty::NsCodec(_) => (),
                            #[cfg(feature = "qoi")]
                            CodecProperty::Qoi if self.opts.has_qoi() => {
                                update_codecs.set_qoi(Some(codec.id));
                            }
                            #[cfg(feature = "qoiz")]
                            CodecProperty::QoiZ if self.opts.has_qoiz() => {
                                update_codecs.set_qoiz(Some(codec.id));
                            }
                            _ => (),
                        }
                    }
                }
                _ => {}
            }
        }

        // Demand Active defines the current coordinate space. Confirm Active
        // can request another size or echo the previous one; keep the advertised
        // size through capture creation and negotiate pending changes separately.
        let desktop_size = activation_size;
        let encoder = UpdateEncoder::new(desktop_size, surface_flags, update_codecs)
            .context("failed to initialize update encoder")?;

        let state = self
            .client_loop(
                reader,
                writer,
                result.io_channel_id,
                result.user_channel_id,
                encoder,
                desktop_size,
                result.input_events,
            )
            .await
            .context("client loop failure")?;

        Ok(state)
    }

    async fn handle_input_backlog(
        &mut self,
        writer: &mut impl FramedWrite,
        io_channel_id: u16,
        user_channel_id: u16,
        frames: Vec<Vec<u8>>,
    ) -> Result<()> {
        for frame in frames {
            match Action::from_fp_output_header(frame[0]) {
                Ok(Action::FastPath) => {
                    let input = decode(&frame)?;
                    self.handle_fastpath(input).await;
                }

                Ok(Action::X224) => {
                    let _ = self
                        .handle_x224(writer, io_channel_id, user_channel_id, &frame)
                        .await?;
                }

                // the frame here is always valid, because otherwise it would
                // have failed during the acceptor loop
                Err(_) => unreachable!(),
            }
        }

        Ok(())
    }

    async fn handle_fastpath(&mut self, input: FastPathInput) {
        if input
            .input_events()
            .iter()
            .any(|event| !matches!(event, FastPathInputEvent::QoeEvent(_)))
        {
            self.input_activity
                .send_replace(tokio::time::Instant::now());
        }
        for event in input.input_events().iter().copied() {
            let mut handler = self.handler.lock().await;
            match event {
                FastPathInputEvent::KeyboardEvent(flags, key) => {
                    handler.keyboard((key, flags).into());
                }

                FastPathInputEvent::UnicodeKeyboardEvent(flags, key) => {
                    handler.keyboard((key, flags).into());
                }

                FastPathInputEvent::SyncEvent(flags) => {
                    handler.keyboard(flags.into());
                }

                FastPathInputEvent::MouseEvent(mouse) => {
                    handler.mouse_pdu(mouse);
                }

                FastPathInputEvent::MouseEventEx(mouse) => {
                    handler.mouse_x_pdu(mouse);
                }

                FastPathInputEvent::MouseEventRel(mouse) => {
                    handler.mouse(mouse.into());
                }

                FastPathInputEvent::QoeEvent(quality) => {
                    warn!("Received QoE: {}", quality);
                }
            }
        }
    }

    async fn handle_io_channel_data(&mut self, data: SendDataRequest<'_>) -> Result<bool> {
        let control: rdp::headers::ShareControlHeader = decode(data.user_data.as_ref())?;

        match control.share_control_pdu {
            ShareControlPdu::Data(header) => match header.share_data_pdu {
                rdp::headers::ShareDataPdu::Input(pdu) => {
                    self.handle_input_event(pdu).await;
                }

                rdp::headers::ShareDataPdu::ShutdownRequest => {
                    return Ok(true);
                }

                unexpected => {
                    warn!(?unexpected, "Unexpected share data pdu");
                }
            },

            unexpected => {
                warn!(?unexpected, "Unexpected share control");
            }
        }

        Ok(false)
    }

    async fn handle_x224(
        &mut self,
        writer: &mut impl FramedWrite,
        io_channel_id: u16,
        user_channel_id: u16,
        frame: &[u8],
    ) -> Result<bool> {
        let message = decode::<X224<mcs::McsMessage<'_>>>(frame)?;
        match message.0 {
            mcs::McsMessage::SendDataRequest(data) => {
                debug!(?data, "McsMessage::SendDataRequest");
                if data.channel_id == io_channel_id {
                    return self.handle_io_channel_data(data).await;
                }

                if let Some(svc) = self.static_channels.get_by_channel_id_mut(data.channel_id) {
                    let response_pdus = svc.process(&data.user_data)?;
                    let response = server_encode_svc_messages(
                        response_pdus,
                        data.channel_id,
                        user_channel_id,
                    )?;
                    writer.write_all(&response).await?;
                } else {
                    warn!(
                        channel_id = data.channel_id,
                        "Unexpected channel received: ID",
                    );
                }
            }

            mcs::McsMessage::DisconnectProviderUltimatum(disconnect) => {
                if disconnect.reason == mcs::DisconnectReason::UserRequested {
                    return Ok(true);
                }
            }

            _ => {
                warn!(
                    name = ironrdp_core::name(&message),
                    "Unexpected mcs message"
                );
            }
        }

        Ok(false)
    }

    async fn handle_input_event(&mut self, input: InputEventPdu) {
        if input
            .0
            .iter()
            .any(|event| !matches!(event, ironrdp_pdu::input::InputEvent::Unused(_)))
        {
            self.input_activity
                .send_replace(tokio::time::Instant::now());
        }
        for event in input.0 {
            let mut handler = self.handler.lock().await;
            match event {
                ironrdp_pdu::input::InputEvent::ScanCode(key) => {
                    handler.keyboard((key.key_code, key.flags).into());
                }

                ironrdp_pdu::input::InputEvent::Unicode(key) => {
                    handler.keyboard((key.unicode_code, key.flags).into());
                }

                ironrdp_pdu::input::InputEvent::Sync(sync) => {
                    handler.keyboard(sync.flags.into());
                }

                ironrdp_pdu::input::InputEvent::Mouse(mouse) => {
                    handler.mouse_pdu(mouse);
                }

                ironrdp_pdu::input::InputEvent::MouseX(mouse) => {
                    handler.mouse_x_pdu(mouse);
                }

                ironrdp_pdu::input::InputEvent::MouseRel(mouse) => {
                    handler.mouse(mouse.into());
                }

                ironrdp_pdu::input::InputEvent::Unused(_) => {}
            }
        }
    }

    async fn accept_finalize<S>(
        &mut self,
        mut framed: TokioFramed<S>,
        mut acceptor: Acceptor,
        mut activation_size: DesktopSize,
    ) -> Result<TokioFramed<S>>
    where
        S: AsyncRead + AsyncWrite + Sync + Send + Unpin,
    {
        let mut client_connected = false;
        let result = loop {
            let (new_framed, result) = match self
                .await_connection_stage("RDP activation", async {
                    ironrdp_acceptor::accept_finalize(framed, &mut acceptor)
                        .await
                        .map_err(Into::into)
                })
                .await
            {
                Ok(value) => value,
                Err(error) => break Err(error),
            };

            let (mut reader, mut writer) = split_tokio_framed(new_framed);
            client_connected = true;

            let run_state = match self
                .client_accepted(&mut reader, &mut writer, result, activation_size)
                .await
            {
                Ok(run_state) => run_state,
                Err(error) => break Err(error),
            };

            match run_state {
                RunState::Continue => {
                    unreachable!();
                }
                RunState::DeactivationReactivation { desktop_size } => {
                    activation_size = desktop_size;
                    // No description of such behavior was found in the
                    // specification, but apparently, we must keep the channel
                    // state as they were during reactivation. This fixes
                    // various state issues during client resize.
                    acceptor = match Acceptor::new_deactivation_reactivation(
                        acceptor,
                        core::mem::take(&mut self.static_channels),
                        desktop_size,
                    ) {
                        Ok(acceptor) => acceptor,
                        // Route reactivation failures through the same real
                        // disconnect cleanup as capture and transport errors.
                        Err(error) => break Err(error.into()),
                    };
                    framed = unsplit_tokio_framed(reader, writer);
                    continue;
                }
                RunState::Disconnect => {
                    let final_framed = unsplit_tokio_framed(reader, writer);
                    break Ok(final_framed);
                }
            }
        };

        // Dispose per-connection channel workers before another client can
        // negotiate against this display. Reactivation stays inside the loop
        // above and preserves those channels until the connection actually ends.
        self.static_channels = StaticChannelSet::new();
        if client_connected {
            self.handler.lock().await.on_disconnect();
        }

        result
    }

    pub fn set_credentials(&mut self, creds: Option<Credentials>) {
        debug!(configured = creds.is_some(), "Changing credentials");
        self.creds = creds
    }
}

async fn deactivate_all(
    io_channel_id: u16,
    user_channel_id: u16,
    writer: &mut impl FramedWrite,
) -> Result<(), anyhow::Error> {
    let pdu = ShareControlPdu::ServerDeactivateAll(ServerDeactivateAll);
    let pdu = rdp::headers::ShareControlHeader {
        share_id: 0,
        pdu_source: io_channel_id,
        share_control_pdu: pdu,
    };
    let user_data = encode_vec(&pdu)?.into();
    let pdu = SendDataIndication {
        initiator_id: user_channel_id,
        channel_id: io_channel_id,
        user_data,
    };
    let msg = encode_vec(&X224(pdu))?;
    writer.write_all(&msg).await?;
    Ok(())
}

fn encode_cliprdr_pdu(pdu: ClipboardPdu<'static>) -> SvcMessage {
    SvcMessage::from(pdu).with_flags(ChannelFlags::SHOW_PROTOCOL)
}
