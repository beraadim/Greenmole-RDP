import { invoke } from "@tauri-apps/api/core";

export type CleanupEvidence = "unconfirmed" | "pending" | "complete" | "failed" | "not_required";
export interface SessionCleanup {
  capture: CleanupEvidence;
  display: CleanupEvidence;
  native_encoder: CleanupEvidence;
  wake: CleanupEvidence;
  private_guard: CleanupEvidence;
  display_disposition: "unconfirmed" | "removed" | "retained_disabled";
  verified: boolean;
  observation_limited: boolean;
}
export interface SessionSummary {
  sequence: number;
  started_at_ms: number;
  ended_at_ms: number | null;
  phase: "preparing_desktop" | "waiting_encoder" | "streaming" | "restoring_desktop" | "ended";
  host_transport: string;
  encoder: string;
  outcome: string;
  cleanup: SessionCleanup;
}
export interface SessionStatus {
  current: SessionSummary | null;
  recent: SessionSummary[];
}
export const getSessionStatus = () => invoke<SessionStatus>("get_session_status");

export function sessionPhaseLabel(phase: SessionSummary["phase"]): string {
  switch (phase) {
    case "preparing_desktop": return "Preparing desktop";
    case "waiting_encoder": return "Waiting for encoder";
    case "streaming": return "Streaming from host";
    case "restoring_desktop": return "Restoring local desktop";
    case "ended": return "Session ended";
  }
}
export function cleanupEvidenceLabel(evidence: CleanupEvidence): string {
  switch (evidence) {
    case "complete": return "Confirmed";
    case "not_required": return "Not needed";
    case "pending": return "Pending";
    case "failed": return "Failed";
    case "unconfirmed": return "Not confirmed";
  }
}
export function sessionOutcomeLabel(outcome: string): string {
  const labels: Record<string, string> = {
    client_disconnect: "Client disconnected", peer_closed: "Peer closed the connection",
    peer_reset: "Connection reset by peer", peer_eof: "Peer connection ended",
    protocol_error: "Protocol error", transport_error: "Network transport error",
    transport_timeout: "Network transport timeout", write_timeout: "Network write timeout",
    idle_timeout: "Idle timeout", server_shutdown: "Server stopped", server_stopped: "Server stopped",
    server_restarted: "Server restarted", superseded: "Replaced by a new session",
    credentials_changed: "Credentials changed", handshake_timeout: "Handshake timeout",
    handshake_rejected: "Handshake rejected", authentication_failed: "Authentication failed",
    activation_failed: "Session activation failed", admission_throttled: "Connection admission limited",
    capture_ended: "Screen capture ended", capture_error: "Screen capture error",
    output_overflow: "Output queue overflow",
  };
  return labels[outcome] ?? "Reason not confirmed";
}
