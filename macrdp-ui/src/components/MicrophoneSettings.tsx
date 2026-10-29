import { useEffect, useState } from "react";
import { api } from "../lib/ipc";
import type { MicrophoneDriverStatus, UiConfig } from "../lib/types";
import { SettingRow } from "./SettingRow";
import { Button } from "./ui/button";
import { Switch } from "./ui/switch";

export function MicrophoneSettings({ config, running, save }: {
  config: UiConfig;
  running: boolean;
  save: (key: keyof UiConfig, value: unknown) => Promise<boolean>;
}) {
  const [status, setStatus] = useState<MicrophoneDriverStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api.getMicrophoneDriverStatus().then(value => { if (!cancelled) setStatus(value); })
      .catch(reason => { if (!cancelled) setError(String(reason)); });
    return () => { cancelled = true; };
  }, []);

  const driverAction = async (action: () => Promise<MicrophoneDriverStatus>) => {
    setBusy(true);
    setError(null);
    try { setStatus(await action()); }
    catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  };

  return (
    <>
      <SettingRow label="Use the client's microphone"
        description="Send the remote client's microphone to this Mac. Off by default. Requires service restart.">
        <Switch aria-label="Use the client's microphone" checked={config.microphone_enabled}
          disabled={saving || busy} onCheckedChange={async value => {
            setSaving(true);
            try { await save("microphone_enabled", value); }
            finally { setSaving(false); }
          }} />
      </SettingRow>
      <p className="py-2 text-xs text-text-muted">
        A client that allows microphone redirection can stream audio throughout an
        active session, even when a Mac app is not recording. Disconnect or disable
        microphone access in the client to stop capture immediately. Changing this
        server setting takes effect after a service restart.
      </p>
      <SettingRow label="Virtual microphone driver"
        description={status?.message ?? "Checking the installed microphone driver…"}>
        <span className="text-xs text-text-secondary" role="status">
          {status ? status.ready ? "Ready" : status.installed ? "Installed · not ready" : "Not installed" : "Checking…"}
        </span>
      </SettingRow>
      <div className="flex flex-wrap gap-2 py-2">
        <Button size="sm" variant="outline" disabled={busy || running || !status?.installation_available}
          onClick={() => driverAction(api.installMicrophoneDriver)}>
          {busy ? "Working…" : status?.installed ? "Reinstall driver…" : "Install driver…"}
        </Button>
        <Button size="sm" variant="outline" disabled={busy || running || !status?.installed || !status?.removal_available}
          onClick={() => driverAction(api.removeMicrophoneDriver)}>
          Remove driver…
        </Button>
        <Button size="sm" variant="ghost" disabled={busy}
          onClick={() => driverAction(api.getMicrophoneDriverStatus)}>
          Refresh status
        </Button>
      </div>
      {error && <p role="alert" className="py-2 text-xs text-red-600 dark:text-red-400">{error}</p>}
      {status && !status.ready && config.microphone_enabled && (
        <p role="status" className="py-2 text-xs text-amber-700 dark:text-amber-400">
          Microphone redirection is configured, but its virtual input device is not
          ready. Mac apps cannot receive client audio until the driver is available.
        </p>
      )}
      <p className="py-2 text-xs text-text-muted">
        Install or remove prompts for a local administrator password and restarts
        Core Audio, briefly interrupting sound on this Mac. Stop the RDP service
        first. Opening this page and changing the toggle never install software.
      </p>
      <p className="py-2 text-xs text-text-muted">
        Enable microphone redirection in the client, then select Greenmole Remote
        Microphone in Teams or another Mac app. Windows Remote Desktop uses
        Local Resources → Remote audio → Settings → Record from this computer.
        Guacamole and Greenmole require audio input enabled, HTTPS and browser
        microphone permission. This feature does not access this Mac's physical microphone.
      </p>
    </>
  );
}
