export interface MetricPoint {
  sampleId: number;
  timestampMs: number;
  value: number;
}

export const HISTORY_WINDOW_MS = 60_000;

/** Each backend sample is admitted once, including unchanged and zero values. */
export function recordMetricSample(
  history: MetricPoint[],
  point: MetricPoint | null,
): MetricPoint[] {
  if (point === null) return history.length ? [] : history;
  if (!Number.isFinite(point.value) || !Number.isFinite(point.timestampMs) ||
      !Number.isSafeInteger(point.sampleId) || point.sampleId <= 0) {
    return history;
  }
  const last = history[history.length - 1];
  if (last && point.sampleId <= last.sampleId) return history;
  // A wall-clock correction starts a new chart rather than reversing its axis.
  const previous = last && point.timestampMs < last.timestampMs ? [] : history;
  const cutoff = point.timestampMs - HISTORY_WINDOW_MS;
  return [...previous.filter(p => p.timestampMs >= cutoff), point].slice(-120);
}

export function sampleAverage(history: readonly MetricPoint[]): number | null {
  return history.length ? history.reduce((sum, p) => sum + p.value, 0) / history.length : null;
}
