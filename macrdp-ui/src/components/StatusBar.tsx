import { useRef, useCallback } from "react";
import { Play, Square, Loader2 } from "lucide-react";

interface StatusBarProps {
  state: string;
  loading?: boolean;
  port?: number;
  uptimeSeconds?: number;
  errorMessage?: string;
  onStart: () => void;
  onStop: () => void;
}

function formatUptime(seconds: number): string {
  if (seconds < 60) return "< 1m";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

const statusConfig: Record<
  "running" | "starting" | "stopped" | "error",
  { dotClass: string; glowColor: string; label: string }
> = {
  running: {
    dotClass: "bg-green",
    glowColor: "0 0 6px var(--color-green)",
    label: "Running",
  },
  starting: {
    dotClass: "bg-yellow",
    glowColor: "0 0 6px var(--color-yellow)",
    label: "Starting...",
  },
  stopped: {
    dotClass: "bg-text-muted",
    glowColor: "none",
    label: "Stopped",
  },
  error: {
    dotClass: "bg-red",
    glowColor: "0 0 6px var(--color-red)",
    label: "Error",
  },
};

const DEBOUNCE_MS = 1500;

function StatusBar({
  state,
  loading = false,
  port,
  uptimeSeconds,
  errorMessage,
  onStart,
  onStop,
}: StatusBarProps) {
  const normalizedState = state.startsWith("error")
    ? "error"
    : (state as "running" | "starting" | "stopped" | "error");
  const config = statusConfig[normalizedState] ?? statusConfig.stopped;
  const isError = normalizedState === "error";
  const isLoading = loading || normalizedState === "starting";
  const showStop =
    normalizedState === "running" ||
    normalizedState === "starting" ||
    (loading && normalizedState !== "stopped");
  const buttonsDisabled = isLoading;
  const lastClickRef = useRef(0);

  const debounced = useCallback(
    (fn: () => void) => {
      const now = Date.now();
      if (now - lastClickRef.current < DEBOUNCE_MS) return;
      lastClickRef.current = now;
      fn();
    },
    []
  );

  const wrapperClass = isError
    ? "flex items-center h-9 bg-red/10 border border-red/20 rounded-[8px] px-3"
    : "flex items-center h-9 bg-card rounded-[8px] px-3";

  return (
    <div className={wrapperClass} role="status" aria-live="polite">
      <div className="flex items-center gap-2 flex-1 min-w-0">
        {isLoading ? (
          <Loader2 className="h-3.5 w-3.5 text-yellow animate-spin shrink-0" />
        ) : (
          <span
            className={`inline-block h-2 w-2 rounded-full shrink-0 ${config.dotClass}`}
            style={{ boxShadow: config.glowColor }}
          />
        )}
        <span className="text-sm font-medium truncate">
          {isError && errorMessage ? errorMessage : config.label}
        </span>
        {!isError && (
          <div className="flex items-center gap-2 text-xs text-text-muted">
            {port != null && <span>:{port}</span>}
            {state === "running" && uptimeSeconds != null && (
              <span>{formatUptime(uptimeSeconds)}</span>
            )}
          </div>
        )}
      </div>

      <div className="shrink-0 ml-2">
        {showStop ? (
          <button
            onClick={() => debounced(onStop)}
            disabled={buttonsDisabled}
            className="inline-flex items-center gap-1.5 rounded-md bg-red/10 px-2.5 py-1 text-xs font-medium text-red hover:bg-red/20 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isLoading ? (
              <Loader2 className="h-3 w-3 animate-spin" />
            ) : (
              <Square className="h-3 w-3" />
            )}
            {isLoading ? "Working..." : "Stop"}
          </button>
        ) : (
          <button
            onClick={() => debounced(onStart)}
            disabled={buttonsDisabled}
            className="inline-flex items-center gap-1.5 rounded-md bg-green/10 px-2.5 py-1 text-xs font-medium text-green hover:bg-green/20 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isLoading ? (
              <Loader2 className="h-3 w-3 animate-spin" />
            ) : (
              <Play className="h-3 w-3" />
            )}
            {isLoading ? "Working..." : "Start"}
          </button>
        )}
      </div>
    </div>
  );
}

export default StatusBar;
