mod commands;
mod database;
mod event_bridge;
mod permissions;
mod state;
mod tray;
mod ui_config;
mod window_commands;
#[cfg(target_os = "macos")]
mod own_windows;

use std::sync::Arc;

use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

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
    // Dispatch before AppKit/Tauri, including the single-instance plugin, can
    // query displays or activate the UI in this background owner process.
    if let Some(result) = macrdp_core::run_display_helper_if_requested() {
        if let Err(error) = result {
            eprintln!("Display helper failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }
    // Load UI config early to get log level
    let ui_config = ui_config::UiConfig::load().unwrap_or_default();
    let log_level = ui_config.log_level.clone();
    let filter = format!("macrdp_ui={log_level},macrdp_core={log_level},{log_level}");

    // Initialize log file + tracing with log bridge layer
    let log_path = macrdp_core::init_log_file();
    eprintln!("Log file: {} (level: {})", log_path.display(), log_level);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::reload;
    use tracing_subscriber::util::SubscriberInitExt;
    let (filter_layer, log_reload) =
        reload::Layer::new(tracing_subscriber::EnvFilter::new(&filter));
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(macrdp_core::LogBridgeLayer::new())
        .init();

    tracing::info!("macrdp-ui starting...");

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Focus main window when a second instance is launched
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .setup(move |app| {
            let handle = app.handle().clone();

            // Initialize application state
            state::init_app_state(&handle, log_reload.clone())?;

            // Request permissions proactively on startup (non-blocking)
            std::thread::spawn(|| {
                let perms = macrdp_core::permissions::request_permissions();
                tracing::info!(?perms, "Startup permission request completed");
            });

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
            commands::get_server_status,
            commands::get_metrics,
            commands::get_connections,
            commands::get_permissions,
            commands::get_input_debug_state,
            commands::get_config,
            commands::set_config,
            commands::get_logs,
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
        .run(tauri::generate_context!())
        .expect("error while running macrdp-ui");
}
