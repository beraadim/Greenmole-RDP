import { useState, useEffect } from "react";
import { Monitor, MousePointer2, Mic, Check } from "lucide-react";
import { api } from "../lib/ipc";
import type { PermissionStatus } from "../lib/types";

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
    key: "microphone" as keyof PermissionStatus,
    name: "Microphone",
    pane: "microphone",
    icon: Mic,
  },
];

function Permissions() {
  const [perms, setPerms] = useState<PermissionStatus | null>(null);

  useEffect(() => {
    api.getPermissions().then(setPerms).catch(console.error);

    const interval = setInterval(() => {
      api.getPermissions().then(setPerms).catch(console.error);
    }, 5000);

    return () => clearInterval(interval);
  }, []);

  const allGranted =
    perms?.screen_capture === true &&
    perms?.accessibility === true &&
    perms?.microphone === true;

  return (
    <div className="flex flex-col gap-3 p-4">
      <h1 className="text-base font-semibold text-text">Permissions</h1>

      {allGranted && (
        <div className="flex items-center gap-2 rounded-[8px] border border-green/20 bg-green/10 p-3">
          <Check size={16} className="flex-shrink-0 text-green" />
          <span className="text-xs font-medium text-green">
            All permissions are ready
          </span>
        </div>
      )}

      <div className="flex flex-col gap-2">
        {permissionDefs.map((def) => {
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
                    className="rounded-md bg-orange/15 px-2.5 py-1 text-[11px] font-medium text-orange transition-colors hover:bg-orange/25"
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
