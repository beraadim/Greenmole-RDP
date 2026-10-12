import { useState, useEffect } from "react";
import { recordMetricSample, type MetricPoint } from "../lib/metricHistory";

export function useMetricHistory(currentValue: number | null, sampleId: number, timestampMs: number): MetricPoint[] {
  const [history, setHistory] = useState<MetricPoint[]>([]);

  useEffect(() => {
    setHistory(previous => recordMetricSample(previous, currentValue === null ? null : {
      sampleId, timestampMs, value: currentValue,
    }));
  }, [currentValue, sampleId, timestampMs]);

  return history;
}
