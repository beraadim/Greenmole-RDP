import { useState, useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/ipc";
import type { Metrics } from "../lib/types";
import { MetricSampleGate } from "../lib/metricStream";

const STALE_TIMEOUT = 5000; // 5s without update → stale
const POLL_INTERVAL = 3000; // 3s fallback polling

export function useMetrics() {
  const [metrics, setMetrics] = useState<Metrics | null>(null);
  const [stale, setStale] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined);
  const mountedRef = useRef(true);
  const sampleGate = useRef(new MetricSampleGate());

  const resetStaleTimer = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => {
      if (mountedRef.current) setStale(true);
    }, STALE_TIMEOUT);
  }, []);

  const acceptMetrics = useCallback((m: Metrics, allowReset = false) => {
    if (!mountedRef.current) return;
    if (m.sample_id === 0 && allowReset) {
      sampleGate.current.reset();
      setMetrics(null);
      setStale(false);
      setError(null);
      if (timerRef.current) clearTimeout(timerRef.current);
      return;
    }
    // A cached poll is not a heartbeat. Ignore duplicates and late poll results.
    if (!sampleGate.current.admit(m.sample_id)) return;
    setMetrics(m);
    setStale(false);
    setError(null);
    resetStaleTimer();
  }, [resetStaleTimer]);

  const fetch = useCallback(() => {
    const requestEpoch = sampleGate.current.epoch;
    api
      .getMetrics()
      .then((m) => {
        if (m && requestEpoch === sampleGate.current.epoch) acceptMetrics(m);
      })
      .catch((err: unknown) => {
        if (mountedRef.current && requestEpoch === sampleGate.current.epoch) {
          setError(err instanceof Error ? err.message : String(err));
        }
      });
  }, [acceptMetrics]);

  useEffect(() => {
    mountedRef.current = true;

    // Initial fetch
    fetch();

    // Event-driven updates (primary)
    const unlisten = listen<Metrics>("metrics", (event) => {
      acceptMetrics(event.payload, true);
    });

    // Polling fallback (secondary)
    const poll = setInterval(fetch, POLL_INTERVAL);

    return () => {
      mountedRef.current = false;
      sampleGate.current.reset();
      void unlisten.then((fn) => fn()).catch(() => {});
      clearInterval(poll);
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [fetch, acceptMetrics]);

  return { metrics, stale, error };
}
