import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/ipc";
import type { ServerStatus } from "../lib/types";

const POLL_INTERVAL = 5000; // 5s fallback polling

export function useServerStatus() {
  const [status, setStatus] = useState<ServerStatus>({
    running: false,
    state: "stopped",
    uptime_secs: 0,
    port: null,
    bind_address: null,
  });
  const mountedRef = useRef(true);
  const revisionRef = useRef(0);

  const fetch = useCallback(() => {
    const revision = ++revisionRef.current;
    api
      .getServerStatus()
      .then((s) => {
        if (mountedRef.current && revision === revisionRef.current) setStatus(s);
      })
      .catch(console.error);
  }, []);

  useEffect(() => {
    mountedRef.current = true;

    // Initial fetch
    fetch();

    // Event-driven updates (primary)
    const unlisten = listen<ServerStatus>("server-status", (event) => {
      revisionRef.current += 1;
      if (mountedRef.current) setStatus(event.payload);
    });

    // Polling fallback (secondary)
    const timer = setInterval(fetch, POLL_INTERVAL);

    return () => {
      mountedRef.current = false;
      revisionRef.current += 1;
      unlisten.then((fn) => fn());
      clearInterval(timer);
    };
  }, [fetch]);

  return status;
}
