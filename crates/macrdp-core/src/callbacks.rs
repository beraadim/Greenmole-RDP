//! Event callback types and ServerEventHandler trait

use serde::Serialize;

/// Server running status
#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub state: String,
    /// Detailed local failure cause; excluded from exported diagnostic bundles.
    pub error_details: Option<String>,
    pub uptime_secs: u64,
    pub port: Option<u16>,
    pub bind_address: Option<String>,
}

impl Default for ServerStatus {
    fn default() -> Self {
        Self {
            running: false,
            state: "stopped".to_string(),
            error_details: None,
            uptime_secs: 0,
            port: None,
            bind_address: None,
        }
    }
}

/// Event-level stage durations; absent values mean no measurements are available.
#[derive(Debug, Clone, Serialize, Default)]
pub struct TimingPercentiles {
    pub samples: usize,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub max_ms: Option<f64>,
}

impl From<ironrdp_server::gfx::TimingSummary> for TimingPercentiles {
    fn from(value: ironrdp_server::gfx::TimingSummary) -> Self {
        Self {
            samples: value.samples,
            p50_ms: value.p50_ms,
            p95_ms: value.p95_ms,
            p99_ms: value.p99_ms,
            max_ms: value.max_ms,
        }
    }
}

/// Independent windows: capture deliveries, encode completions and wire updates
/// can have different counts. These are stage durations, not end-to-end latency.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Metrics {
    /// Delivered graphics updates per second; bitmap updates may be subregions.
    pub fps: u32,
    pub bitrate_kbps: u64,
    /// Graphics frame send-to-decode-ACK latency, including transport and client decoding.
    /// Serialized as "latency_ms" for backward compatibility; this is not a network ping.
    #[serde(rename = "latency_ms")]
    pub rtt_ms: f64,
    pub bytes_sent: u64,
    pub timestamp: u64,
    /// Process-wide sample order, shared by events and polling snapshots.
    pub sample_id: u64,
    /// Wall-clock sample time in milliseconds; legacy timestamp remains seconds.
    pub timestamp_ms: u64,
    /// Time spent encoding the last frame (milliseconds)
    pub encode_ms: f64,
    pub capture_queue_ms: f64,
    pub completion_queue_ms: f64,
    pub write_ms: f64,
    pub capture_frames_coalesced: u64,
    /// Adaptive native capture target, distinct from delivered update rate.
    pub requested_capture_frame_rate: Option<u32>,
    /// Last rate confirmed by the native capture API.
    pub applied_frame_rate: Option<u32>,
    /// Failed frame-rate configuration attempts for the current capture owner.
    pub capture_frame_rate_update_failures: u64,
    pub capture_frame_rate_retry_exhausted: bool,
    pub requested_encoder_bitrate: Option<u32>,
    pub applied_encoder_bitrate: Option<u32>,
    pub encoder_bitrate_update_failures: u64,
    pub bitmap_codec: Option<String>,
    /// Client's preferred bitmap depth; our emitted RGB bitmap format is 32bpp.
    pub negotiated_client_depth: Option<u16>,
    pub client_queue_depth_bytes: Option<u32>,
    pub ack_suspended: bool,
    /// Clipboard was quarantined for this connection; display/input remain active.
    pub clipboard_disabled: bool,
    /// Legacy alias of ACK latency. Network-only latency is not measured by GFX ACKs.
    pub net_ms: f64,
    /// Byte size of the last encoded frame
    pub last_frame_bytes: u32,
    /// Smoothed network quality score in [0.0, 1.0]
    pub network_quality: f32,
    /// Number of GFX frames awaiting ACK from the client
    pub pending_acks: u32,
    /// Effective transport path for the active session.
    pub transport_mode: String,
    /// Instantiated backend and active graphics path, including fallback.
    pub encoder_mode: String,
    /// Human-readable note for how bitrate should be interpreted in this mode.
    pub bitrate_mode: String,
    pub audio_dropped_frames: u64,
    pub audio_stale_frames: u64,
    pub audio_discontinuities: u64,
    pub audio_invalid_frames: u64,
    pub audio_queue_age_ms: f64,
    pub audio_queued_ms: f64,
    pub capture_timings: TimingPercentiles,
    pub encode_timings: TimingPercentiles,
    pub completion_timings: TimingPercentiles,
    pub write_timings: TimingPercentiles,
    /// Fixed operational label, never a free-form error or client identifier.
    pub connection_outcome: Option<String>,
}

/// Connection event
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event")]
pub enum ConnectionEvent {
    #[serde(rename = "connected")]
    Connected(ConnectionInfo),
    #[serde(rename = "disconnected")]
    Disconnected(DisconnectionInfo),
}

/// Active client connection info
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionInfo {
    pub client_ip: String,
    pub client_name: String,
    pub connected_at: String,
}

/// Disconnection info
#[derive(Debug, Clone, Serialize, Default)]
pub struct DisconnectionInfo {
    pub client_ip: String,
    pub client_name: String,
    pub connected_at: String,
    pub disconnected_at: String,
    pub duration_secs: u64,
    pub bytes_total: u64,
    pub reason: String,
    pub final_metrics: Metrics,
}

/// A single log entry
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub level: String,
    pub message: String,
    pub timestamp: String,
}

/// macOS permission status
#[derive(Debug, Clone, Serialize, Default)]
pub struct PermissionStatus {
    pub screen_capture: bool,
    pub accessibility: bool,
    pub microphone: bool,
}

/// Lightweight keyboard/input diagnostics exposed to the UI.
#[derive(Debug, Clone, Serialize, Default)]
pub struct InputDebugState {
    pub active_modifiers: Vec<String>,
    pub suppressed_modifiers: Vec<String>,
    pub native_modifiers: Vec<String>,
    pub native_flags_hex: String,
    pub translation_profile: String,
    pub pending_dead_key: Option<String>,
    pub last_event: Option<String>,
    pub last_translation: Option<String>,
}

/// Hot-updatable configuration
#[derive(Clone)]
pub enum ConfigUpdate {
    FrameRate(u32),
    BitrateKbps(u32),
    LogLevel(String),
    ShowCursor(bool),
    Resolution(String),
    Encoder(String),
    ChromaMode(String),
    GfxValidationMode(bool),
    Credentials { username: String, password: String },
}

impl std::fmt::Debug for ConfigUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrameRate(value) => f.debug_tuple("FrameRate").field(value).finish(),
            Self::BitrateKbps(value) => f.debug_tuple("BitrateKbps").field(value).finish(),
            Self::LogLevel(value) => f.debug_tuple("LogLevel").field(value).finish(),
            Self::ShowCursor(value) => f.debug_tuple("ShowCursor").field(value).finish(),
            Self::Resolution(value) => f.debug_tuple("Resolution").field(value).finish(),
            Self::Encoder(value) => f.debug_tuple("Encoder").field(value).finish(),
            Self::ChromaMode(value) => f.debug_tuple("ChromaMode").field(value).finish(),
            Self::GfxValidationMode(value) => {
                f.debug_tuple("GfxValidationMode").field(value).finish()
            }
            Self::Credentials { .. } => f
                .debug_struct("Credentials")
                .field("password", &"[REDACTED]")
                .finish_non_exhaustive(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_updates_never_include_secret_values_in_debug_output() {
        let update = ConfigUpdate::Credentials {
            username: "test-user".into(),
            password: "test-secret".into(),
        };
        let output = format!("{update:?}");
        assert!(output.contains("REDACTED"));
        assert!(!output.contains("test-secret"));
        assert!(!output.contains("test-user"));
        assert_eq!(
            format!("{:?}", ConfigUpdate::FrameRate(60)),
            "FrameRate(60)"
        );
    }
}

impl ConfigUpdate {
    /// Validate public runtime updates before they reach the session worker.
    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::FrameRate(fps) => anyhow::ensure!(
                (1..=240).contains(fps),
                "frame_rate must be between 1 and 240"
            ),
            Self::BitrateKbps(kbps) => anyhow::ensure!(
                (1..=1_000_000).contains(kbps),
                "bitrate must be between 1 and 1000000 Kbps"
            ),
            Self::Credentials { username, password } => {
                anyhow::ensure!(
                    !username.trim().is_empty(),
                    "A nonempty RDP username is required"
                );
                anyhow::ensure!(!password.is_empty(), "A nonempty RDP password is required");
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod config_update_tests {
    use super::ConfigUpdate;

    #[test]
    fn hot_encoder_updates_reject_zero_and_overflowing_values() {
        for fps in [0, 241, u32::MAX] {
            assert!(ConfigUpdate::FrameRate(fps).validate().is_err());
        }
        for fps in [1, 240] {
            assert!(ConfigUpdate::FrameRate(fps).validate().is_ok());
        }
        for kbps in [0, 1_000_001, u32::MAX] {
            assert!(ConfigUpdate::BitrateKbps(kbps).validate().is_err());
        }
        for kbps in [1, 1_000_000] {
            assert!(ConfigUpdate::BitrateKbps(kbps).validate().is_ok());
        }
    }

    #[test]
    fn hot_credentials_require_both_values_without_disclosing_them() {
        for (username, password) in [(" ", "private-value"), ("user", "")] {
            let error = ConfigUpdate::Credentials {
                username: username.into(),
                password: password.into(),
            }
            .validate()
            .unwrap_err()
            .to_string();
            assert!(!error.contains("private-value"));
        }
        assert!(ConfigUpdate::Credentials {
            username: "user".into(),
            password: "private-value".into()
        }
        .validate()
        .is_ok());
    }
}

/// Callback trait for receiving server events.
/// All methods have default empty implementations so callers can override only what they need.
pub trait ServerEventHandler: Send + Sync + 'static {
    fn on_status_change(&self, _status: ServerStatus) {}
    fn on_metrics(&self, _metrics: Metrics) {}
    fn on_connection(&self, _event: ConnectionEvent) {}
    fn on_log(&self, _entry: LogEntry) {}
}
