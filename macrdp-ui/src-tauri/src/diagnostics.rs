//! Explicitly selected operational data, never a dump of preferences or raw logs.
//! Free-form messages can contain input text, paths, hostnames or credentials.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::{state::AppState, ui_config::UiConfig};

const LOG_BYTES: u64 = 1024 * 1024;
const MAX_EVENTS: usize = 200;
const EXPORT_BYTES: usize = 2 * 1024 * 1024;

mod history;
pub(crate) use history::DiagnosticHistory;

#[derive(Serialize)]
pub struct ExportResult {
    path: String,
}

#[tauri::command]
pub async fn export_diagnostics(state: State<'_, Arc<AppState>>) -> Result<ExportResult, String> {
    let lifecycle = state.lifecycle_lock.lock().await;
    let config = state.ui_config.lock().await.clone();
    let history = state.diagnostic_history.lock().await.snapshot();
    let (status, metrics, connection_count) = {
        let handles = state.server_handle.lock().await;
        match handles.as_ref() {
            Some(handle) => (
                handle.status(),
                handle.metrics(),
                usize::from(handle.active_connection().is_some()),
            ),
            None => (
                state.status.lock().await.clone(),
                macrdp_core::Metrics::default(),
                0,
            ),
        }
    };
    let running_config = state
        .running_config
        .lock()
        .await
        .clone()
        .filter(|_| status.running);
    let configuration_state =
        crate::configuration::configuration_status(&config, running_config.as_ref());
    drop(lifecycle);
    tokio::task::spawn_blocking(move || {
        let directory = dirs::desktop_dir().ok_or("Desktop folder is unavailable")?;
        let permissions = macrdp_core::permissions::check_permissions();
        let logs = macrdp_core::log_file_path().map_or_else(
            || json!({"available": false}),
            |path| match summarize_logs(path) {
                Ok(summary) => summary,
                Err(_) => json!({"available": false, "reason": "log_read_failed"}),
            },
        );
        let report = json!({
            "schema_version": 3,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "application": {"version": env!("CARGO_PKG_VERSION"),
                "revision": option_env!("MACRDP_BUILD_REVISION").unwrap_or("development")},
            "system": {"os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
                "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).ok()},
            "service": {"running": status.running,
                "state": service_state(&status.state), "uptime_secs": status.uptime_secs,
                "port": status.port, "connections": connection_count},
            "configuration": {
                "saved": safe_configuration(&config),
                "running": running_config.as_ref().map(safe_configuration),
                "state": configuration_state,
                "live_updates": "Running preferences include accepted live updates; observed capture rates are in metrics."
            },
            "permissions": permissions,
            "metrics": safe_metrics(&metrics),
            "history": history,
            "recent_events": logs,
            "privacy": "Only selected operational fields are included. Credentials, input text, clipboard contents, addresses, client names, paths, raw log messages and free-form error details are excluded."
        });
        let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
        let path = write_report(&directory, &bytes).map_err(|e| format!("Could not save diagnostics: {e}"))?;
        Ok(ExportResult { path: path.to_string_lossy().into_owned() })
    }).await.map_err(|e| format!("Diagnostics worker failed: {e}"))?
}

fn service_state(value: &str) -> &'static str {
    match value {
        "running" => "running",
        "starting" => "starting",
        "stopped" => "stopped",
        _ => "error",
    }
}

fn selection<'a>(value: &str, allowed: &'a [&'a str]) -> &'a str {
    allowed
        .iter()
        .copied()
        .find(|candidate| *candidate == value)
        .unwrap_or("unknown")
}

fn safe_configuration(config: &UiConfig) -> Value {
    let resolution = config
        .resolution
        .split_once('x')
        .and_then(|(w, h)| Some([w.parse::<u16>().ok()?, h.parse::<u16>().ok()?]));
    json!({
        "frame_rate": config.frame_rate, "bitrate_mbps": config.bitrate_mbps,
        "encoder": selection(&config.encoder, &["auto", "hardware", "software"]),
        "chroma": selection(&config.chroma_mode, &["avc420", "avc444"]),
        "display_backend": config.display_backend, "resolution": resolution,
        "automatic_resolution": config.resolution == "auto", "show_cursor": config.show_cursor,
        "idle_timeout_secs": config.idle_timeout_secs,
        "audio_enabled": config.audio_enabled,
        "clipboard_enabled": config.clipboard_enabled,
        "clipboard_file_transfer": config.clipboard_file_transfer,
        "clipboard_max_file_size_mb": config.clipboard_max_file_size_mb,
        "custom_tls": !config.tls_cert_path.is_empty() || !config.tls_key_path.is_empty(),
    })
}

/// Select typed values explicitly so a future Metrics field cannot leak a name/path.
fn safe_metrics(metrics: &macrdp_core::Metrics) -> Value {
    let value = serde_json::to_value(metrics).unwrap_or(Value::Null);
    let mut selected = serde_json::Map::new();
    for key in [
        "fps",
        "bitrate_kbps",
        "latency_ms",
        "bytes_sent",
        "timestamp",
        "timestamp_ms",
        "sample_id",
        "encode_ms",
        "capture_queue_ms",
        "completion_queue_ms",
        "write_ms",
        "capture_frames_coalesced",
        "requested_capture_frame_rate",
        "applied_frame_rate",
        "capture_frame_rate_update_failures",
        "capture_frame_rate_retry_exhausted",
        "requested_encoder_bitrate",
        "applied_encoder_bitrate",
        "encoder_bitrate_update_failures",
        "client_queue_depth_bytes",
        "ack_suspended",
        "last_frame_bytes",
        "network_quality",
        "pending_acks",
        "audio_dropped_frames",
        "audio_stale_frames",
        "audio_discontinuities",
        "audio_invalid_frames",
        "audio_queue_age_ms",
        "audio_queued_ms",
    ] {
        if let Some(field) = value
            .get(key)
            .filter(|v| v.is_number() || v.is_boolean() || v.is_null())
        {
            selected.insert(key.into(), field.clone());
        }
    }
    selected.insert(
        "transport_mode".into(),
        json!(selection(
            &metrics.transport_mode,
            &[
                "bitmap",
                "bitmap-fallback",
                "gfx-avc420",
                "gfx-avc444",
                "gfx-uncompressed",
                "gfx-uncompressed-fallback",
                "gfx-negotiating",
                "inactive"
            ]
        )),
    );
    // Encoder mode has a human-readable suffix; never export unvalidated free text.
    selected.insert(
        "encoder_mode".into(),
        json!(selection(
            &metrics.encoder_mode,
            &[
                "videotoolbox / avc420",
                "videotoolbox / avc444",
                "openh264 / avc420",
                "openh264 / avc444",
                "bitmap",
                "uncompressed",
                "inactive",
                "negotiating"
            ]
        )),
    );
    for (name, timing) in [
        ("capture_timings", &metrics.capture_timings),
        ("encode_timings", &metrics.encode_timings),
        ("completion_timings", &metrics.completion_timings),
        ("write_timings", &metrics.write_timings),
    ] {
        selected.insert(
            name.into(),
            json!({"samples": timing.samples,
            "p50_ms": timing.p50_ms, "p95_ms": timing.p95_ms,
            "p99_ms": timing.p99_ms, "max_ms": timing.max_ms}),
        );
    }
    selected.insert(
        "connection_outcome".into(),
        json!(metrics.connection_outcome.as_deref().map(safe_outcome)),
    );
    Value::Object(selected)
}

fn safe_outcome(value: &str) -> &str {
    selection(
        value,
        &[
            "client_disconnect",
            "peer_closed",
            "protocol_error",
            "transport_error",
            "write_timeout",
            "idle_timeout",
            "server_shutdown",
            "credentials_changed",
            "handshake_timeout",
            "handshake_rejected",
            "authentication_failed",
            "activation_failed",
            "admission_throttled",
            "capture_ended",
            "capture_error",
            "output_overflow",
        ],
    )
}

fn event_category(message: &str) -> &'static str {
    for (prefix, category) in [
        ("RDP server startup failed", "startup_failed"),
        ("RDP listener and TLS identity ready", "listener_ready"),
        ("Client loop failed", "client_failed"),
        (
            "Disconnecting client after output queue exhaustion",
            "output_overflow",
        ),
        ("TLS identity ready", "tls_ready"),
        ("Renewed managed TLS identity", "tls_renewed"),
        ("Screen capture started", "capture_started"),
        (
            "Capture cleanup could not confirm stop",
            "capture_cleanup_failed",
        ),
        ("Desktop resumed after idle", "desktop_resumed"),
        ("Capture frame rate updated", "capture_pacing"),
        (
            "Native capture frame rate update failed",
            "capture_rate_failed",
        ),
        (
            "Native capture frame rate update recovered",
            "capture_rate_recovered",
        ),
        ("Clipboard connection failed", "clipboard_failed"),
        ("Audio recovery counters", "audio_recovery"),
        ("Connection outcome", "connection_outcome"),
        ("Connection admission throttled", "admission_throttled"),
        ("Keyboard injection failed", "keyboard_injection_failed"),
        ("Mouse injection failed", "mouse_injection_failed"),
    ] {
        if message.starts_with(prefix) {
            return category;
        }
    }
    "other"
}

fn summarize_logs(path: &Path) -> std::io::Result<Value> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let offset = size.saturating_sub(LOG_BYTES);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(LOG_BYTES).read_to_end(&mut bytes)?;
    Ok(summarize_bytes(&bytes, offset > 0))
}

fn summarize_bytes(bytes: &[u8], truncated: bool) -> Value {
    let mut counts = BTreeMap::<String, u64>::new();
    let mut events = std::collections::VecDeque::new();
    let mut lines = bytes.split_inclusive(|b| *b == b'\n');
    if truncated {
        lines.next();
    }
    for line in lines.filter(|line| line.len() <= 65536 && line.last() == Some(&b'\n')) {
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let level = selection(
            event["level"].as_str().unwrap_or(""),
            &["trace", "debug", "info", "warn", "error"],
        );
        *counts.entry(level.into()).or_default() += 1;
        let category = event_category(event["message"].as_str().unwrap_or(""));
        if category == "other" && !matches!(level, "warn" | "error") {
            continue;
        }
        let timestamp = event["timestamp"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.to_rfc3339());
        let mut fields = serde_json::Map::new();
        for key in [
            "fps",
            "requested_fps",
            "applied_fps",
            "attempt",
            "width",
            "height",
            "bitrate_kbps",
            "idle_ms",
            "pending_acks",
            "encode_ms",
            "write_ms",
            "queue_age_ms",
            "retry_after_ms",
            "queued_ms",
            "dropped_frames",
            "stale_frames",
            "invalid_frames",
            "discontinuities",
            "duration_ms",
        ] {
            if let Some(value) = event["fields"].get(key).filter(|v| v.is_number()) {
                fields.insert(key.into(), value.clone());
            }
        }
        if category == "connection_outcome" {
            fields.insert(
                "reason".into(),
                json!(safe_outcome(
                    event["fields"]["reason"].as_str().unwrap_or("")
                )),
            );
            if let Some(authenticated) = event["fields"]["authenticated"].as_bool() {
                fields.insert("authenticated".into(), json!(authenticated));
            }
        }
        if category == "capture_rate_failed" {
            if let Some(retrying) = event["fields"]["retrying"].as_bool() {
                fields.insert("retrying".into(), json!(retrying));
            }
        }
        if category == "clipboard_failed" {
            fields.insert(
                "reason".into(),
                json!(selection(
                    event["fields"]["reason"].as_str().unwrap_or(""),
                    &[
                        "clipboard worker panicked",
                        "clipboard worker unavailable (previous storage operation still blocked)",
                        "clipboard worker queue capacity exceeded",
                        "clipboard request or storage operation timed out",
                        "cannot start clipboard timeout monitor",
                    ],
                )),
            );
        }
        if events.len() == MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(
            json!({"timestamp": timestamp, "level": level, "category": category, "fields": fields}),
        );
    }
    json!({"available": true, "tail_truncated": truncated, "bytes_examined": bytes.len(), "counts": counts, "events": events})
}

fn write_report(directory: &Path, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    if bytes.len() > EXPORT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Diagnostics exceed size limit",
        ));
    }
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f");
    for suffix in 0..100 {
        let path = directory.join(format!("macrdp-diagnostics-{timestamp}-{suffix}.json"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        return Ok(path);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "Could not create a unique diagnostics file",
    ))
}

#[cfg(test)]
mod tests;
