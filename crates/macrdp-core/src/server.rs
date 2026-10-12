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
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ironrdp_server::gfx::GfxState;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::callbacks::*;
use crate::config::ServerConfig;
use crate::permissions;
use crate::tls;

mod audio_metrics;
mod metrics;
mod startup;
use metrics::MetricsSnapshot;
use startup::{StartupCancellation, StartupSignal};

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

/// Final stage timings and totals; no interval rate is inferred at disconnect.
pub(crate) fn final_metrics(gfx: &GfxState, audio: &macrdp_audio::AudioStats) -> Metrics {
    let mut metrics = MetricsSnapshot::default().sample(gfx);
    audio_metrics::enrich(&mut metrics, audio);
    metrics
}

/// Handle to a running RDP server. Use to query state and stop the server.
pub struct ServerHandle {
    audio_stats: Arc<macrdp_audio::AudioStats>,
    tls_certificate: tls::TlsCertificateInfo,
    terminal_result: Arc<Mutex<Option<std::result::Result<(), String>>>>,
    display_backend: macrdp_display::DisplayBackend,
    display_controller: macrdp_display::DisplayController,
    port: u16,
    bind_addr: SocketAddr,
    started_at: Instant,
    gfx_state: Arc<Mutex<GfxState>>,
    input_debug_state: Arc<Mutex<InputDebugState>>,
    active_connection: Arc<Mutex<Option<ConnectionInfo>>>,
    shutdown_notify: Arc<Notify>,
    stopped: AtomicBool,
    stop_lock: tokio::sync::Mutex<()>,
    /// The OS thread running the RDP server (RdpServer is !Send, needs dedicated thread)
    server_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    metrics_task: Mutex<Option<JoinHandle<()>>>,
    latest_metrics: Arc<Mutex<Metrics>>,
    config_tx: tokio::sync::mpsc::Sender<ConfigUpdate>,
}

impl ServerHandle {
    /// Backend of the running server, independent of unsaved/pending UI changes.
    pub fn display_backend(&self) -> macrdp_display::DisplayBackend {
        self.display_backend
    }

    /// Fast UI availability check. The display worker revalidates its owned
    /// virtual screen when the queued action runs, including disconnect races.
    pub fn window_move_unavailable_reason(&self) -> Option<&'static str> {
        if self.stopped.load(Ordering::Acquire) {
            Some("Start the server and connect to the remote desktop first.")
        } else if self.display_backend != macrdp_display::DisplayBackend::Virtual {
            Some("Select the virtual display in Settings and restart the server.")
        } else if self.active_connection.lock().unwrap().is_none() {
            Some("Connect to the remote desktop first.")
        } else {
            None
        }
    }

    /// Enqueue before the async UI command yields so disconnect cleanup cannot
    /// overtake an action that has already been accepted for this connection.
    pub fn queue_move_windows_to_remote_with(
        &self,
        own_windows: impl FnOnce(macrdp_display::DisplayGeometry) -> Result<usize> + Send + 'static,
    ) -> Result<macrdp_display::WindowMovePreparation> {
        // The input handler holds this same lock while enqueuing disconnect.
        // Validation and enqueue therefore belong to the same connection.
        let connection = self.active_connection.lock().unwrap();
        anyhow::ensure!(
            !self.stopped.load(Ordering::Acquire),
            "The server is stopping."
        );
        anyhow::ensure!(
            self.display_backend == macrdp_display::DisplayBackend::Virtual,
            "Select the virtual display in Settings and restart the server."
        );
        anyhow::ensure!(connection.is_some(), "Connect to the remote desktop first.");
        self.display_controller
            .queue_move_windows_to_remote_with(own_windows)
    }
    /// The TCP port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Certificate actually loaded by this server, unaffected by pending settings.
    pub fn tls_certificate(&self) -> &tls::TlsCertificateInfo {
        &self.tls_certificate
    }

    /// Current server status.
    pub fn status(&self) -> ServerStatus {
        if self.stopped.load(Ordering::Acquire)
            || self.terminal_result.lock().unwrap().is_some()
            || self.thread_finished()
        {
            ServerStatus {
                running: false,
                state: if !self.stopped.load(Ordering::Acquire) {
                    "error: RDP server stopped unexpectedly"
                } else {
                    "stopped"
                }
                .to_string(),
                error_details: self
                    .terminal_result
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(|result| result.as_ref().err().cloned()),
                uptime_secs: 0,
                port: Some(self.port),
                bind_address: Some(self.bind_addr.ip().to_string()),
            }
        } else {
            ServerStatus {
                running: true,
                state: "running".to_string(),
                error_details: None,
                uptime_secs: self.started_at.elapsed().as_secs(),
                port: Some(self.port),
                bind_address: Some(self.bind_addr.ip().to_string()),
            }
        }
    }

    /// Current performance metrics.
    pub fn metrics(&self) -> Metrics {
        // All observers share the ticker's interval. Reading/exporting a report
        // must not restart rate sampling for another window or the CLI recorder.
        self.latest_metrics.lock().unwrap().clone()
    }

    /// Total successfully written display bytes without advancing rate sampling.
    pub fn bytes_sent(&self) -> u64 {
        self.gfx_state.lock().unwrap().total_bytes_sent
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
    pub fn update_config(&self, update: ConfigUpdate) -> Result<()> {
        update.validate()?;
        self.config_tx
            .try_send(update)
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    anyhow::anyhow!("Too many pending settings updates; retry shortly")
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    anyhow::anyhow!("Server configuration worker is no longer running")
                }
            })
    }

    fn thread_finished(&self) -> bool {
        self.server_thread
            .lock()
            .unwrap()
            .as_ref()
            .map_or(true, |thread| thread.is_finished())
    }

    /// Wait for a fatal server error or an orderly stop without losing a notification.
    pub async fn wait_until_stopped(&self) -> Result<()> {
        while !self.stopped.load(Ordering::Acquire) && !self.thread_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        match self.terminal_result.lock().unwrap().as_ref() {
            Some(Err(error)) => anyhow::bail!("{error}"),
            None if !self.stopped.load(Ordering::Acquire) => {
                anyhow::bail!("RDP server thread terminated unexpectedly")
            }
            _ => Ok(()),
        }
    }

    /// Gracefully stop the server.
    pub async fn stop(&self) -> Result<()> {
        let _stop = self.stop_lock.lock().await;
        if self.stopped.swap(true, Ordering::AcqRel) {
            return Ok(()); // Already stopped
        }
        self.shutdown_notify.notify_one();

        // Extract thread handle from mutex *before* the await point so that
        // the MutexGuard is dropped and the future remains Send.
        let thread = self.server_thread.lock().unwrap().take();
        let join_result = if let Some(thread) = thread {
            tokio::task::spawn_blocking(move || {
                thread
                    .join()
                    .map_err(|_| anyhow::anyhow!("RDP server thread panicked"))
            })
            .await
            .context("RDP server join task failed")
            .and_then(|result| result)
        } else {
            Ok(())
        };
        // Cancel metrics task
        let task = self.metrics_task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
        }
        let _ = tokio::task::spawn_blocking(crate::log_bridge::flush_logs).await;
        join_result?;
        if let Some(Err(error)) = self.terminal_result.lock().unwrap().as_ref() {
            anyhow::bail!("{error}");
        }
        Ok(())
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if !self.stopped.load(Ordering::Relaxed) {
            self.shutdown_notify.notify_one();
        }
        if let Some(task) = self.metrics_task.lock().unwrap().take() {
            task.abort();
        }
    }
}

/// Arguments that are Send-safe, passed to the server thread.
/// RdpServer itself is !Send, so we pass everything needed to construct it.
struct ServerThreadArgs {
    audio_stats: Arc<macrdp_audio::AudioStats>,
    terminal_result: Arc<Mutex<Option<std::result::Result<(), String>>>>,
    listener: TcpListener,
    perf_stats: Option<crate::perf_stats::SharedPerfStats>,
    display_controller: macrdp_display::DisplayController,
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
    config_rx: tokio::sync::mpsc::Receiver<ConfigUpdate>,
}

/// Try to bind to the given address. If the port is in use, try the next 99 ports.
/// Returns the successfully bound TcpListener and the actual port.
fn find_available_port(addr: &str, preferred_port: u16) -> Result<(TcpListener, u16)> {
    let last_port = preferred_port.saturating_add(99);
    for port in preferred_port..=last_port {
        match TcpListener::bind((addr, port)) {
            Ok(listener) => {
                let actual_port = listener.local_addr()?.port();
                if actual_port != preferred_port {
                    tracing::warn!(
                        preferred = preferred_port,
                        actual = actual_port,
                        "Preferred port in use, using alternative"
                    );
                }
                listener.set_nonblocking(true)?;
                return Ok((listener, actual_port));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => {
                return Err(error).context(format!("Failed to bind {addr} on port {port}"))
            }
        }
    }
    anyhow::bail!("No available port found in range {preferred_port}-{last_port}")
}

/// Start the RDP server with the given configuration and event handler.
/// Returns a handle to control the running server.
pub async fn start_server(
    config: ServerConfig,
    handler: impl ServerEventHandler,
) -> Result<Arc<ServerHandle>> {
    start_server_with_perf(config, handler, None).await
}

/// Start the shared engine with optional bounded performance collection.
pub async fn start_server_with_perf(
    config: ServerConfig,
    handler: impl ServerEventHandler,
    perf_stats: Option<crate::perf_stats::SharedPerfStats>,
) -> Result<Arc<ServerHandle>> {
    let handler: Arc<dyn ServerEventHandler> = Arc::new(handler);
    let result = start_server_inner(config, Arc::clone(&handler), perf_stats).await;
    if let Err(error) = &result {
        tracing::error!(%error, "RDP server startup failed");
        handler.on_status_change(ServerStatus {
            state: "error: RDP server startup failed".into(),
            error_details: Some(format!("{error:#}")),
            ..ServerStatus::default()
        });
    }
    result
}

async fn start_server_inner(
    config: ServerConfig,
    handler: Arc<dyn ServerEventHandler>,
    perf_stats: Option<crate::perf_stats::SharedPerfStats>,
) -> Result<Arc<ServerHandle>> {
    config.validate()?;
    // Notify handler: starting
    handler.on_status_change(ServerStatus {
        running: false,
        state: "starting".to_string(),
        error_details: None,
        uptime_secs: 0,
        port: None,
        bind_address: None,
    });

    // Check and request permissions
    let perms = permissions::request_permissions();
    tracing::info!(?perms, "Permission status");

    // TLS
    let (cert_path, key_path) = tls::ensure_tls_files(
        config.cert_path.as_deref(),
        config.key_path.as_deref(),
        &config.tls_server_names,
    )?;

    let tls_certificate = tls::inspect_tls_certificate(&cert_path)?;
    let backend = config.display_backend;
    let display_controller = tokio::task::spawn_blocking(move || {
        macrdp_display::DisplayController::new(backend, 1920, 1080)
    })
    .await
    .context("Display initialization worker failed")??;
    // An idle virtual backend owns no native display. Only physical capture
    // needs live geometry before a client negotiates its initial desktop size.
    let geometry = if backend == macrdp_display::DisplayBackend::Main {
        Some(display_controller.geometry()?)
    } else {
        None
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

    let resolution = config.resolution.as_deref().unwrap_or("auto");
    let (width, height) = if let Some(geometry) = &geometry {
        macrdp_display::resolve_desktop_size(resolution, geometry, config.width, config.height)?
    } else {
        macrdp_display::resolve_virtual_desktop_size(resolution, config.width, config.height)?
    };
    let (width, height) = display_controller.normalize_size(width, height);
    let coord_mapper = crate::handler::MouseCoordMapper::new(1, 1, width, height);
    if let Some(geometry) = &geometry {
        coord_mapper.update_display_bounds(
            geometry.origin_x,
            geometry.origin_y,
            geometry.logical_width,
            geometry.logical_height,
        );
    }
    // For virtual capture, activation commits real bounds before IronRDP
    // dispatches any queued input. The idle mapper never selects a display.
    tracing::info!(
        ?backend,
        ?geometry,
        virtual_display_deferred = geometry.is_none(),
        width,
        height,
        "Prepared remote desktop negotiation"
    );

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
    // Transfer this owned listener to the server thread; never release/rebind the port.
    let shutdown_notify = Arc::new(Notify::new());
    let mut cancellation = StartupCancellation(Some(Arc::clone(&shutdown_notify)));
    let (mut ready, ready_rx) = StartupSignal::channel();
    let (config_tx, config_rx) = tokio::sync::mpsc::channel::<ConfigUpdate>(32);

    let terminal_result = Arc::new(Mutex::new(None));
    let audio_stats = Arc::new(macrdp_audio::AudioStats::default());
    // Pack everything the server thread needs
    let args = ServerThreadArgs {
        audio_stats: audio_stats.clone(),
        terminal_result: Arc::clone(&terminal_result),
        listener,
        perf_stats,
        display_controller: display_controller.clone(),
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
    let worker_result = Arc::clone(&terminal_result);
    let server_thread = std::thread::Builder::new()
        .name("rdp-server".into())
        .spawn(move || {
            let result = run_server_thread(args, &mut ready);
            if let Err(error) = &result {
                ready.failed(error);
            }
            *worker_result.lock().unwrap() = Some(result.map_err(|error| format!("{error:#}")));
        })
        .context("Failed to spawn RDP server thread")?;

    let handle = Arc::new(ServerHandle {
        audio_stats,
        tls_certificate,
        terminal_result,
        display_backend: backend,
        display_controller,
        port,
        bind_addr,
        started_at: Instant::now(),
        gfx_state,
        input_debug_state,
        active_connection,
        shutdown_notify,
        stopped: AtomicBool::new(false),
        stop_lock: tokio::sync::Mutex::new(()),
        server_thread: Mutex::new(Some(server_thread)),
        metrics_task: Mutex::new(None),
        latest_metrics: Arc::new(Mutex::new(Metrics::default())),
        config_tx,
    });

    if let Err(error) = startup::wait_for_startup(ready_rx, Duration::from_secs(30)).await {
        // Joining also ensures a failed startup releases its listener before retrying.
        let _ = handle.stop().await;
        return Err(error);
    }
    cancellation.0 = None;
    // Spawn metrics ticker (1 second interval) — all types here are Send
    let metrics_gfx = Arc::clone(&handle.gfx_state);
    let metrics_audio = Arc::clone(&handle.audio_stats);
    let latest_metrics = Arc::clone(&handle.latest_metrics);
    let handler_for_metrics = Arc::clone(&handler);
    let worker_done = Arc::clone(&handle.terminal_result);
    let metrics_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut snapshot = MetricsSnapshot::default();
        loop {
            interval.tick().await;
            if worker_done.lock().unwrap().is_some() {
                break;
            }
            let mut metrics = snapshot.sample(&metrics_gfx.lock().unwrap());
            audio_metrics::enrich(&mut metrics, &metrics_audio);
            *latest_metrics.lock().unwrap() = metrics.clone();
            handler_for_metrics.on_metrics(metrics);
        }
    });

    *handle.metrics_task.lock().unwrap() = Some(metrics_task);

    Ok(handle)
}

/// Runs the RDP server entirely on the current thread. Called from the dedicated
/// server thread. Constructs the !Send RdpServer here so it never crosses thread
/// boundaries.
fn run_server_thread(args: ServerThreadArgs, ready: &mut StartupSignal) -> Result<()> {
    use crate::display::MacDisplay;
    use crate::handler::MacInputHandler;
    use ironrdp_server::{Credentials, RdpServer, TlsIdentityCtx};

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime for RDP server")?;

    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let mut config_rx = args.config_rx;
        let listener = tokio::net::TcpListener::from_std(args.listener)
            .context("Failed to register the bound RDP listener")?;

        // Load TLS identity (on this thread)
        let tls_identity = match TlsIdentityCtx::init_from_paths(&args.cert_path, &args.key_path) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to load TLS certificate: {e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: "error: TLS certificate initialization failed".to_string(),
                    error_details: Some(format!("{e:#}")),
                    uptime_secs: 0,
                    port: Some(args.bind_addr.port()),
                    bind_address: Some(args.bind_addr.ip().to_string()),
                });
                return Err(e).context("Failed to load TLS certificate");
            }
        };
        let tls_acceptor = match tls_identity.make_acceptor() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Failed to create TLS acceptor: {e}");
                args.handler.on_status_change(ServerStatus {
                    running: false,
                    state: "error: TLS configuration failed".to_string(),
                    error_details: Some(format!("{e:#}")),
                    uptime_secs: 0,
                    port: Some(args.bind_addr.port()),
                    bind_address: Some(args.bind_addr.ip().to_string()),
                });
                return Err(e).context("Failed to create TLS acceptor");
            }
        };

        // Retain display ownership outside the RDP server so cancellation can
        // restore the local primary display before the server thread exits.
        let display_controller = args.display_controller.clone();
        // Create input handler (clone coord_mapper — shared with display for resize sync)
        let input_handler = MacInputHandler::new(
            args.coord_mapper.clone(),
            Arc::clone(&args.input_debug_state),
            Arc::clone(&args.active_connection),
            Some(display_controller.clone()),
        )
        .with_events(
            Arc::clone(&args.handler),
            Arc::clone(&args.gfx_state),
            Arc::clone(&args.audio_stats),
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
                Some(Box::new(
                    macrdp_audio::MacAudioFactory::new(
                        shared_tx.clone(),
                        args.config.audio.sample_rate,
                        args.config.audio.channels,
                    )
                    .with_stats(args.audio_stats.clone()),
                ))
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
                    args.config.clipboard.file_transfer,
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
            args.perf_stats,
            args.display_controller,
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
        server.set_idle_timeout(Some(std::time::Duration::from_secs(
            args.config.idle_timeout_secs,
        )));

        // Validated before any native resources are created; never log the password.
        server.set_credentials(Some(Credentials {
            username: args.config.username.clone().expect("validated username"),
            password: args.config.password.clone().expect("validated password"),
            domain: None,
        }));

        tracing::info!(%args.bind_addr, "RDP listener and TLS identity ready");

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
                        if let Err(error) =
                            ev_sender.send(ironrdp_server::ServerEvent::SetCredentials(
                                ironrdp_server::Credentials {
                                    username,
                                    password,
                                    domain: None,
                                },
                            ))
                        {
                            tracing::error!(%error, "Credential control channel unavailable");
                        }
                    }
                }
            }
        });

        let server_result = if let Err(error) = ready.ready() {
            Err(error)
        } else {
            args.handler.on_status_change(ServerStatus {
                running: true,
                state: "running".into(),
                error_details: None,
                uptime_secs: 0,
                port: Some(args.bind_addr.port()),
                bind_address: Some(args.bind_addr.ip().to_string()),
            });
            tokio::select! {
            biased;
            _ = args.shutdown_notify.notified() => {
                tracing::info!("Shutdown signal received, stopping server");
                args.gfx_state.lock().unwrap().connection_outcome =
                    Some(ironrdp_server::gfx::ConnectionOutcome::ServerShutdown);
                Ok(())
            }
            result = server.run_with_listener(listener) => {
                if let Err(ref e) = result {
                    tracing::error!("RDP server error: {e}");
                    args.handler.on_status_change(ServerStatus {
                        running: false,
                        state: "error: RDP server stopped unexpectedly".to_string(),
                        error_details: Some(format!("{e:#}")),
                        uptime_secs: 0,
                        port: Some(args.bind_addr.port()),
                        bind_address: Some(args.bind_addr.ip().to_string()),
                    });
                }
                result
            }
            }
        };

        // Publish terminal state before native display cleanup so polling/start
        // cannot mistake a failed worker for a still-running server.
        *args.terminal_result.lock().unwrap() = Some(
            server_result
                .as_ref()
                .map(|_| ())
                .map_err(|error| format!("{error:#}")),
        );

        // Cancelling server.run() during shutdown skips its connection cleanup.
        // Restore the physical display layout explicitly in that path too.
        // Keep the virtual display alive until restoration has completed.
        drop(server);
        if let Err(error) = tokio::task::spawn_blocking(move || {
            crate::display::capture_lifecycle::restore_after_capture(display_controller)
        })
        .await
        .context("Display restoration task failed")
        .and_then(|result| result)
        {
            tracing::warn!(%error, "Physical display restoration failed during shutdown");
        }
        server_result
    })
}
