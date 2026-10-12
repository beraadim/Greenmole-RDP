import { useState } from "react";
import { Download, Loader2 } from "lucide-react";
import { api } from "../lib/ipc";
import { Button } from "./ui/button";

export default function DiagnosticsExport() {
  const [busy, setBusy] = useState(false);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setSavedPath(null);
    try {
      const result = await api.exportDiagnostics();
      setSavedPath(result.path);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="rounded-md border border-border bg-card p-3 space-y-2 text-xs">
      <div className="flex items-center justify-between gap-3">
        <p className="text-text-muted">Save recent timings and session outcomes to Desktop, including after stopping the service. Credentials and personal content are excluded.</p>
        <Button size="sm" variant="outline" className="shrink-0" disabled={busy} onClick={() => void save()}>
          {busy ? <Loader2 className="mr-1 h-3 w-3 animate-spin" /> : <Download className="mr-1 h-3 w-3" />}
          {busy ? "Saving..." : "Export diagnostics"}
        </Button>
      </div>
      {savedPath && <p role="status" className="break-all text-green">Saved: {savedPath}</p>}
      {error && <p role="alert" className="break-words text-red">{error}</p>}
    </div>
  );
}
