import { useEffect, useId, useRef, useState } from "react";
import { Loader2, PanelsTopLeft } from "lucide-react";
import { Button } from "./ui/button";
import { api } from "../lib/ipc";
import type { WindowMoveResult, WindowMoveStatus } from "../lib/types";

function resultMessage(result: WindowMoveResult): string {
  const messages = [
    `${result.moved} ${result.moved === 1 ? "window moved" : "windows moved"} to the remote display.`,
  ];
  if (result.already_visible > 0) {
    messages.push(`${result.already_visible} already visible.`);
  }
  if (result.skipped > 0) {
    messages.push(`${result.skipped} skipped (minimized, full screen, or unavailable).`);
  }
  if (result.failed > 0) {
    messages.push(`${result.failed} ${result.failed === 1 ? "window" : "windows"} could not be moved.`);
  }
  if (result.app_failures > 0) {
    messages.push(`${result.app_failures} ${result.app_failures === 1 ? "app" : "apps"} could not be inspected.`);
  }
  if (result.timed_out) {
    messages.push("The time limit was reached before all windows were checked. You can try again.");
  }
  return messages.join(" ");
}

export default function MoveWindowsControl() {
  const descriptionId = useId();
  const availabilityId = useId();
  const [status, setStatus] = useState<WindowMoveStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState<{ text: string; error: boolean } | null>(null);
  const pending = useRef(false);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    // Schedule the next request after this one finishes so slow native status
    // checks cannot accumulate. The backend checks the active display again
    // when moving, since a client may disconnect between polls.
    const poll = async () => {
      try {
        const next = await api.getWindowMoveStatus();
        if (!cancelled) setStatus(next);
      } catch {
        if (!cancelled) {
          setStatus({ available: false, reason: "Unable to check the remote display. Retrying…" });
        }
      }
      if (!cancelled) timer = setTimeout(poll, 2000);
    };
    void poll();
    return () => {
      cancelled = true;
      mounted.current = false;
      if (timer) clearTimeout(timer);
    };
  }, []);

  const moveWindows = async () => {
    if (pending.current || !status?.available) return;
    pending.current = true;
    setBusy(true);
    setFeedback(null);
    try {
      const result = await api.moveWindowsToRemote();
      if (mounted.current) {
        setFeedback({
          text: resultMessage(result),
          error: result.failed > 0 || result.app_failures > 0 || result.timed_out,
        });
      }
    } catch (error: unknown) {
      if (mounted.current) {
        const reason = error instanceof Error ? error.message : String(error);
        setFeedback({ text: `Unable to move windows. ${reason}`, error: true });
      }
    } finally {
      pending.current = false;
      if (mounted.current) setBusy(false);
    }
  };

  return (
    <div className="rounded-[8px] bg-card p-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="text-xs font-medium text-text">Remote windows</div>
          <p id={descriptionId} className="mt-1 text-[11px] text-text-muted">
            Bring open windows from other displays onto the remote display. Minimized and full screen windows may be skipped.
          </p>
        </div>
        <Button
          size="sm"
          onClick={moveWindows}
          disabled={busy || !status?.available}
          aria-busy={busy}
          aria-describedby={`${descriptionId} ${availabilityId}`}
        >
          {busy ? <Loader2 className="animate-spin" aria-hidden="true" /> : <PanelsTopLeft aria-hidden="true" />}
          {busy ? "Moving windows…" : "Move windows to remote display"}
        </Button>
      </div>
      <p id={availabilityId} className="mt-1 text-[11px] text-text-muted">
        {!status ? "Checking remote display…" : status.available ? null : status.reason ?? "Connect using a virtual display to move windows."}
      </p>
      {feedback && (
        <p
          role={feedback.error ? "alert" : "status"}
          className={`mt-2 text-xs ${feedback.error ? "text-orange" : "text-green"}`}
        >
          {feedback.text}
        </p>
      )}
    </div>
  );
}
