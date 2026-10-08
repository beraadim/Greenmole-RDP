mod config;
mod display;
mod handler;
mod perf_stats;
mod tls;

use anyhow::{Context, Result};
use clap::Parser;
use config::{Cli, ServerConfig};
use display::MacDisplay;
use handler::MacInputHandler;
use ironrdp_server::{gfx::GfxState, Credentials, RdpServer, TlsIdentityCtx};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    // A display owner must start before this process queries CoreGraphics or
    // initializes the server. Its stdio is a private parent/child protocol.
    if let Some(result) = macrdp_display::run_display_helper_if_requested() {
        return result;
    }
    run_server()
}

#[tokio::main]
async fn run_server() -> Result<()> {
    let cli = Cli::parse();
    let config = ServerConfig::load(&cli)?;

    // Initialize logging — write to both stderr and file
    let log_level = config.log_level.as_deref().unwrap_or("info");
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level));

    let log_path = std::env::current_dir()
        .unwrap_or_default()
        .join("macrdp.log");
    let log_file = std::fs::File::create(&log_path)?;

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(log_file)),
        )
        .init();

    eprintln!("Log file: {}", log_path.display());

    // Configuration includes credentials; log only operational fields.
    tracing::info!(port = config.port, backend = ?config.display_backend, "macrdp server starting");

    // Ensure TLS certificates exist
    let (cert_path, key_path) =
        tls::ensure_tls_files(config.cert_path.as_deref(), config.key_path.as_deref())?;

    // Load TLS identity
    let tls_identity = TlsIdentityCtx::init_from_paths(&cert_path, &key_path)
        .context("Failed to load TLS certificate")?;
    let tls_acceptor = tls_identity
        .make_acceptor()
        .context("Failed to create TLS acceptor")?;

    // Check and request required macOS permissions
    check_permissions();

    let backend = config.display_backend;
    let display_controller = tokio::task::spawn_blocking(move || {
        macrdp_display::DisplayController::new(backend, 1920, 1080)
    })
    .await
    .context("Display initialization worker failed")??;
    // Virtual display ownership begins with the first connected activation.
    let geometry = if backend == macrdp_display::DisplayBackend::Main {
        Some(display_controller.geometry()?)
    } else {
        None
    };

    // Parse quality setting
    let quality = match config.quality.as_deref() {
        Some("low_latency") => macrdp_encode::Quality::LowLatency,
        Some("balanced") => macrdp_encode::Quality::Balanced,
        _ => macrdp_encode::Quality::HighQuality, // default: best quality
    };

    // Parse encoder preference
    let encoder_pref = macrdp_encode::EncoderPreference::from_str_opt(config.encoder.as_deref());
    tracing::info!(?encoder_pref, "Encoder preference");

    // Parse chroma mode
    let mode_444 = config.chroma_mode.as_deref() == Some("avc444");
    tracing::info!(
        chroma_mode = config.chroma_mode.as_deref().unwrap_or("avc420"),
        mode_444,
        "Chroma mode"
    );

    let res_mode = config.resolution.as_deref().unwrap_or("auto");
    let res_auto = res_mode == "auto";
    let (width, height) = if let Some(geometry) = &geometry {
        macrdp_display::resolve_desktop_size(res_mode, geometry, config.width, config.height)?
    } else {
        macrdp_display::resolve_virtual_desktop_size(res_mode, config.width, config.height)?
    };
    let (width, height) = display_controller.normalize_size(width, height);
    let coord_mapper = handler::MouseCoordMapper::new(1, 1, width, height);
    if let Some(geometry) = &geometry {
        coord_mapper.update_display_bounds(
            geometry.origin_x,
            geometry.origin_y,
            geometry.logical_width,
            geometry.logical_height,
        );
    }
    // Virtual bounds are committed by capture activation before queued RDP
    // input is dispatched; startup never substitutes a physical display.
    tracing::info!(
        ?backend,
        ?geometry,
        virtual_display_deferred = geometry.is_none(),
        width,
        height,
        "Prepared remote desktop negotiation"
    );

    // Create shared GFX state
    let gfx_state = Arc::new(Mutex::new(GfxState::new(width, height, mode_444, false)));

    // Input disconnect queues local display restoration on the controller.
    let input_handler =
        MacInputHandler::new(coord_mapper.clone(), Some(display_controller.clone()));

    // Audio setup — shared sender slot, recreated per connection by AudioFactory
    let shared_audio_tx = if config.audio.enabled {
        Some(macrdp_audio::new_shared_audio_tx())
    } else {
        None
    };

    // Create audio factory
    let sound_factory: Option<Box<dyn ironrdp_server::SoundServerFactory>> =
        if let Some(ref shared_tx) = shared_audio_tx {
            Some(Box::new(macrdp_audio::MacAudioFactory::new(
                shared_tx.clone(),
                config.audio.sample_rate,
                config.audio.channels,
            )))
        } else {
            None
        };

    // Clipboard factory
    let cliprdr_factory: Option<Box<dyn ironrdp_server::CliprdrServerFactory>> =
        if config.clipboard.enabled {
            let temp_dir = dirs::cache_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("macrdp")
                .join("clipboard");
            Some(Box::new(macrdp_clipboard::MacClipboardFactory::new(
                temp_dir,
                config.clipboard.max_file_size_mb,
            )))
        } else {
            None
        };

    // fixed_resolution = true when user explicitly set resolution (not "auto")
    let fixed_resolution = (config.width > 0 && config.height > 0) || !res_auto;

    // Bitrate override: convert Mbps to bps, or None for auto-calculate
    let bitrate_override = config.bitrate_mbps.map(|mbps| mbps * 1_000_000);

    // Create shared performance stats (enabled via --perf flag)
    let perf_stats = if cli.perf {
        tracing::info!("Performance statistics collection enabled (--perf)");
        Some(perf_stats::new_shared(true))
    } else {
        None
    };

    // Create display with shared GFX state
    let show_cursor = config.show_cursor.unwrap_or(true);
    let display = MacDisplay::new(
        width,
        height,
        fixed_resolution,
        config.frame_rate,
        quality,
        encoder_pref,
        mode_444,
        show_cursor,
        bitrate_override,
        Arc::clone(&gfx_state),
        coord_mapper,
        shared_audio_tx,
        perf_stats.clone(),
        display_controller.clone(),
    );

    let bind_addr: SocketAddr = format!("0.0.0.0:{}", config.port).parse()?;

    // Build RDP server with Hybrid security (NLA/CredSSP)
    // This enables Windows mstsc to prompt for credentials before connecting
    let mut server = RdpServer::builder()
        .with_addr(bind_addr)
        .with_hybrid(tls_acceptor, tls_identity.pub_key)
        .with_input_handler(input_handler)
        .with_display_handler(display)
        .with_sound_factory(sound_factory)
        .with_cliprdr_factory(cliprdr_factory)
        .build();

    // Share GFX state with the server
    server.set_gfx_state(gfx_state);

    // Set credentials — required for RDP authentication
    let (username, password) = match (&config.username, &config.password) {
        (Some(u), Some(p)) => (u.clone(), p.clone()),
        _ => {
            // Generate a random password from PID + timestamp entropy
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
            tracing::info!("Set [username] and [password] in ~/.config/macrdp/config.toml to use fixed credentials");
            (user, random_pass)
        }
    };
    server.set_credentials(Some(Credentials {
        username: username.clone(),
        password: password.clone(),
        domain: None,
    }));
    tracing::info!("Authentication configured for user: {}", username);

    tracing::info!(%bind_addr, "RDP server listening");
    tracing::info!("Connect using an RDP client (e.g., Windows mstsc or Microsoft Remote Desktop)");

    // Run server; on Ctrl-C print perf summary if enabled
    let result = tokio::select! {
        res = server.run() => res.context("RDP server error"),
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received Ctrl-C — shutting down");
            Ok(())
        }
    };

    if let Some(ps) = &perf_stats {
        ps.lock().unwrap().print_summary();
    }

    // Ctrl-C cancels run() before the usual disconnect callback. Restore the
    // local display layout while the virtual display owner is still retained.
    drop(server);
    if let Err(error) =
        tokio::task::spawn_blocking(move || display_controller.end_remote_session())
            .await
            .context("Display restoration task failed")
            .and_then(|result| result)
    {
        tracing::warn!(%error, "Physical display restoration failed during shutdown");
    }

    result
}

/// Check and request all required macOS permissions at startup.
///
/// Note: `cargo run` produces unsigned debug binaries whose path/hash changes on
/// every recompile.  macOS TCC ties permissions to the exact binary identity, so
/// the preflight check may return `false` even after the user has granted access
/// to a previous build.  We therefore only warn (never auto-open Settings) and
/// continue — actual capture or input injection will fail at runtime with a clear
/// error if the permission is truly missing.
fn check_permissions() {
    tracing::info!("Checking macOS permissions...");
    let mut all_granted = true;

    // 1. Screen Recording
    if macrdp_capture::check_screen_recording_permission() {
        tracing::info!("[OK] Screen Recording permission granted");
    } else {
        all_granted = false;
        tracing::warn!("[!!] Screen Recording — preflight check returned false");
        // Trigger the system dialog (no-op if already granted to this binary)
        macrdp_capture::request_screen_recording_permission();
        tracing::warn!(
            "     If capture fails: System Settings > Privacy & Security > Screen Recording"
        );
        tracing::warn!("     Tip: debug builds change binary identity on each compile — re-authorize after rebuild");
    }

    // 2. Accessibility (required for keyboard/mouse injection)
    if macrdp_input::check_accessibility_permission() {
        tracing::info!("[OK] Accessibility permission granted");
    } else {
        all_granted = false;
        tracing::warn!("[!!] Accessibility — preflight check returned false");
        macrdp_input::request_accessibility_permission();
        tracing::warn!("     If input fails: System Settings > Privacy & Security > Accessibility");
    }

    if all_granted {
        tracing::info!("All permissions granted");
    } else {
        tracing::warn!("Some preflight checks failed — server will start anyway.");
        tracing::warn!(
            "If you already granted permission, this is likely a debug-build identity change."
        );
    }
}
