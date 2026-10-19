import { useState, useEffect } from "react";
import { Link } from "react-router-dom";
import { Monitor, MousePointer2, Keyboard, Check } from "lucide-react";
import { api } from "../lib/ipc";
import type { PermissionStatus } from "../lib/types";
import { useServerStatus } from "../hooks/useServerStatus";

const permissionDefs = [
  {
    key: "screen_capture" as keyof PermissionStatus,
    name: "Screen Recording",
    pane: "screen_capture",
    icon: Monitor,
  },
  {
    key: "accessibility" as keyof PermissionStatus,
    name: "Accessibility",
    pane: "accessibility",
    icon: MousePointer2,
  },
  {
    key: "input_monitoring" as keyof PermissionStatus,
    name: "Input Monitoring",
    pane: "input_monitoring",
    icon: Keyboard,
    privateOnly: true,
  },
];

function Permissions() {
  const [perms, setPerms] = useState<PermissionStatus | null>(null);
  const [privateSession, setPrivateSession] = useState<boolean | null>(null);
  const status = useServerStatus();
  const [requesting, setRequesting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const serviceOwnsResources = status.running || status.state === "stopping" || status.state === "cleanup_pending";
  const request = async () => {
    if (requesting || serviceOwnsResources) return;
    setRequesting(true);
    setError(null);
    try { setPerms(await api.requestPermissions()); }
    catch (reason) { setError(String(reason)); }
    finally { setRequesting(false); }
  };

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const [permissions, config] = await Promise.all([api.getPermissions(), api.getConfig()]);
        if (!cancelled) {
          setPerms(permissions);
          setPrivateSession(config.private_session);
        }
      } catch (reason) {
        if (!cancelled) setError(String(reason));
      }
    };
    void refresh();
    const interval = setInterval(refresh, 5000);
    return () => { cancelled = true; clearInterval(interval); };
  }, []);

  const requiredGranted = privateSession !== null && perms?.screen_capture === true
    && perms?.accessibility === true && (!privateSession || perms?.input_monitoring === true);

  return (
    <div className="flex flex-col gap-3 p-4">
      <h1 className="text-base font-semibold text-text">Permissions</h1>

      {requiredGranted && (
        <div className="flex items-center gap-2 rounded-[8px] border border-green/20 bg-green/10 p-3">
          <Check size={16} className="flex-shrink-0 text-green" />
          <div className="flex flex-1 items-center justify-between gap-3">
            <span className="text-xs font-medium text-green">
              Required permissions are ready
            </span>
            <Link
              to="/"
              className="rounded-md bg-green/15 px-2.5 py-1 text-[11px] font-medium text-green transition-colors hover:bg-green/25"
            >
              Back to dashboard
            </Link>
          </div>
        </div>
      )}

      {!requiredGranted && (
        <div className="rounded-[8px] border border-orange/20 bg-orange/10 p-3 text-[11px] text-text-secondary">
          Screen Recording and Input Monitoring can require reopening the app
          before macOS reports approval. Input Monitoring is requested only when
          Guarded private session is enabled in Settings.
          <p className="mt-2">Complete setup on the physical display before starting a remote connection.</p>
          <button onClick={() => void request()} disabled={requesting || serviceOwnsResources}
            className="mt-2 rounded-md bg-orange/15 px-2.5 py-1 font-medium text-orange disabled:opacity-50">
            {requesting ? "Requesting permissions..." : "Request required permissions"}
          </button>
          {serviceOwnsResources && <p className="mt-2">Stop the server and wait for cleanup before requesting permissions.</p>}
          {error && <p role="alert" className="mt-2 text-red">{error}</p>}
        </div>
      )}

      <div className="flex flex-col gap-2">
        {permissionDefs.filter(def => !("privateOnly" in def && def.privateOnly) || privateSession).map((def) => {
          const granted = perms?.[def.key] ?? false;
          const Icon = def.icon;
          return (
            <div
              key={def.key}
              className="flex items-center justify-between rounded-[8px] bg-card p-3"
            >
              <div className="flex items-center gap-2.5">
                <Icon size={20} className="flex-shrink-0 text-text-muted" />
                <div>
                  <div className="text-xs font-medium text-text">
                    {def.name}
                    {"optional" in def && def.optional ? " (optional)" : ""}
                  </div>
                  <div className="text-[11px] text-text-muted">
                    {granted ? "Granted" : "Not granted"}
                  </div>
                </div>
              </div>
              <div>
                {granted ? (
                  <Check size={16} className="text-green" />
                ) : (
                  <button
                    onClick={() => api.openSystemPreferences(def.pane)}
                    disabled={def.key === "input_monitoring" && serviceOwnsResources}
                    className="rounded-md bg-orange/15 px-2.5 py-1 text-[11px] font-medium text-orange transition-colors hover:bg-orange/25 disabled:opacity-50"
                  >
                    Open System Settings
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default Permissions;
