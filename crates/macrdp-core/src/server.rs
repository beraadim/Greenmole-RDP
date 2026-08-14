//! RDP server lifecycle: start, stop, metrics, connections

/// Parse "WxH" resolution string, e.g. "3840x2160" → Some((3840, 2160))
fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn auto_resolution(logical_w: u16, logical_h: u16) -> (u16, u16) {
    match macrdp_capture::detect_display_pixel_size() {
        Ok((physical_w, physical_h)) => {
            tracing::info!(
                logical_w,
                logical_h,
                physical_w,
                physical_h,
                "Resolution auto: using physical display size"
            );
            (physical_w as u16, physical_h as u16)
        }
        Err(err) => {
            tracing::warn!(%err, logical_w, logical_h, "Resolution auto: failed to detect physical display size, using logical");
            (logical_w, logical_h)
        }
    }
}

/// Resolve resolution config value to (width, height).
///
/// Supports three formats:
/// - "auto": detect display scale and multiply logical resolution
/// - "WxH" (e.g. "3840x2160"): use directly
/// - legacy numeric scale (e.g. "2"): multiply logical resolution by scale factor
pub fn resolve_resolution(res_mode: &str, logical_w: u16, logical_h: u16) -> (u16, u16) {
    if res_mode == "auto" {
        auto_resolution(logical_w, logical_h)
    } else if let Some((w, h)) = parse_resolution(res_mode) {
        tracing::info!(w, h, "Resolution: explicit WxH");
        (w as u16, h as u16)
    } else if let Ok(scale) = res_mode.parse::<u32>() {
        // Legacy hidpi_scale: 1, 2, 3, 4 → multiply logical resolution
        let scale = scale.max(1).min(4);
        let (w, h) = (logical_w as u32 * scale, logical_h as u32 * scale);
        tracing::info!(
            scale,
            w,
            h,
            logical_w,
            logical_h,
            "Resolution: legacy hidpi_scale → {}x{}",
            w,
            h
        );
        (w as u16, h as u16)
    } else {
        tracing::warn!(
            res_mode,
            "Resolution: unrecognized value, using logical resolution"
        );
        (logical_w, logical_h)
    }
}

use std::net::{SocketAddr, TcpListener};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;

use anyhow::{Context, Result};
use ironrdp_server::gfx::GfxState;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::callbacks::*;
use crate::config::ServerConfig;
use crate::permissions;
use crate::tls;

fn sanitize_f64(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn sanitize_f32(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn derive_transport_mode(gfx: &GfxState) -> String {
    if !gfx.session_uses_gfx {
        return "bitmap".to_string();
    }

    if gfx.bitmap_fallback_mode {
        return "bitmap-fallback".to_string();
    }

    if gfx.avc_fallback_to_uncompressed {
        return "gfx-uncompressed-fallback".to_string();
    }

    if gfx.avc420_supported {
        if gfx.avc444_supported && gfx.avc444_enabled {
            "gfx-avc444".to_string()
        } else {
            "gfx-avc420".to_string()
        }
    } else if gfx.caps_confirmed {
        "gfx-uncompressed".to_string()
    } else {
        "gfx-negotiating".to_string()
    }
}

fn derive_encoder_mode(gfx: &GfxState, transport_mode: &str) -> String {
    let encoder = gfx.encoder_pref.as_deref().unwrap_or("auto");
    let chroma = gfx.chroma_mode.as_deref().unwrap_or("avc420");

    match transport_mode {
        "gfx-avc444" => format!("{encoder} / avc444"),
        "gfx-avc420" => format!("{encoder} / avc420"),
        "gfx-uncompressed" | "gfx-uncompressed-fallback" => {
            format!("{encoder} / {chroma} configured, uncompressed active")
        }
        "bitmap" | "bitmap-fallback" => {
            format!("{encoder} / {chroma} configured, bitmap active")
        }
        _ => format!("{encoder} / {chroma}"),
    }
}

fn derive_bitrate_mode(transport_mode: &str) -> String {
    match transport_mode {
        "gfx-avc420" | "gfx-avc444" => "AVC target + wire throughput".to_string(),
        "gfx-uncompressed" | "gfx-uncompressed-fallback" => {
            "Wire throughput only (uncompressed GFX)".to_string()
        }
        "bitmap" | "bitmap-fallback" => "Wire throughput only (bitmap updates)".to_string(),
        _ => "Wire throughput".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_resolution;

    #[test]
    fn parse_resolution_accepts_valid_dimensions() {
        assert_eq!(parse_resolution("3840x2160"), Some((3840, 2160)));
    }

    #[test]
    fn parse_resolution_rejects_invalid_dimensions() {
        assert_eq!(parse_resolution("bad"), None);
        assert_eq!(parse_resolution("1920"), None);
        assert_eq!(parse_resolution("1920xnope"), None);
    }
}

#[derive(Debug, Default)]
struct MetricsSnapshot {
    last_sample_at: Option<Instant>,
    last_bytes_sent: u64,
}

/// Handle to a running RDP server. Use to query state and stop the server.
pub struct ServerHandle {
    port: u16,
    bind_addr: SocketAddr,
    started_at: Instant,
    fps: u32,
    gfx_state: Arc<Mutex<GfxState>>,
    input_debug_state: Arc<Mutex<InputDebugState>>,
    active_connection: Arc<Mutex<Option<ConnectionInfo>>>,
    shutdown_notify: Arc<Notify>,
    stopped: AtomicBool,
    /// The OS thread running the RDP server (RdpServer is !Send, needs dedicated thread)
    server_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    metrics_task: Mutex<Option<JoinHandle<()>>>,
    metrics_snapshot: Mutex<MetricsSnapshot>,
    config_tx: tokio::sync::mpsc::UnboundedSender<ConfigUpdate>,
}

impl ServerHandle {
    /// The TCP port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Current server status.
    pub fn status(&self) -> ServerStatus {
        if self.stopped.load(Ordering::Relaxed) {
            ServerStatus {
                running: false,
                state: "stopped".to_string(),
                uptime_secs: 0,
                port: Some(self.port),
                bind_address: Some(self.bind_addr.ip().to_string()),
            }
        } else {
            ServerStatus {
                running: true,
                state: "running".to_string(),
                uptime_secs: self.started_at.elapsed().as_secs(),
                port: Some(self.port),
                bind_address: Some(self.bind_addr.ip().to_string()),
            }
        }
    }

    /// Current performance metrics.
    pub fn metrics(&self) -> Metrics {
        let gfx = self.gfx_state.lock().unwrap();
        let now = Instant::now();
        let mut snapshot = self.metrics_snapshot.lock().unwrap();
        let bitrate_kbps = if let Some(last_sample_at) = snapshot.last_sample_at {
            let elapsed = now.duration_since(last_sample_at).as_secs_f64();
            if elapsed > 0.0 {
                let delta_bytes = gfx
                    .total_bytes_sent
                    .saturating_sub(snapshot.last_bytes_sent);
                ((delta_bytes as f64 * 8.0) / elapsed / 1000.0).round() as u64
            } else {
                0
            }
        } else {
            0
        };
        snapshot.last_sample_at = Some(now);
        snapshot.last_bytes_sent = gfx.total_bytes_sent;
        let rtt_ms = sanitize_f64(gfx.rtt_ewma_ms);
        let encode_ms = sanitize_f64(gfx.last_encode_ms);
        let net_ms = sanitize_f64((rtt_ms - encode_ms).max(0.0));
        let transport_mode = derive_transport_mode(&gfx);
        let encoder_mode = derive_encoder_mode(&gfx, &transport_mode);
        let bitrate_mode = derive_bitrate_mode(&transport_mode);
        Metrics {
            fps: gfx
                .applied_frame_rate
                .or(gfx.target_frame_rate)
                .unwrap_or(self.fps)
                .max(1),
            bitrate_kbps,
            rtt_ms,
            bytes_sent: gfx.total_bytes_sent,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            encode_ms,
            net_ms,
            last_frame_bytes: gfx.last_frame_bytes,
            network_quality: sanitize_f32(gfx.network_quality),
            pending_acks: gfx.pending_acks,
            transport_mode,
            encoder_mode,
            bitrate_mode,
        }
    }

    /// Current keyboard/input diagnostic state.
    pub fn input_debug_state(&self) -> InputDebugState {
        self.input_debug_state.lock().unwrap().clone()
    }

    /// Current active connection, if a client is connected.
    pub fn active_connection(&self) -> Option<ConnectionInfo> {
        self.active_connection.lock().unwrap().clone()
    }

    /// Push a hot config update to the running server.
    pub fn update_config(&self, update: ConfigUpdate) {
        let _ = self.config_tx.send(update);
    }

    /// Gracefully stop the server.
    pub async fn stop(&self) -> Result<()> {
        if self.stopped.swap(true, Ordering::Relaxed) {
            return Ok(()); // Already stopped
        }
        self.shutdown_notify.notify_waiters();

        // Extract thread handle from mutex *before* the await point so that
        // the MutexGuard is dropped and the future remains Send.
        let thread = self.server_thread.lock().unwrap().take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = thread.join();
            })
            .await;
        }
        // Cancel metrics task
        let task = self.metrics_task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
        }
        Ok(())
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if !self.stopped.load(Ordering::Relaxed) {
            self.shutdown_notify.notify_waiters();
        }
    }
}

/// Arguments that are Send-safe, passed to the server thread.
/// RdpServer itself is !Send, so we pass everything needed to construct it.
struct ServerThreadArgs {
    config: ServerConfig,
    bind_addr: SocketAddr,
    cert_path: std::path::PathBuf,
    key_path: std::path::PathBuf,
    width: u16,
    height: u16,
    quality: macrdp_encode::Quality,
    encoder_pref: macrdp_encode::EncoderPreference,
    mode_444: bool,
    coord_mapper: crate::handler::MouseCoordMapper,
    gfx_state: Arc<Mutex<GfxState>>,
    input_debug_state: Arc<Mutex<InputDebugState>>,
    active_connection: Arc<Mutex<Option<ConnectionInfo>>>,
    shutdown_notify: Arc<Notify>,
    handler: Arc<dyn ServerEventHandler>,
    config_rx: tokio::sync::mpsc::UnboundedReceiver<ConfigUpdate>,
}

/// Try to bind to the given address. If the port is in use, try the next 99 ports.
/// Returns the successfully bound TcpListener and the actual port.
fn find_available_port(addr: &str, preferred_port: u16) -> Result<(TcpListener, u16)> {
    for offset in 0..100u16 {
        let port = preferred_port
            .checked_add(offset)
            .context("Port number overflow")?;
        let bind_addr = format!("{addr}:{port}");
        match TcpListener::bind(&bind_addr) {
            Ok(listener) => {
                if offset > 0 {
                    tracing::warn!(
                        preferred = preferred_port,
                        actual = port,
                        "Preferred port in use, using alternative"
                    );
                }
                return Ok((listener, port));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                tracing::debug!(port, "Port in use, trying next");
                continue;
            }
            Err(e) => {
                return Err(e).context(format!("Failed to bind to {bind_addr}"));
            }
        }
    }
    anyhow::bail!(
        "No available port found in range {}-{}",
        preferred_port,
        preferred_port + 99
    )
}

/// Start the RDP server with the given configuration and event handler.
/// Returns a handle to control the running server.
pub async fn start_server(
    config: ServerConfig,
    handler: impl ServerEventHandler,
) -> Result<Arc<ServerHandle>> {
    let handler: Arc<dyn ServerEventHandler> = Arc::new(handler);

    // Notify handler: starting
    handler.on_status_change(ServerStatus {
        running: true,
        state: "starting".to_string(),
        uptime_secs: 0,
        port: None,
        bind_address: None,
    });

    // Check and request permissions
    let perms = permissions::request_permissions();
    tracing::info!(?perms, "Permission status");

    // TLS
    let (cert_path, key_path) =
        tls::ensure_tls_files(config.cert_path.as_deref(), config.key_path.as_deref())?;

    // Display detection — SCK for capture sizing, CG for mouse mapping
    let (sck_w, sck_h) = match permissions::detect_display_size() {
        Ok((w, h)) => {
            tracing::info!(width = w, height = h, "SCK logical display size");
            (w as u16, h as u16)
        }
        Err(e) => {
            tracing::warn!("Failed to detect SCK display size: {e}, defaulting to 1920x1080");
            (1920u16, 1080u16)
        }
    };
    let (cg_w, cg_h) = match macrdp_capture::detect_cg_display_size() {
        Ok((w, h)) => {
            tracing::info!(
                width = w,
                height = h,
                "CG logical display size (CGEvent coordinate space)"
            );
            (w as u16, h as u16)
        }
        Err(e) => {
            tracing::warn!("Failed to detect CG display size: {e}, falling back to SCK size");
            (sck_w, sck_h)
        }
    };
    if sck_w != cg_w || sck_h != cg_h {
        tracing::warn!(sck_w, sck_h, cg_w, cg_h, "SCK and CG display sizes differ!");
    }

    // Resolution
    let (width, height) = if config.width > 0 && config.height > 0 {
        (config.width as u16, config.height as u16)
    } else {
        (sck_w, sck_h)
    };

    // Quality / encoder / chroma
    let quality = match config.quality.as_deref() {
        Some("low_latency") => macrdp_encode::Quality::LowLatency,
        Some("balanced") => macrdp_encode::Quality::Balanced,
        _ => macrdp_encode::Quality::HighQuality,
    };
    let encoder_pref = macrdp_encode::EncoderPreference::from_str_opt(config.encoder.as_deref());
    let mode_444 = config.chroma_mode.as_deref() == Some("avc444");
    let gfx_validation_mode = config.gfx_validation_mode.unwrap_or(false);

    // Resolution
    let res_mode = config.resolution.as_deref().unwrap_or("auto");
    let (width, height) = resolve_resolution(res_mode, width, height);

    // Mouse coordinate mapping: mac = rdp × cg_logical ÷ rdp_desktop
    let coord_mapper = crate::handler::MouseCoordMapper::new(cg_w, cg_h, width, height);

    // GFX state (shared between server thread and metrics task)
    let gfx_state = Arc::new(Mutex::new(GfxState::new(
        width,
        height,
        mode_444,
        gfx_validation_mode,
    )));
    let input_debug_state = Arc::new(Mutex::new(InputDebugState::default()));
    let active_connection = Arc::new(Mutex::new(None::<ConnectionInfo>));

    let bind_host = config.bind_address.as_deref().unwrap_or("0.0.0.0");
    let (listener, port) = find_available_port(bind_host, config.port)?;
    let bind_addr = listener.local_addr()?;
    // Drop listener so the RDP server can rebind the same port.
    // IronRDP's RdpServer::with_addr() binds its own socket; we cannot pass
    // a pre-bound TcpListener. The TOCTOU window is microsecond-level.
    drop(listener);
    let shutdown_notify = Arc::new(Notify::new());
    let (config_tx, config_rx) = tokio::sync::mpsc::unbounded_channel::<ConfigUpdate>();

    // Pack everything the server thread needs
    let args = ServerThreadArgs {
        config: config.clone(),
        bind_addr,
        cert_path,
        key_path,
        width,
        height,
        quality,
        encoder_pref,
        mode_444,
        coord_mapper,
        gfx_state: Arc::clone(&gfx_state),
        input_debug_state: Arc::clone(&input_debug_state),
        active_connection: Arc::clone(&active_connection),
        shutdown_notify: Arc::clone(&shutdown_notify),
        handler: Arc::clone(&handler),
        config_rx,
    };

    // Spawn RDP server on a dedicated OS thread.
    // RdpServer is !Send (contains dyn SoundServerFactory, dyn CliprdrServerFactory),
    // so we construct and run it entirely within this thread.
    let server_thread = std::thread::Builder::new()
        .name("rdp-server".into())
        .spawn(move || {
            run_server_thread(args);
        })
        .context("Failed to spawn RDP server thread")?;

    // Spawn metrics ticker (1 second interval) — all types here are Send
    let metrics_gfx = Arc::clone(&gfx_state);
    let handler_for_metrics = Arc::clone(&handler);
    let fps = config.frame_rate;
    let metrics_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut prev_bytes_sent: u64 = 0;
        loop {
            interval.tick().await;
            let (
                rtt_ms,
                bytes_sent,
                encode_ms,
                last_frame_bytes,
                network_quality,
                pending_acks,
                transport_mode,
                encoder_mode,
                bitrate_mode,
            ) = {
                let gfx = metrics_gfx.lock().unwrap();
                let transport_mode = derive_transport_mode(&gfx);
                let encoder_mode = derive_encoder_mode(&gfx, &transport_mode);
                let bitrate_mode = derive_bitrate_mode(&transport_mode);
                (
                    gfx.rtt_ewma_ms,
                    gfx.total_bytes_sent,
                    gfx.last_encode_ms,
                    gfx.last_frame_bytes,
                    gfx.network_quality,
                    gfx.pending_acks,
                    transport_mode,
                    encoder_mode,
                    bitrate_mode,
                )
            };
            let delta_bytes = bytes_sent.saturating_sub(prev_bytes_sent);
            prev_bytes_sent = bytes_sent;
            let bitrate_kbps = delta_bytes * 8 / 1000;
            let rtt_ms = sanitize_f64(rtt_ms);
            let encode_ms = sanitize_f64(encode_ms);
            let net_ms = sanitize_f64((rtt_ms - encode_ms).max(0.0));
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            handler_for_metrics.on_metrics(Metrics {
                fps,
                bitrate_kbps,
                rtt_ms,
                bytes_sent,
                timestamp,
                encode_ms,
                net_ms,
                last_frame_bytes,
                network_quality: sanitize_f32(network_quality),
                pending_acks,
                transport_mode,
                encoder_mode,
                bitrate_mode,
            });
        }
    });

    let handle = Arc::new(ServerHandle {
        port,
        bind_addr,
        started_at: Instant::now(),
        fps,
        gfx_state,
        input_debug_state,
        active_connection,
        shutdown_notify,
        stopped: AtomicBool::new(false),
        server_thread: Mutex::new(Some(server_thread)),
        metrics_task: Mutex::new(Some(metrics_task)),
        metrics_snapshot: Mutex::new(MetricsSnapshot::default()),
        config_tx,
    });

    // Notify handler: running
    handler.on_status_change(ServerStatus {
        running: true,
        state: "running".to_string(),
        uptime_secs: 0,
        port: Some(port),
        bind_address: Some(bind_addr.ip().to_string()),
    });

    Ok(handle)
}

/// Runs the RDP server entirely on the current thread. Called from the dedicated
/// server thread. Constructs the !Send RdpServer here so it never crosses thread
/// boundaries.
fn run_server_thread(args: ServerThreadArgs) {
    use crate::display::MacDisplay;
    use crate::handler::MacInputHandler;
    use ironrdp_server::{Credentials, RdpServer, TlsIdentityCtx};

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime for RDP server");

    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let mut config_rx = args.config_rx;

        // Load TLS identity (on this thread)
        let tls_identity = match TlsIdentityCtx::init_from_paths(&args.cert_path, &args.key_path) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to load TLS certificate: {e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: format!("error: {e}"),
                    uptime_secs: 0,
                    port: Some(args.bind_addr.port()),
                    bind_address: Some(args.bind_addr.ip().to_string()),
                });
                return;
            }
        };
        let tls_acceptor = match tls_identity.make_acceptor() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Failed to create TLS acceptor: {e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: format!("error: {e}"),
                    uptime_secs: 0,
                    port: Some(args.bind_addr.port()),
                    bind_address: Some(args.bind_addr.ip().to_string()),
                });
                return;
            }
        };

        // Create input handler (clone coord_mapper — shared with display for resize sync)
        let input_handler = MacInputHandler::new(
            args.coord_mapper.clone(),
            Arc::clone(&args.input_debug_state),
            Arc::clone(&args.active_connection),
        );

        // Audio setup — shared sender slot, recreated per connection by AudioFactory
        let shared_audio_tx = if args.config.audio.enabled {
            Some(macrdp_audio::new_shared_audio_tx())
        } else {
            None
        };

        // Create audio factory
        let sound_factory: Option<Box<dyn ironrdp_server::SoundServerFactory>> =
            if let Some(ref shared_tx) = shared_audio_tx {
                Some(Box::new(macrdp_audio::MacAudioFactory::new(
                    shared_tx.clone(),
                    args.config.audio.sample_rate,
                    args.config.audio.channels,
                )))
            } else {
                None
            };

        // Clipboard factory
        let cliprdr_factory: Option<Box<dyn ironrdp_server::CliprdrServerFactory>> =
            if args.config.clipboard.enabled {
                let temp_dir = dirs::cache_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                    .join("macrdp")
                    .join("clipboard");
                Some(Box::new(macrdp_clipboard::MacClipboardFactory::new(
                    temp_dir,
                    args.config.clipboard.max_file_size_mb,
                )))
            } else {
                None
            };

        // Create display
        let res_fixed = args.config.resolution.as_deref().unwrap_or("auto") != "auto";
        let fixed_resolution = (args.config.width > 0 && args.config.height > 0) || res_fixed;
        let bitrate_override = args.config.bitrate_mbps.map(|mbps| mbps * 1_000_000);
        let show_cursor = args.config.show_cursor.unwrap_or(true);
        let display = MacDisplay::new(
            args.width,
            args.height,
            fixed_resolution,
            args.config.frame_rate,
            args.quality,
            args.encoder_pref,
            args.mode_444,
            show_cursor,
            bitrate_override,
            Arc::clone(&args.gfx_state),
            args.coord_mapper,
            shared_audio_tx,
            None, // perf_stats: not exposed via macrdp-core API yet
        );

        // Build RDP server (this is the !Send type)
        let mut server = RdpServer::builder()
            .with_addr(args.bind_addr)
            .with_hybrid(tls_acceptor, tls_identity.pub_key)
            .with_input_handler(input_handler)
            .with_display_handler(display)
            .with_sound_factory(sound_factory)
            .with_cliprdr_factory(cliprdr_factory)
            .build();

        server.set_gfx_state(Arc::clone(&args.gfx_state));

        // Credentials
        let (username, password) = match (&args.config.username, &args.config.password) {
            (Some(u), Some(p)) => (u.clone(), p.clone()),
            _ => {
                let seed = std::process::id() as u64
                    ^ std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos() as u64;
                let random_pass: String = (0..8)
                    .map(|i| {
                        let v = ((seed
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(i * 1442695040888963407))
                            >> (i * 5 + 3))
                            % 36;
                        if v < 10 {
                            (b'0' + v as u8) as char
                        } else {
                            (b'a' + (v - 10) as u8) as char
                        }
                    })
                    .collect();
                let user = "macrdp".to_string();
                tracing::warn!("No credentials in config — using generated credentials:");
                println!("\n  ┌──────────────────────────────────┐");
                println!("  │  Username: {:<22}│", &user);
                println!("  │  Password: {:<22}│", &random_pass);
                println!("  └──────────────────────────────────┘\n");
                (user, random_pass)
            }
        };
        server.set_credentials(Some(Credentials {
            username: username.clone(),
            password: password.clone(),
            domain: None,
        }));
        tracing::info!("Authentication configured for user: {}", username);

        tracing::info!(%args.bind_addr, "RDP server listening");

        // Spawn config hot-update listener
        let gfx_for_config = Arc::clone(&args.gfx_state);
        let ev_sender = server.event_sender().clone();
        tokio::task::spawn_local(async move {
            while let Some(update) = config_rx.recv().await {
                match update {
                    ConfigUpdate::FrameRate(fps) => {
                        tracing::info!(fps, "Hot-update: frame_rate");
                        gfx_for_config.lock().unwrap().target_frame_rate = Some(fps.max(1));
                    }
                    ConfigUpdate::BitrateKbps(kbps) => {
                        let bps = kbps * 1000;
                        tracing::info!(bitrate_kbps = kbps, "Hot-update: bitrate");
                        gfx_for_config.lock().unwrap().target_bitrate = Some(bps);
                    }
                    ConfigUpdate::LogLevel(level) => {
                        tracing::info!(%level, "Hot-update: log_level");
                    }
                    ConfigUpdate::ShowCursor(show) => {
                        tracing::info!(
                            show_cursor = show,
                            "Hot-update: show_cursor (next connection)"
                        );
                        gfx_for_config.lock().unwrap().show_cursor = Some(show);
                    }
                    ConfigUpdate::Resolution(res) => {
                        tracing::info!(%res, "Hot-update: resolution (next connection)");
                        gfx_for_config.lock().unwrap().resolution = Some(res);
                    }
                    ConfigUpdate::Encoder(enc) => {
                        tracing::info!(%enc, "Hot-update: encoder (next connection)");
                        gfx_for_config.lock().unwrap().encoder_pref = Some(enc);
                    }
                    ConfigUpdate::ChromaMode(mode) => {
                        let avc444 = mode == "avc444";
                        tracing::info!(%mode, avc444, "Hot-update: chroma_mode (next connection)");
                        gfx_for_config.lock().unwrap().chroma_mode = Some(mode);
                    }
                    ConfigUpdate::GfxValidationMode(enabled) => {
                        tracing::info!(
                            gfx_validation_mode = enabled,
                            "Hot-update: gfx validation mode"
                        );
                        gfx_for_config.lock().unwrap().gfx_validation_mode = enabled;
                    }
                    ConfigUpdate::Credentials { username, password } => {
                        tracing::info!(%username, "Hot-update: credentials");
                        let _ = ev_sender.send(ironrdp_server::ServerEvent::SetCredentials(
                            ironrdp_server::Credentials {
                                username,
                                password,
                                domain: None,
                            },
                        ));
                    }
                }
            }
        });

        tokio::select! {
            result = server.run() => {
                if let Err(e) = result {
                    tracing::error!("RDP server error: {e}");
                    args.handler.on_status_change(ServerStatus {
                        running: false,
                        state: format!("error: {e}"),
                        uptime_secs: 0,
                        port: Some(args.bind_addr.port()),
                        bind_address: Some(args.bind_addr.ip().to_string()),
                    });
                }
            }
            _ = args.shutdown_notify.notified() => {
                tracing::info!("Shutdown signal received, stopping server");
            }
        }
    });
}
