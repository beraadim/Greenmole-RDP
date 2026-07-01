import { useState, useEffect } from "react";
import { useServerStatus } from "../hooks/useServerStatus";
import { useMetrics } from "../hooks/useMetrics";
import { useConnections } from "../hooks/useConnections";
import { useMetricHistory } from "../hooks/useFpsHistory";
import { api } from "../lib/ipc";
import StatusBar from "../components/StatusBar";
import MetricCard from "../components/MetricCard";
import { ChartPanel } from "../components/ChartPanel";
import { AreaChart } from "../components/AreaChart";
import ConnItem from "../components/ConnItem";
import EmptyState from "../components/EmptyState";
import type { UiConfig } from "../lib/types";

function Dashboard() {
  const status = useServerStatus();
  const { metrics, stale, error } = useMetrics();
  const connections = useConnections();
  const [loading, setLoading] = useState(false);
  const [config, setConfig] = useState<UiConfig | null>(null);

  const metricsAvailable = status.running && metrics && !stale;
  const bitrateHistory = useMetricHistory(
    metricsAvailable ? metrics.bitrate_kbps / 1000 : null
  );
  const encodeHistory = useMetricHistory(
    metricsAvailable ? metrics.encode_ms : null
  );
  const port = status.port ?? config?.port ?? 3389;

  useEffect(() => {
    api.getConfig().then(setConfig).catch(console.error);
  }, []);

  // Clear loading when backend state actually changes
  useEffect(() => {
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
    try {
      await api.startServer();
    } catch (err: unknown) {
      console.error("Failed to start server:", err);
      setLoading(false);
    }
  };

  const handleStop = async () => {
    if (loading) return;
    setLoading(true);
    try {
      await api.stopServer();
    } catch (err: unknown) {
      console.error("Failed to stop server:", err);
      setLoading(false);
    }
  };

  const chartsEmpty = !metricsAvailable || bitrateHistory.length === 0;

  const avgBitrate = !chartsEmpty && bitrateHistory.length > 0
    ? (bitrateHistory.reduce((a, b) => a + b, 0) / bitrateHistory.length).toFixed(1)
    : null;

  const avgEncode = !chartsEmpty && encodeHistory.length > 0
    ? (encodeHistory.reduce((a, b) => a + b, 0) / encodeHistory.length).toFixed(1)
    : null;

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

      <div className="flex gap-3">
        <ChartPanel
          title="Network throughput (Mbps)"
          value={avgBitrate != null ? `${avgBitrate} Mbps` : undefined}
          valueColor="var(--color-green)"
          empty={chartsEmpty}
          emptyText="Waiting for data..."
        >
          <AreaChart data={bitrateHistory} color="var(--color-green)" />
        </ChartPanel>

        <ChartPanel
          title="Encode latency (ms)"
          value={avgEncode != null ? `${avgEncode} ms` : undefined}
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
          label="FPS"
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
