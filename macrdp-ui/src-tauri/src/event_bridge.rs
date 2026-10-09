//! Bridge between macrdp-core's ServerEventHandler callbacks and Tauri state/events.
//!
//! The ServerEventHandler callbacks are called from sync context (server/metrics thread).
//! Async work lives on the desktop runtime, which survives server stop/restart.
//! Connection events are ordered and database writes run on a blocking worker.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use macrdp_core::{ConnectionEvent, LogEntry, Metrics, ServerEventHandler, ServerStatus};

use crate::database::Database;
use crate::state::{AppState, Connection};

/// Maximum number of log entries to keep in memory.
const MAX_LOG_ENTRIES: usize = 5000;

/// Implements ServerEventHandler by forwarding events to Tauri state and emitting frontend events.
pub struct TauriEventBridge {
    app: AppHandle,
    connection_tx: tokio::sync::mpsc::Sender<ConnectionEvent>,
}

impl TauriEventBridge {
    pub fn new(app: AppHandle) -> Self {
        let (connection_tx, mut receiver) = tokio::sync::mpsc::channel(256);
        let worker_app = app.clone();
        // This worker belongs to the desktop runtime, not the server runtime that is
        // torn down during stop/restart. Events from each server are processed in order.
        tauri::async_runtime::spawn(async move {
            while let Some(event) = receiver.recv().await {
                process_connection_event(&worker_app, event).await;
            }
        });
        Self { app, connection_tx }
    }
}

impl ServerEventHandler for TauriEventBridge {
    fn on_status_change(&self, status: ServerStatus) {
        let app = self.app.clone();
        let status_clone = status.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                let mut guard = state.status.lock().await;
                *guard = status_clone.clone();
            }
            let _ = app.emit("server-status", &status_clone);
        });
    }

    fn on_metrics(&self, metrics: Metrics) {
        // Update tray directly (sync-safe)
        let has_connections = {
            // We can't hold async lock here, but we can check tray based on metrics
            // If bitrate > 0, there's likely a connection
            metrics.bitrate_kbps > 0
        };
        let state_str = if has_connections {
            "connected"
        } else {
            "running"
        };
        crate::tray::update_tray_status(&self.app, state_str, &metrics);

        let app = self.app.clone();
        let metrics_clone = metrics.clone();
        tauri::async_runtime::spawn(async move {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                let mut guard = state.metrics.lock().await;
                *guard = metrics_clone.clone();
            }
            let _ = app.emit("metrics", &metrics_clone);
        });
    }

    fn on_connection(&self, event: ConnectionEvent) {
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

async fn process_connection_event(app: &AppHandle, event: ConnectionEvent) {
    if let Some(state) = app.try_state::<Arc<AppState>>() {
        let mut connections = state.connections.lock().await;
        update_connections(&mut connections, &event);
        let _ = app.emit("connections", &*connections);
    }
    let _ = app.emit("connection-event", &event);
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

#[cfg(test)]
mod tests {
    use super::*;
    use macrdp_core::{ConnectionInfo, DisconnectionInfo};

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
            }),
        );
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].connected_at, "2026-01-01T00:01:00Z");
    }
}
