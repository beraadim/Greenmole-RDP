import { useState, useEffect } from "react";
import { useServerStatus } from "../hooks/useServerStatus";
import { useMetrics } from "../hooks/useMetrics";
import { useConnections } from "../hooks/useConnections";
import { useMetricHistory } from "../hooks/useFpsHistory";
import { sampleAverage } from "../lib/metricHistory";
import { api } from "../lib/ipc";
import StatusBar from "../components/StatusBar";
import ServerError from "../components/ServerError";
import TimingDetails from "../components/TimingDetails";
import MoveWindowsControl from "../components/MoveWindowsControl";
import RecoveryControl from "../components/RecoveryControl";
import MetricCard from "../components/MetricCard";
import { ChartPanel } from "../components/ChartPanel";
import { AreaChart } from "../components/AreaChart";
import ConnItem from "../components/ConnItem";
import EmptyState from "../components/EmptyState";
import type { InputDebugState, UiConfig } from "../lib/types";

function Dashboard() {
  const status = useServerStatus();
  const { metrics, stale, error } = useMetrics();
  const connections = useConnections();
  const [loading, setLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [config, setConfig] = useState<UiConfig | null>(null);
  const [inputDebug, setInputDebug] = useState<InputDebugState | null>(null);

  const metricsAvailable = status.running && metrics && !stale;
  const bitrateHistory = useMetricHistory(
    metricsAvailable ? metrics.bitrate_kbps / 1000 : null,
    metrics?.sample_id ?? 0, metrics?.timestamp_ms ?? 0,
  );
  const encodeHistory = useMetricHistory(
    metricsAvailable ? metrics.encode_ms : null,
    metrics?.sample_id ?? 0, metrics?.timestamp_ms ?? 0,
  );
  const port = status.port ?? config?.port ?? 3389;

  useEffect(() => {
    api.getConfig().then(setConfig).catch(console.error);
  }, []);

  useEffect(() => {
    let cancelled = false;

    const poll = async () => {
      try {
        const debug = await api.getInputDebugState();
        if (!cancelled) {
          setInputDebug(debug);
        }
      } catch (error) {
        if (!cancelled) {
          console.error("Failed to fetch input debug state:", error);
        }
      }
    };

    poll();
    const interval = setInterval(poll, 1000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  // Clear loading when backend state actually changes
  useEffect(() => {
    if (status.running) setActionError(null);
    if (loading && (status.state === "running" || status.state.startsWith("error"))) {
      setLoading(false);
    }
    if (loading && status.state === "stopped" && !status.running) {
      // stopServer completed
      const timer = setTimeout(() => setLoading(false), 500);
      return () => clearTimeout(timer);
    }
  }, [status.state, status.running, loading]);

  const handleStart = async () => {
    if (loading) return;
    setLoading(true);
    setActionError(null);
    try {
      await api.startServer();
    } catch (err: unknown) {
      setActionError(String(err));
    } finally { setLoading(false); }
  };

  const handleStop = async () => {
    if (loading && status.state !== "starting") return;
    setLoading(true);
    setActionError(null);
    try {
      await api.stopServer();
    } catch (err: unknown) {
      setActionError(String(err));
    } finally { setLoading(false); }
  };

  const chartsEmpty = !metricsAvailable || bitrateHistory.length === 0;

  const avgBitrate = !chartsEmpty && bitrateHistory.length > 0
    ? sampleAverage(bitrateHistory)?.toFixed(1)
    : null;

  const avgEncode = !chartsEmpty && encodeHistory.length > 0
    ? sampleAverage(encodeHistory)?.toFixed(1)
    : null;
  const currentTransport = metricsAvailable ? metrics.transport_mode : null;
  const currentEncoder = metricsAvailable ? metrics.encoder_mode : null;
  const currentBitrateMode = metricsAvailable ? metrics.bitrate_mode : null;

  return (
    <div className="flex flex-col gap-3 p-4 overflow-y-auto">
      <StatusBar
        state={status.state}
        loading={loading}
        port={port}
        uptimeSeconds={status.uptime_secs}
        errorMessage={status.state.startsWith("error:") ? status.state : undefined}
        onStart={handleStart}
        onStop={handleStop}
      />

      <ServerError message={actionError ?? status.error_details} />
      {metricsAvailable && connections.length > 0 && metrics.clipboard_disabled && (
        <p role="status" className="rounded-md bg-orange/10 p-3 text-xs text-orange">
          Clipboard is unavailable for this connection. Reconnect to restore clipboard sharing; the desktop remains connected.
        </p>
      )}

      <MoveWindowsControl />
      <RecoveryControl />

      <div className="flex gap-3">
        <ChartPanel
          title="Network throughput · last 60s"
          value={avgBitrate != null ? `Avg ${avgBitrate} Mbps` : undefined}
          valueColor="var(--color-green)"
          empty={chartsEmpty}
          emptyText="Waiting for data..."
        >
          <AreaChart data={bitrateHistory} color="var(--color-green)" />
        </ChartPanel>

        <ChartPanel
          title="Encode latency · last 60s"
          value={avgEncode != null ? `Avg ${avgEncode} ms` : undefined}
          valueColor="var(--color-orange)"
          empty={chartsEmpty}
          emptyText="Waiting for data..."
        >
          <AreaChart data={encodeHistory} color="var(--color-orange)" />
        </ChartPanel>
      </div>

      <div className="flex gap-2">
        <MetricCard
          value={metricsAvailable ? metrics.fps : null}
          label="Updates/s"
          color="blue"
        />
        <MetricCard
          value={metricsAvailable ? metrics.bitrate_kbps : null}
          label="Mbps"
          color="green"
          formatter={(v) => (v / 1000).toFixed(1)}
        />
        <MetricCard
          value={metricsAvailable ? metrics.encode_ms : null}
          label="Encode ms"
          color="orange"
          formatter={(v) => v.toFixed(1)}
        />
        <MetricCard
          value={metricsAvailable ? connections.length : null}
          label="Connections"
          color="purple"
        />
      </div>

      <div className="bg-card rounded-[8px] p-3">
        <div className="flex items-center justify-between gap-3 mb-2">
          <span className="text-[11px] text-text-muted font-medium">
            RDP graphics mode
          </span>
          <span className="rounded-full bg-accent/10 px-2 py-0.5 text-[10px] text-accent uppercase tracking-[0.12em]">
            {currentTransport ?? "inactive"}
          </span>
        </div>
        {metricsAvailable && connections.some(connection => connection.client_name.startsWith("Guacamole")) && (
          <p className="mb-3 text-[11px] text-text-muted">
            This is the connection to Guacamole. Check Guacamole's Display menu to confirm browser video is active.
          </p>
        )}
        {metricsAvailable && (
          <div className="grid grid-cols-2 md:grid-cols-4 gap-2 text-[11px] mb-3">
            <div>Capture queue: {metrics.capture_queue_ms.toFixed(1)} ms</div>
            <div>Encode delivery: {metrics.completion_queue_ms.toFixed(1)} ms</div>
            <div>Socket write: {metrics.write_ms.toFixed(1)} ms</div>
            <div>Capture requested: {metrics.requested_capture_frame_rate ?? "--"} fps</div>
            <div>Capture applied: {metrics.applied_frame_rate ?? "--"} fps</div>
            <div>Capture update failures: {metrics.capture_frame_rate_update_failures}</div>
            <div>Video target: {metrics.requested_encoder_bitrate === null ? "not active" : `${(metrics.requested_encoder_bitrate / 1_000_000).toFixed(1)} Mbps`}</div>
            <div>Video applied: {metrics.requested_encoder_bitrate === null ? "not active" : metrics.applied_encoder_bitrate === null ? "not confirmed" : `${(metrics.applied_encoder_bitrate / 1_000_000).toFixed(1)} Mbps`}</div>
            <div>Encoder update failures: {metrics.encoder_bitrate_update_failures}</div>
            {metrics.capture_frame_rate_retry_exhausted && <div role="status" className="text-amber-600">Capture rate change failed; reconnect to retry.</div>}
            <div>Frames combined: {metrics.capture_frames_coalesced}</div>
            <div>Audio queue: {metrics.audio_queued_ms.toFixed(0)} ms</div>
            <div>Last audio wait: {metrics.audio_queue_age_ms.toFixed(0)} ms</div>
            <div>Audio drops: {metrics.audio_dropped_frames}</div>
            <div>Stale audio drops: {metrics.audio_stale_frames}</div>
            <div>Audio resyncs: {metrics.audio_discontinuities}</div>
            <div title="Time from sending graphics to client decode acknowledgement; includes client processing.">
              Graphics ACK: {metrics.ack_suspended ? "not reported" : `${metrics.latency_ms.toFixed(1)} ms`}
            </div>
            <div>Pending ACKs: {metrics.ack_suspended ? "not reported" : metrics.pending_acks}</div>
            <div>Client queue: {metrics.client_queue_depth_bytes === null ? "not reported" : `${Math.round(metrics.client_queue_depth_bytes / 1024)} KiB`}</div>
            {metrics.bitmap_codec && <div>Bitmap codec: {metrics.bitmap_codec === "remotefx" ? "RemoteFX (lossy)" : metrics.bitmap_codec === "rdp6-rle" ? "Full-color RGB (lossless)" : metrics.bitmap_codec}</div>}
            {metrics.negotiated_client_depth !== null && <div>Client depth request: {metrics.negotiated_client_depth} bits</div>}
          </div>
        )}
        {metricsAvailable && <TimingDetails metrics={metrics} />}
        <div className="grid grid-cols-1 md:grid-cols-3 gap-2 text-[11px]">
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Transport</div>
            <div className="text-text">{currentTransport ?? "--"}</div>
          </div>
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Encoding</div>
            <div className="text-text">{currentEncoder ?? "--"}</div>
          </div>
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Bitrate meaning</div>
            <div className="text-text">{currentBitrateMode ?? "--"}</div>
          </div>
        </div>
      </div>

      <div className="bg-card rounded-[8px] p-3">
        <div className="flex items-center justify-between gap-3 mb-2">
          <span className="text-[11px] text-text-muted font-medium">
            Keyboard modifiers
          </span>
          <span className="rounded-full bg-blue/10 px-2 py-0.5 text-[10px] text-blue uppercase tracking-[0.12em]">
            {inputDebug?.translation_profile ?? "none"}
          </span>
        </div>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-2 text-[11px]">
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Active modifiers</div>
            <div className="text-text">
              {inputDebug?.active_modifiers?.length
                ? inputDebug.active_modifiers.join(", ")
                : "none"}
            </div>
          </div>
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Suppressed companions</div>
            <div className="text-text">
              {inputDebug?.suppressed_modifiers?.length
                ? inputDebug.suppressed_modifiers.join(", ")
                : "none"}
            </div>
          </div>
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Native host modifiers</div>
            <div className="text-text">
              {inputDebug?.native_modifiers?.length
                ? `${inputDebug.native_modifiers.join(", ")} (${inputDebug.native_flags_hex})`
                : inputDebug?.native_flags_hex ?? "none"}
            </div>
          </div>
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Last event</div>
            <div className="text-text break-all">{inputDebug?.last_event ?? "--"}</div>
          </div>
          <div className="rounded-[6px] bg-black/10 px-2 py-2">
            <div className="text-text-muted mb-1">Last translated key</div>
            <div className="text-text">
              {inputDebug?.last_translation ?? inputDebug?.pending_dead_key ?? "--"}
            </div>
          </div>
        </div>
      </div>

      {status.running && !metricsAvailable && error && (
        <div className="bg-card rounded-[8px] px-3 py-2 text-[11px] text-orange">
          Metrics fetch error: {error}
        </div>
      )}

      <div className="bg-card rounded-[8px] p-3">
        <div className="flex items-center gap-2 mb-2">
          <span className="text-[11px] text-text-muted font-medium">
            Active connections
          </span>
          <span className="text-accent bg-accent/10 rounded-full px-2 text-[10px]">
            {connections.length}
          </span>
        </div>
        {connections.length > 0 ? (
          <div className="flex flex-col gap-1">
            {connections.map((c) => (
              <ConnItem
                key={`${c.client_ip}-${c.connected_at}`}
                connection={c}
              />
            ))}
          </div>
        ) : (
          <EmptyState message="No active connections" />
        )}
      </div>
    </div>
  );
}

export default Dashboard;
