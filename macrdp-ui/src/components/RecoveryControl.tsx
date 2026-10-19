import { useState } from "react";
import { Loader2, Monitor } from "lucide-react";
import { api } from "../lib/ipc";

/** Recovery must remain available even when ordinary Stop is still pending. */
export default function RecoveryControl() {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  const recover = async () => {
    if (busy) return;
    setBusy(true);
    setMessage(null);
    setFailed(false);
    try {
      await api.recoverLocalDesktop();
      setMessage("The server is stopped. Local recovery is complete; guarded sessions remain locked.");
    } catch (error) {
      setFailed(true);
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="rounded-lg bg-card p-3 text-xs">
      <div className="flex items-center justify-between gap-3">
        <p className="text-text-muted">Stop the server and recover this Mac. A guarded private session is locked before local control returns.</p>
        <button
          type="button"
          onClick={recover}
          disabled={busy}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-md bg-white/10 px-3 py-1.5 font-medium hover:bg-white/20 disabled:opacity-50"
        >
          {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Monitor className="h-3.5 w-3.5" />}
          {busy ? "Recovering…" : "Recover local desktop"}
        </button>
      </div>
      {message && <p role={failed ? "alert" : "status"} className={`mt-2 ${failed ? "text-red" : "text-text-muted"}`}>{message}</p>}
    </div>
  );
}
