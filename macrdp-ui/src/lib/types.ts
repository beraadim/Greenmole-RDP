export interface ServerStatus {
  running: boolean;
  state: string;
  error_details?: string | null;
  uptime_secs: number;
  port?: number | null;
  bind_address?: string | null;
}

export interface TimingPercentiles {
  samples: number;
  p50_ms: number | null;
  p95_ms: number | null;
  p99_ms: number | null;
  max_ms: number | null;
}

export interface Metrics {
  fps: number; // Delivered updates/s; bitmap regions count individually.
  bitrate_kbps: number;
  latency_ms: number;  // JSON field name (backend uses serde rename from rtt_ms)
  bytes_sent: number;
  timestamp: number;
  // New fields
  encode_ms: number;
  capture_queue_ms: number;
  completion_queue_ms: number;
  write_ms: number;
  capture_frames_coalesced: number;
  client_queue_depth_bytes: number | null;
  ack_suspended: boolean;
  net_ms: number;
  last_frame_bytes: number;
  network_quality: number;
  pending_acks: number;
  transport_mode: string;
  encoder_mode: string;
  bitrate_mode: string;
  audio_dropped_frames: number;
  audio_stale_frames: number;
  audio_discontinuities: number;
  audio_invalid_frames: number;
  audio_queue_age_ms: number;
  audio_queued_ms: number;
  capture_timings: TimingPercentiles;
  encode_timings: TimingPercentiles;
  completion_timings: TimingPercentiles;
  write_timings: TimingPercentiles;
  connection_outcome: string | null;
}

export interface Connection {
  client_ip: string;
  client_name: string;
  connected_at: string;
  bytes_total: number;
}

export interface LogEntry {
  level: "trace" | "debug" | "info" | "warn" | "error";
  message: string;
  timestamp: string;
}

export interface PermissionStatus {
  screen_capture: boolean;
  accessibility: boolean;
  microphone: boolean;
}

export interface DisplayGeometry {
  display_id: number;
  origin_x: number;
  origin_y: number;
  logical_width: number;
  logical_height: number;
  pixel_width: number;
  pixel_height: number;
}

export interface WindowMoveStatus {
  available: boolean;
  reason: string | null;
}

export interface WindowMoveResult {
  moved: number;
  already_visible: number;
  skipped: number;
  failed: number;
  app_failures: number;
  timed_out: boolean;
  target: DisplayGeometry;
}

export interface InputDebugState {
  active_modifiers: string[];
  suppressed_modifiers: string[];
  native_modifiers: string[];
  native_flags_hex: string;
  translation_profile: string;
  pending_dead_key: string | null;
  last_event: string | null;
  last_translation: string | null;
}

export interface ConnectionHistory {
  id: number;
  client_ip: string;
  client_name: string;
  connected_at: string;
  disconnected_at: string;
  duration_secs: number;
  bytes_total: number;
}

export interface TrafficStats {
  date: string;
  bytes_sent: number;
  connection_count: number;
}

export interface UiConfig {
  port: number;
  frame_rate: number;
  bitrate_mbps: number;
  encoder: string;
  chroma_mode: string;
  bind_address: string;
  max_connections: number;
  idle_timeout_secs: number;
  username: string;
  password: string;
  display_backend: "main" | "virtual";
  resolution: string;
  show_cursor: boolean;
  audio_enabled: boolean;
  clipboard_enabled: boolean;
  clipboard_file_transfer: boolean;
  clipboard_max_file_size_mb: number;
  log_level: string;
  theme: string;
  autostart: boolean;
  tls_cert_path: string;
  tls_key_path: string;
  tls_server_names: string[];
}

export interface TlsStatus {
  source: "running" | "configured";
  error: string | null;
  certificate: {
    fingerprint_sha256: string;
    expires_at: string;
    not_before_unix: number;
    not_after_unix: number;
    server_names: string[];
  } | null;
}

/** @deprecated Use UiConfig instead */
export type ServerConfig = UiConfig;
