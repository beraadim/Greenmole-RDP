//! Bridge between macrdp-core's ServerEventHandler callbacks and Tauri state/events.
//!
//! The ServerEventHandler callbacks are called from sync context (server/metrics thread).
//! Async work lives on the desktop runtime, which survives server stop/restart.
//! Connection events are ordered and database writes run on a blocking worker.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use tauri::{AppHandle, Emitter, Manager};

use macrdp_core::{ConnectionEvent, LogEntry, Metrics, ServerEventHandler, ServerStatus};

use crate::database::Database;
use crate::state::{AppState, Connection};

/// Maximum number of log entries to keep in memory.
const MAX_LOG_ENTRIES: usize = 5000;

/// Implements ServerEventHandler by forwarding events to Tauri state and emitting frontend events.
pub struct TauriEventBridge {
    app: AppHandle,
    generation: u64,
    status_sequence: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    connection_tx: tokio::sync::mpsc::Sender<ConnectionEvent>,
}

impl TauriEventBridge {
    pub fn new(app: AppHandle, generation: u64) -> Self {
        if let Some(state) = app.try_state::<Arc<AppState>>() {
            if let Ok(mut observer) = state.session_status.lock() {
                observer.begin_generation(generation);
            }
        }
        let (connection_tx, mut receiver) = tokio::sync::mpsc::channel(256);
        let worker_app = app.clone();
        let running = Arc::new(AtomicBool::new(false));
        let worker_running = running.clone();
        // This worker belongs to the desktop runtime, not the server runtime that is
        // torn down during stop/restart. Events from each server are processed in order.
        tauri::async_runtime::spawn(async move {
            while let Some(event) = receiver.recv().await {
                process_connection_event(&worker_app, event, generation, &worker_running).await;
            }
        });
        Self {
            app,
            generation,
            status_sequence: Arc::new(AtomicU64::new(0)),
            running,
            connection_tx,
        }
    }
}

impl ServerEventHandler for TauriEventBridge {
    fn on_status_change(&self, status: ServerStatus) {
        if let Some(state) = self.app.try_state::<Arc<AppState>>() {
            if let Ok(mut observer) = state.session_status.lock() {
                observer.status(self.generation, &status);
            }
        }
        // Stop accepting metrics immediately, before the async UI status task runs.
        // Polling a handle during thread cleanup must not revive a failed server's tray.
        self.running.store(status.running, Ordering::Release);
        let app = self.app.clone();
        let generation = self.generation;
        let sequence = self.status_sequence.clone();
        let revision = sequence.fetch_add(1, Ordering::AcqRel) + 1;
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                let mut guard = state.status.lock().await;
                if !event_is_current(
                    generation,
                    state.server_generation.load(Ordering::Acquire),
                    revision,
                    sequence.load(Ordering::Acquire),
                ) {
                    return;
                }
                *guard = status.clone();
                if !status.running {
                    let mut metrics = state.metrics.lock().await;
                    *metrics = Metrics::default();
                    crate::tray::update_tray_status(&app, &status.state, &metrics);
                    let _ = app.emit("metrics", &*metrics);
                    let mut connections = state.connections.lock().await;
                    connections.clear();
                    let _ = app.emit("connections", &*connections);
                }
                let _ = app.emit("server-status", &status);
            }
        });
    }

    fn on_metrics(&self, metrics: Metrics) {
        if let Some(state) = self.app.try_state::<Arc<AppState>>() {
            if state.server_generation.load(Ordering::Acquire) == self.generation
                && self.running.load(Ordering::Acquire)
            {
                if let Ok(mut observer) = state.session_status.lock() {
                    observer.metrics(self.generation, &metrics);
                }
            }
        }
        let app = self.app.clone();
        let generation = self.generation;
        let running = self.running.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                // Use the same lock order as status callbacks and hold status through
                // tray publication, so a terminal update always clears older metrics.
                let status = state.status.lock().await;
                let mut guard = state.metrics.lock().await;
                if !metrics_are_current(
                    generation,
                    state.server_generation.load(Ordering::Acquire),
                    status.running,
                    running.load(Ordering::Acquire),
                ) {
                    return;
                }
                // Callback tasks may acquire the lock out of order. Cached polling
                // must never expose an older snapshot after a newer event.
                if metrics.sample_id > 0 && metrics.sample_id <= guard.sample_id {
                    return;
                }
                *guard = metrics.clone();
                state
                    .diagnostic_history
                    .lock()
                    .await
                    .record_metrics(generation, &metrics);
                crate::tray::update_tray_status(
                    &app,
                    if metrics.bitrate_kbps > 0 {
                        "connected"
                    } else {
                        "running"
                    },
                    &metrics,
                );
                let _ = app.emit("metrics", &metrics);
            }
        });
    }

    fn on_connection(&self, event: ConnectionEvent) {
        if let Some(state) = self.app.try_state::<Arc<AppState>>() {
            // Stop increments the service generation before real cleanup completes.
            // Retain that bridge's disconnect evidence until a new bridge replaces it.
            if matches!(&event, ConnectionEvent::Disconnected(_))
                || state.server_generation.load(Ordering::Acquire) == self.generation
            {
                if let Ok(mut observer) = state.session_status.lock() {
                    observer.connection(self.generation, &event);
                }
            }
        }
        if let Err(error) = self.connection_tx.try_send(event) {
            tracing::error!(%error, "Connection history queue unavailable; event not persisted");
            let _ = self.app.emit(
                "connection-history-error",
                "Connection event could not be queued",
            );
        }
    }

    fn on_log(&self, entry: LogEntry) {
        let app = self.app.clone();
        let entry_clone = entry.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                let mut guard = state.logs.lock().await;
                if guard.len() >= MAX_LOG_ENTRIES {
                    guard.pop_front();
                }
                guard.push_back(entry_clone.clone());
            }
            let _ = app.emit("log", &entry_clone);
        });
    }

    fn on_operational_record(&self, record: &str) {
        if let Some(state) = self.app.try_state::<Arc<AppState>>() {
            if let Ok(mut observer) = state.session_status.lock() {
                observer.record(self.generation, record);
            }
        }
    }
}

/// Session identity includes the start time, so a delayed disconnect cannot remove a
/// newer connection from the same address during a server restart.
fn update_connections(connections: &mut Vec<Connection>, event: &ConnectionEvent) {
    match event {
        ConnectionEvent::Connected(info) => {
            if !connections.iter().any(|connection| {
                connection.client_ip == info.client_ip
                    && connection.connected_at == info.connected_at
            }) {
                connections.push(Connection {
                    client_ip: info.client_ip.clone(),
                    client_name: info.client_name.clone(),
                    connected_at: info.connected_at.clone(),
                    bytes_total: 0,
                });
            }
        }
        ConnectionEvent::Disconnected(info) => connections.retain(|connection| {
            connection.client_ip != info.client_ip || connection.connected_at != info.connected_at
        }),
    }
}

fn event_is_current(
    generation: u64,
    current_generation: u64,
    revision: u64,
    latest_revision: u64,
) -> bool {
    generation == current_generation && revision == latest_revision
}

fn metrics_are_current(
    generation: u64,
    current_generation: u64,
    ui_running: bool,
    server_running: bool,
) -> bool {
    generation == current_generation && ui_running && server_running
}

async fn process_connection_event(
    app: &AppHandle,
    event: ConnectionEvent,
    generation: u64,
    running: &AtomicBool,
) {
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        if let ConnectionEvent::Disconnected(info) = &event {
            state
                .diagnostic_history
                .lock()
                .await
                .record_disconnect(generation, info);
        }
        let mut connections = state.connections.lock().await;
        if connection_is_current(
            generation,
            state.server_generation.load(Ordering::Acquire),
            running.load(Ordering::Acquire),
            &event,
        ) {
            update_connections(&mut connections, &event);
            let _ = app.emit("connections", &*connections);
            let _ = app.emit("connection-event", &event);
        }
    }
    // Persist completed older sessions even when their UI events are obsolete.
    if let ConnectionEvent::Disconnected(info) = event {
        let database = app
            .try_state::<Arc<Database>>()
            .map(|database| Arc::clone(database.inner()));
        let result = match database {
            Some(database) => {
                match tokio::task::spawn_blocking(move || database.record_disconnection(&info))
                    .await
                {
                    Ok(result) => result,
                    Err(error) => Err(format!("Connection history worker failed: {error}")),
                }
            }
            None => Err("Connection history database is unavailable".to_string()),
        };
        if let Err(error) = result {
            tracing::error!(%error, "Failed to persist connection history");
            let _ = app.emit("connection-history-error", &error);
        }
    }
}

fn connection_is_current(
    generation: u64,
    current_generation: u64,
    running: bool,
    event: &ConnectionEvent,
) -> bool {
    generation == current_generation
        && (running || matches!(event, ConnectionEvent::Disconnected(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use macrdp_core::{ConnectionInfo, DisconnectionInfo};

    #[test]
    fn delayed_metrics_cannot_revive_a_terminal_or_replaced_server() {
        assert!(metrics_are_current(2, 2, true, true));
        // Fatal callback received, but its async status update has not run yet.
        assert!(!metrics_are_current(2, 2, true, false));
        // Terminal UI state already applied, including a manual stop.
        assert!(!metrics_are_current(2, 2, false, true));
        // A healthy replacement server must still reject the old ticker.
        assert!(!metrics_are_current(1, 2, true, true));
    }

    #[test]
    fn superseded_startup_events_and_old_servers_cannot_revive_ui_state() {
        assert!(!event_is_current(1, 2, 4, 4));
        assert!(!event_is_current(2, 2, 1, 3));
        assert!(event_is_current(2, 2, 3, 3));
    }

    #[test]
    fn delayed_disconnect_preserves_reconnection_from_same_address() {
        let mut connections = Vec::new();
        let connected = |at: &str| {
            ConnectionEvent::Connected(ConnectionInfo {
                client_ip: "192.0.2.1".into(),
                client_name: "test".into(),
                connected_at: at.into(),
            })
        };
        assert!(connection_is_current(2, 2, true, &connected("current")));
        assert!(!connection_is_current(
            2,
            2,
            false,
            &connected("late-after-failure")
        ));
        assert!(!connection_is_current(1, 2, true, &connected("old-server")));
        update_connections(&mut connections, &connected("2026-01-01T00:00:00Z"));
        update_connections(&mut connections, &connected("2026-01-01T00:01:00Z"));
        update_connections(&mut connections, &connected("2026-01-01T00:01:00Z"));
        assert_eq!(connections.len(), 2);
        update_connections(
            &mut connections,
            &ConnectionEvent::Disconnected(DisconnectionInfo {
                client_ip: "192.0.2.1".into(),
                client_name: "test".into(),
                connected_at: "2026-01-01T00:00:00Z".into(),
                disconnected_at: "2026-01-01T00:00:59Z".into(),
                duration_secs: 59,
                bytes_total: 100,
                ..Default::default()
            }),
        );
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].connected_at, "2026-01-01T00:01:00Z");
    }
}
