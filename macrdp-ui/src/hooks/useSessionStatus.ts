import { useEffect, useState } from "react";
import { getSessionStatus, type SessionStatus } from "../lib/sessionStatus";

/** Polls bounded in-memory evidence only; never probes or wakes native resources. */
export function useSessionStatus() {
  const [snapshot, setSnapshot] = useState<SessionStatus>({ current: null, recent: [] });
  const [unavailable, setUnavailable] = useState(false);
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      if (!document.hidden) {
        try {
          const next = await getSessionStatus();
          if (!cancelled) { setSnapshot(next); setUnavailable(false); }
        } catch {
          if (!cancelled) setUnavailable(true);
        }
      }
      if (!cancelled) timer = setTimeout(poll, 1000);
    };
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, []);
  return { snapshot, unavailable };
}
