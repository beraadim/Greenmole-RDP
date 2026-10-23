use std::collections::{BTreeSet, VecDeque};
use std::sync::{atomic::AtomicU64, Arc};

use serde::Serialize;
use tauri::AppHandle;
use tauri::Manager;
use tokio::sync::Mutex;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;
use tracing_subscriber::Registry;

use macrdp_core::supervised::SupervisedServerHandle as ServerHandle;

use crate::ui_config::UiConfig;

// Re-export macrdp-core types that the UI serializes to the frontend.
// We keep a local Connection struct because macrdp-core doesn't track per-connection bytes_total.
pub use macrdp_core::{LogEntry, Metrics, ServerStatus};

/// An active client connection (UI-side tracking).
#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    pub client_ip: String,
    pub client_name: String,
    pub connected_at: String,
    pub bytes_total: u64,
}

/// Shared handle to the running server. `None` when the server is stopped.
pub type SharedServerHandle = Arc<Mutex<Option<Arc<ServerHandle>>>>;
pub type LogReloadHandle = reload::Handle<EnvFilter, Registry>;

/// Application-wide shared state.
pub struct AppState {
    pub(crate) exit: crate::app_lifecycle::ExitState,
    /// Serializes start/stop/settings transitions without blocking read-only polling.
    pub lifecycle_lock: Mutex<()>,
    pub stop_requests: tokio::sync::watch::Sender<u64>,
    /// Delayed events from an older server must not revive its UI state.
    pub server_generation: AtomicU64,
    pub status: Mutex<ServerStatus>,
    pub metrics: Mutex<Metrics>,
    pub(crate) diagnostic_history: Mutex<crate::diagnostics::DiagnosticHistory>,
    /// Pure lifecycle evidence reducer; held briefly at synchronous callback ingress.
    pub(crate) session_status: std::sync::Mutex<crate::session_status::SessionStatusObserver>,
    pub connections: Mutex<Vec<Connection>>,
    pub logs: Mutex<VecDeque<LogEntry>>,
    pub server_handle: SharedServerHandle,
    pub ui_config: Mutex<UiConfig>,
    /// Settings used to start this service, plus accepted live updates. Kept in
    /// the backend so pending restart changes survive window/navigation changes.
    pub running_config: Mutex<Option<UiConfig>>,
    /// Ambiguous live acknowledgements remain retryable even after the desired
    /// value returns to the last accepted snapshot. Never persisted to disk.
    pub(crate) unconfirmed_graphics: Mutex<BTreeSet<&'static str>>,
    pub log_reload: LogReloadHandle,
    pub window_move_lock: Mutex<()>,
    pub diagnostic_export_lock: Mutex<()>,
}

impl AppState {
    pub fn new(log_reload: LogReloadHandle, ui_config: UiConfig) -> Self {
        Self {
            exit: Default::default(),
            lifecycle_lock: Mutex::new(()),
            stop_requests: tokio::sync::watch::channel(0).0,
            server_generation: AtomicU64::new(0),
            status: Mutex::new(ServerStatus::default()),
            metrics: Mutex::new(Metrics::default()),
            diagnostic_history: Mutex::new(crate::diagnostics::DiagnosticHistory::default()),
            session_status: std::sync::Mutex::new(
                crate::session_status::SessionStatusObserver::default(),
            ),
            connections: Mutex::new(Vec::new()),
            logs: Mutex::new(VecDeque::new()),
            server_handle: Arc::new(Mutex::new(None)),
            ui_config: Mutex::new(ui_config),
            running_config: Mutex::new(None),
            unconfirmed_graphics: Mutex::new(BTreeSet::new()),
            log_reload,
            window_move_lock: Mutex::new(()),
            diagnostic_export_lock: Mutex::new(()),
        }
    }
}

/// Initialize application state and register it with the Tauri app.
pub fn init_app_state(
    handle: &AppHandle,
    log_reload: LogReloadHandle,
    ui_config: UiConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = Arc::new(AppState::new(log_reload, ui_config));
    handle.manage(state);
    Ok(())
}
