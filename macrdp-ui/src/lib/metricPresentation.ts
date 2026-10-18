import type { Metrics } from "./types";

/** A fresh server heartbeat can contain an idle snapshot rather than frame work. */
export function hasLiveSessionMetrics(
  metrics: Metrics | null,
  serverRunning: boolean,
  connectionCount: number,
  stale: boolean,
): metrics is Metrics {
  // The connection list is also required because disconnect events can arrive
  // before the next metrics tick. Older servers do not report session_active.
  return serverRunning && connectionCount > 0 && !stale && metrics !== null &&
    metrics.session_active !== false;
}
