import { useState } from "react";
import {
  Settings,
  LogOut,
  FileText,
  Play,
  Square,
  Monitor,
} from "lucide-react";
import { useServerStatus } from "../hooks/useServerStatus";
import { useMetrics } from "../hooks/useMetrics";
import { useConnections } from "../hooks/useConnections";
import { api } from "../lib/ipc";
import ServerError from "../components/ServerError";
import { formatBytes, formatDuration } from "../lib/format";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";

const statusDotColor: Record<string, string> = {
  running: "bg-green",
  starting: "bg-accent",
  stopping: "bg-accent",
  cleanup_pending: "bg-accent",
  stopped: "bg-muted-foreground",
  error: "bg-destructive",
};

function Popover() {
  const status = useServerStatus();
  const { metrics, stale, error } = useMetrics();
  const connections = useConnections();
  const [toggling, setToggling] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const metricsAvailable = status.running && metrics && !stale;
  const cleanupPending = status.state === "stopping" || status.state === "cleanup_pending";

  const handleToggle = async () => {
    setToggling(true);
    setActionError(null);
    try {
      if (status.running || cleanupPending || status.state === "starting") {
        await api.stopServer();
      } else {
        await api.startServer();
      }
    } catch (err) {
      setActionError(String(err));
    } finally {
      setToggling(false);
    }
  };

  const handleQuit = () => {
    if (window.confirm("Quit macrdp?")) {
      api.quitApp();
    }
  };

  const handleShowMain = () => {
    api.showMainWindow();
  };

  const handleShowLogs = () => {
    api.showMainWindow();
  };

  const displayedConnections = connections.slice(0, 3);
  const extraCount = connections.length - 3;

  return (
    <div className="min-h-screen bg-[rgba(245,245,247,0.95)] backdrop-blur-xl rounded-xl overflow-hidden shadow-2xl">
      <div className="flex flex-col p-3.5 gap-3">
        <ServerError message={!status.running ? actionError ?? status.error_details : null} />
        {metricsAvailable && connections.length > 0 && metrics.clipboard_disabled && <p role="status" className="text-xs text-orange">Clipboard unavailable. Reconnect to restore sharing.</p>}
        {/* Header row */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <div
              className={`h-2 w-2 rounded-full ${statusDotColor[status.state.startsWith("error") ? "error" : status.state] ?? "bg-muted-foreground"}`}
            />
            <span className="text-sm font-semibold text-foreground">
              macrdp
            </span>
            {status.running && status.uptime_secs > 0 && (
              <span className="text-xs text-muted-foreground">
                {formatDuration(status.uptime_secs)}
              </span>
            )}
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon-xs"
              onClick={handleShowMain}
              title="Settings"
            >
              <Settings className="h-3.5 w-3.5" />
            </Button>
            <Button
              variant="destructive"
              size="icon-xs"
              onClick={handleQuit}
              title="Quit"
            >
              <LogOut className="h-3.5 w-3.5" />
            </Button>
          </div>
        </div>

        {/* Metrics row - 6 columns */}
        <div className="grid grid-cols-6 gap-1">
          <div className="flex flex-col items-center justify-center rounded-md bg-muted/50 py-2 px-0.5">
            <span className="text-sm font-semibold text-foreground">
              {metricsAvailable ? metrics.fps : "--"}
            </span>
            <span className="text-[10px] text-muted-foreground">Updates/s</span>
          </div>
          <div className="flex flex-col items-center justify-center rounded-md bg-muted/50 py-2 px-0.5">
            <span className="text-sm font-semibold text-foreground">
              {metricsAvailable
                ? (metrics.bitrate_kbps / 1000).toFixed(1)
                : "--"}
            </span>
            <span className="text-[10px] text-muted-foreground">M</span>
          </div>
          <div className="flex flex-col items-center justify-center rounded-md bg-muted/50 py-2 px-0.5">
            <span className="text-sm font-semibold text-foreground">
              {metricsAvailable && !metrics.ack_suspended ? metrics.latency_ms.toFixed(1) : "--"}
            </span>
            <span className="text-[10px] text-muted-foreground">ACK ms</span>
          </div>
          <div className="flex flex-col items-center justify-center rounded-md bg-muted/50 py-2 px-0.5">
            <span className="text-sm font-semibold text-foreground">
              {metricsAvailable ? metrics.encode_ms : "--"}
            </span>
            <span className="text-[10px] text-muted-foreground">Encode</span>
          </div>
          <div className="flex flex-col items-center justify-center rounded-md bg-muted/50 py-2 px-0.5">
            <span className="text-sm font-semibold text-foreground">
              {metricsAvailable ? metrics.write_ms.toFixed(1) : "--"}
            </span>
            <span className="text-[10px] text-muted-foreground">Write ms</span>
          </div>
          <div className="flex flex-col items-center justify-center rounded-md bg-muted/50 py-2 px-0.5">
            <span className="text-sm font-semibold text-foreground">
              {metricsAvailable ? formatBytes(metrics.bytes_sent) : "--"}
            </span>
            <span className="text-[10px] text-muted-foreground">Traffic</span>
          </div>
        </div>

        {status.running && !metricsAvailable && error && (
          <div className="rounded-md bg-orange/10 px-2 py-1 text-[10px] text-orange">
            Metrics fetch error: {error}
          </div>
        )}

        {/* Connection list */}
        <div>
          <div className="mb-1.5 text-xs text-muted-foreground">
            Connections ({connections.length})
          </div>
          {connections.length === 0 ? (
            <div className="text-center text-xs text-muted-foreground py-2">
              No connections
            </div>
          ) : (
            <div className="space-y-1">
              {displayedConnections.map((conn) => {
                const connectedAt = new Date(conn.connected_at);
                const durationSecs = Math.floor(
                  (Date.now() - connectedAt.getTime()) / 1000,
                );
                return (
                  <div
                    key={`${conn.client_ip}-${conn.connected_at}`}
                    className="flex items-center justify-between text-xs px-2 py-1.5 rounded-md bg-muted/30"
                  >
                    <div className="flex items-center gap-1.5 min-w-0">
                      <div className="h-1.5 w-1.5 shrink-0 rounded-full bg-green" />
                      <span className="truncate text-foreground font-medium">
                        {conn.client_name || conn.client_ip}
                      </span>
                      <span className="shrink-0 text-muted-foreground">
                        {formatDuration(durationSecs)}
                      </span>
                    </div>
                    <span className="shrink-0 ml-2 text-muted-foreground">
                      {formatBytes(conn.bytes_total)}
                    </span>
                  </div>
                );
              })}
              {extraCount > 0 && (
                <button
                  type="button"
                  onClick={handleShowMain}
                  className="w-full text-center text-xs text-primary hover:underline py-0.5"
                >
                  +{extraCount} more
                </button>
              )}
            </div>
          )}
        </div>

        <Separator />

        {/* Action buttons row */}
        <div className="grid grid-cols-3 gap-2">
          <Button
            variant={status.running ? "destructive" : "default"}
            size="sm"
            disabled={toggling && status.state !== "starting"}
            onClick={handleToggle}
            className="w-full"
          >
            {status.running || cleanupPending || status.state === "starting" ? (
              <><Square className="h-3 w-3" /> {cleanupPending ? "Check cleanup" : "Stop"}</>
            ) : (
              <><Play className="h-3 w-3" /> Start</>
            )}
          </Button>
          <Button
            size="sm"
            onClick={handleShowMain}
            className="w-full"
          >
            <Monitor className="h-3 w-3" />
            Main window
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={handleShowLogs}
            className="w-full"
          >
            <FileText className="h-3 w-3" />
            Logs
          </Button>
        </div>
      </div>
    </div>
  );
}

export default Popover;
