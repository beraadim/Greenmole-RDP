mod app_lifecycle;
mod commands;
mod configuration;
mod database;
mod diagnostics;
mod event_bridge;
mod graphics_profiles;
#[cfg(target_os = "macos")]
mod own_windows;
mod permissions;
mod recovery;
mod server_lifecycle;
mod session_status;
mod startup_identity;
mod state;
mod tray;
mod ui_config;
mod window_commands;

use std::sync::Arc;

use tauri::Manager;

use database::Database;

async fn start_server_on_launch(app: tauri::AppHandle) {
    let Some(state) = app.try_state::<Arc<state::AppState>>() else {
        tracing::warn!("App state unavailable during launch-start");
        return;
    };

    if let Err(err) = commands::ensure_server_running(&app, state.inner()).await {
        tracing::error!(%err, "Automatic server start on launch failed");
    }
}

pub fn run() {
    if let Some(result) = macrdp_core::run_guard_helper_if_requested() {
        if let Err(error) = result {
            eprintln!("Private session guard failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    // Dispatch before AppKit/Tauri, including the single-instance plugin, can
    // query displays or activate the UI in this background owner process.
    if let Some(result) = macrdp_core::run_display_helper_if_requested() {
        if let Err(error) = result {
            eprintln!("Display helper failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(result) = macrdp_core::supervised::run_server_helper_if_requested() {
        if let Err(error) = result {
            eprintln!("Server worker failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(result) = recovery::run_command_if_requested() {
        if let Err(error) = result {
            eprintln!("Local desktop recovery failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    // Load UI config early to get log level
    let ui_config = match ui_config::UiConfig::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("Cannot load Greenmole RDP configuration securely: {error}");
            return;
        }
    };
    let log_level = ui_config.log_level.clone();
    let filter = format!("macrdp_ui={log_level},macrdp_core={log_level},{log_level}");

    // Initialize log file + tracing with log bridge layer
    let log_path = macrdp_core::init_log_file();
    eprintln!("Log file: {} (level: {})", log_path.display(), log_level);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::reload;
    use tracing_subscriber::util::SubscriberInitExt;
    let (filter_layer, log_reload) = reload::Layer::new(macrdp_core::with_transition_diagnostics(
        tracing_subscriber::EnvFilter::new(&filter),
    ));
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(macrdp_core::LogBridgeLayer::new())
        .init();

    tracing::info!(
        process_id = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        revision = option_env!("MACRDP_BUILD_REVISION").unwrap_or("development"),
        "Greenmole RDP starting..."
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            app_lifecycle::request_show_main(app, "second_instance");
        }))
        .plugin(startup_identity::plugin())
        .setup(move |app| {
            let handle = app.handle().clone();

            // Initialize application state
            state::init_app_state(&handle, log_reload.clone(), ui_config.clone())?;
            startup_identity::refresh_existing_registration(&handle, ui_config.autostart);
            recovery::install_control_endpoint(&handle)?;

            // Setup is explicit and only allowed while no remote owner exists.
            // Automatic launch never races a system dialog against virtual-display creation.
            let perms = macrdp_core::permissions::check_permissions();
            tracing::info!(?perms, "Startup permission check completed");

            // Database for logs and history
            let db_path = dirs::data_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("macrdp")
                .join("macrdp.db");
            if let Some(parent) = db_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let database = Arc::new(Database::new(&db_path)?);
            app.manage(database);

            // Setup tray icon
            tray::setup_tray(&handle)?;

            // Intercept main window close — hide instead of quit
            if let Some(window) = app.get_webview_window("main") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            // Hide popover on blur
            if let Some(popover) = app.get_webview_window("popover") {
                let p = popover.clone();
                popover.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let _ = p.hide();
                    }
                });
            }

            let app_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                start_server_on_launch(app_handle).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::start_server,
            commands::stop_server,
            commands::recover_local_desktop,
            commands::get_server_status,
            commands::get_metrics,
            commands::get_connections,
            session_status::get_session_status,
            commands::get_permissions,
            commands::request_permissions,
            commands::get_input_debug_state,
            configuration::get_config,
            configuration::get_configuration_status,
            commands::get_tls_status,
            configuration::set_config,
            graphics_profiles::get_graphics_profiles,
            graphics_profiles::save_graphics_profile,
            graphics_profiles::delete_graphics_profile,
            graphics_profiles::apply_graphics_profile,
            commands::get_logs,
            diagnostics::export_diagnostics,
            commands::get_connection_history,
            commands::get_traffic_stats,
            commands::check_for_updates,
            commands::show_main_window,
            commands::open_system_preferences,
            commands::quit_app,
            commands::set_autostart,
            window_commands::get_window_move_status,
            window_commands::move_windows_to_remote,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Greenmole RDP")
        .run(app_lifecycle::handle_run_event);
}
