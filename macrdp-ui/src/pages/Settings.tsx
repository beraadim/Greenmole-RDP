import { CertificateSettings } from "../components/CertificateSettings";
import { ClipboardSettings } from "../components/ClipboardSettings";
import { GraphicsSettings } from "../components/GraphicsSettings";
import { useState, useEffect } from "react";
import { AlertTriangle } from "lucide-react";
import { api } from "../lib/ipc";
import type { ConfigurationStatus, UiConfig } from "../lib/types";
import { Input } from "../components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui/select";
import { Switch } from "../components/ui/switch";
import { Alert, AlertDescription } from "../components/ui/alert";
import { SettingRow } from "../components/SettingRow";
import { useTheme } from "../contexts/ThemeContext";

const categories = [
  { id: "rdp", label: "RDP Service" },
  { id: "network", label: "Network" },
  { id: "security", label: "Security" },
  { id: "display", label: "Display" },
  { id: "audio", label: "Audio" },
  { id: "clipboard", label: "Clipboard" },
  { id: "general", label: "General" },
] as const;

type CategoryId = (typeof categories)[number]["id"];

const restartLabels: Record<string, string> = {
  display_backend: "display", bind_address: "bind address", port: "port",
  reuse_virtual_display: "virtual display reuse",
  private_session: "guarded private session",
  idle_timeout_secs: "idle timeout", tls_cert_path: "certificate", tls_key_path: "certificate key",
  tls_server_names: "certificate names", audio_enabled: "system audio",
  clipboard_enabled: "clipboard synchronization", clipboard_file_transfer: "file transfers",
  clipboard_max_file_size_mb: "file size limit", resolution: "resolution",
};
const settingLabel = (key: string) => restartLabels[key] ?? "service setting";
const resolutionPresets = ["3840x2160", "2560x1600", "2560x1440", "2048x1536",
  "1920x1440", "1920x1200", "1920x1080", "1680x1050", "1600x1200",
  "1600x1024", "1600x900", "1440x1080", "1366x768"];

function Settings() {
  const [config, setConfig] = useState<UiConfig | null>(null);
  const [configurationState, setConfigurationState] = useState<ConfigurationStatus | null>(null);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [activeCategory, setActiveCategory] = useState<CategoryId>("rdp");
  const { theme, setTheme } = useTheme();

  useEffect(() => {
    let cancelled = false;
    api.getConfig().then((value) => { if (!cancelled) setConfig(value); })
      .catch(error => { if (!cancelled) setSettingsError(String(error)); });
    const refreshStatus = async () => {
      try {
        const value = await api.getConfigurationStatus();
        if (!cancelled) setConfigurationState(value);
      } catch (error) {
        if (!cancelled) setSettingsError(String(error));
      }
    };
    void refreshStatus();
    const timer = setInterval(refreshStatus, 2000);
    return () => { cancelled = true; clearInterval(timer); };
  }, []);

  const updateConfig = async (key: keyof UiConfig, value: unknown) => {
    try {
      setSettingsError(null);
      const result = await api.setConfig(key, value);
      setConfigurationState(result);
      setConfig((prev) => (prev ? { ...prev, [key]: value } : prev));
      return true;
    } catch (err) {
      setSettingsError(String(err));
      const [saved, status] = await Promise.allSettled([
        api.getConfig(), api.getConfigurationStatus(),
      ]);
      if (saved.status === "fulfilled") setConfig(saved.value);
      if (status.status === "fulfilled") setConfigurationState(status.value);
      return false;
    }
  };

  const handleAutostart = async (enabled: boolean) => {
    try {
      await api.setAutostart(enabled);
      await updateConfig("autostart", enabled);
    } catch (err) {
      console.error("Failed to set autostart:", err);
    }
  };

  if (!config) {
    return (
      <div className="flex items-center justify-center py-20 text-sm text-text-muted">
        {settingsError ?? "Loading configuration..."}
      </div>
    );
  }

  const resolutions = config.display_backend === "main"
    ? ["7680x4320", "5120x2880", ...resolutionPresets] : resolutionPresets;

  return (
    <div className="flex h-full">
      {/* Left nav */}
      <nav className="w-[140px] shrink-0 border-r border-border py-3 px-2 space-y-0.5">
        {categories.map((cat) => (
          <button
            key={cat.id}
            onClick={() => setActiveCategory(cat.id)}
            className={`w-full text-left text-xs px-2.5 py-1.5 rounded-md transition-colors ${
              activeCategory === cat.id
                ? "bg-accent/12 text-accent font-medium"
                : "text-text-secondary hover:text-text hover:bg-card"
            }`}
          >
            {cat.label}
          </button>
        ))}
      </nav>

      {/* Right panel */}
      <div className="flex-1 overflow-y-auto p-4">
        {settingsError && <p role="alert" className="mb-3 text-xs text-red-600 dark:text-red-400">{settingsError}</p>}
        {/* Restart required banner */}
        {configurationState?.restart_required && (
          <Alert
            variant="default"
            className="mb-4 border-yellow-400/60 bg-yellow-50 dark:bg-yellow-950/30"
          >
            <AlertTriangle className="h-4 w-4 text-yellow-600 dark:text-yellow-400" />
            <AlertDescription className="flex items-center justify-between">
              <span className="text-yellow-800 dark:text-yellow-300 text-xs">
                Saved settings are pending a service restart: {configurationState.pending_restart.map(settingLabel).join(", ")}.
              </span>

            </AlertDescription>
          </Alert>
        )}

        {activeCategory === "rdp" && (
          <CategorySection title="RDP Service">
            <GraphicsSettings config={config} running={configurationState?.running ?? false}
              pendingLive={configurationState?.pending_live ?? []} save={updateConfig} loadSaved={api.getConfig} />
          </CategorySection>
        )}

        {activeCategory === "network" && (
          <CategorySection title="Network">
            <SettingRow label="Port" description="Requires service restart">
              <NumericSettingInput
                label="Port" min={1} max={65535}
                value={config.port}
                save={value => updateConfig("port", value)}
              />
            </SettingRow>

            <SettingRow label="Bind address" description="Requires service restart">
              <Input
                type="text"
                value={config.bind_address}
                placeholder="0.0.0.0"
                onChange={(e) =>
                  setConfig((prev) =>
                    prev ? { ...prev, bind_address: e.target.value } : prev
                  )
                }
                onBlur={(e) => updateConfig("bind_address", e.target.value)}
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>

            <SettingRow label="Connections" description="One remote session at a time">
              <span className="text-xs text-text-muted">1 active session</span>
            </SettingRow>

            <SettingRow label="Idle timeout (seconds)" description="Keyboard/mouse inactivity; 0 disables. Requires service restart">
              <NumericSettingInput
                label="Idle timeout" min={0} max={Number.MAX_SAFE_INTEGER}
                value={config.idle_timeout_secs}
                save={value => updateConfig("idle_timeout_secs", value)}
              />
            </SettingRow>
          </CategorySection>
        )}

        {activeCategory === "security" && (
          <CategorySection title="Security">
            <SettingRow label="Guarded private session"
              description={config.display_backend === "virtual"
                ? "Requires service restart"
                : "Select Virtual display under Display first"}>
              <Switch aria-label="Guarded private session"
                checked={config.private_session}
                disabled={config.display_backend !== "virtual"}
                onCheckedChange={(enabled) => updateConfig("private_session", enabled)} />
            </SettingRow>
            <p className="py-2 text-xs text-text-muted">
              Hides physical screens and blocks local keyboard and pointer input
              while connected. Restores the locked state on disconnect if this
              Mac was locked before connecting; a guard failure locks the Mac.
              Accessibility and Input Monitoring access are required.
              This shares your macOS session and does not isolate it from local
              administrator access or physical power controls.
            </p>
            <SettingRow label="Username">
              <Input
                type="text"
                value={config.username}
                onChange={(e) =>
                  setConfig((prev) =>
                    prev ? { ...prev, username: e.target.value } : prev
                  )
                }
                onBlur={(e) => updateConfig("username", e.target.value)}
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>

            <SettingRow label="Password">
              <Input
                type="password"
                value={config.password}
                onChange={(e) =>
                  setConfig((prev) =>
                    prev ? { ...prev, password: e.target.value } : prev
                  )
                }
                onBlur={(e) => updateConfig("password", e.target.value)}
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>
            <CertificateSettings config={config} save={updateConfig} />
          </CategorySection>
        )}

        {activeCategory === "display" && (
          <CategorySection title="Display">
            <SettingRow label="Display" description="Requires service restart">
              <Select
                value={config.display_backend}
                onValueChange={(v) => updateConfig("display_backend", v)}
              >
                <SelectTrigger className="h-7 w-[210px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="main" disabled={config.private_session || config.reuse_virtual_display}>Main display</SelectItem>
                  <SelectItem value="virtual">Virtual display (experimental)</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>

            {config.private_session && (
              <p className="py-2 text-xs text-text-muted">
                Disable Guarded private session under Security before selecting the main display.
              </p>
            )}

            {config.reuse_virtual_display && (
              <p className="py-2 text-xs text-text-muted">
                Disable virtual display reuse before selecting the main display.
              </p>
            )}

            {config.display_backend === "virtual" && (
              <p className="py-2 text-xs text-text-muted">
                Creates a main Mac display when you connect, bringing the Dock
                and menu bar to the remote desktop. Restores the previous display
                arrangement and captured window positions when you disconnect.
              </p>
            )}

            <SettingRow label="Reuse virtual display between sessions"
              description={config.display_backend === "virtual"
                ? "Experimental · Off by default · Requires service restart"
                : "Select Virtual display first"}>
              <Switch aria-label="Reuse virtual display between sessions"
                checked={config.reuse_virtual_display}
                disabled={config.display_backend !== "virtual"}
                onCheckedChange={(enabled) => updateConfig("reuse_virtual_display", enabled)} />
            </SettingRow>
            {config.display_backend === "virtual" && (
              <p className="py-2 text-xs text-text-muted">
                Creates the display on the first connection, then keeps it
                allocated but disabled between sessions. Capture and encoding
                stop while disconnected. The display is removed when the server
                stops. This tests whether reduced display creation and removal
                improves local responsiveness after remote sessions.
              </p>
            )}

            <SettingRow
              label="Resolution"
              description={config.display_backend === "virtual"
                ? "Requires service restart; Auto follows the client"
                : "Applies to new connections; Auto follows the screen"}
            >
              <Select
                value={config.resolution || "auto"}
                onValueChange={(v) => updateConfig("resolution", v)}
              >
                <SelectTrigger className="h-7 w-[180px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="auto">Auto</SelectItem>
                  {config.resolution && config.resolution !== "auto" && !resolutions.includes(config.resolution) && (
                    <SelectItem value={config.resolution}>{config.resolution.replace("x", " × ")} (custom)</SelectItem>
                  )}
                  {resolutions.map(value => <SelectItem key={value} value={value}>{value.replace("x", " × ")}</SelectItem>)}
                </SelectContent>
              </Select>
            </SettingRow>

            <SettingRow label="Show cursor" description="Render the system cursor in the captured output">
              <Switch
                checked={config.show_cursor}
                onCheckedChange={(checked) => updateConfig("show_cursor", checked)}
              />
            </SettingRow>
          </CategorySection>
        )}

        {activeCategory === "audio" && (
          <CategorySection title="Audio">
            <SettingRow label="Forward system audio" description="Play this Mac's audio on the remote client. Requires service restart.">
              <Switch aria-label="Forward system audio" checked={config.audio_enabled}
                onCheckedChange={(value) => updateConfig("audio_enabled", value)} />
            </SettingRow>
            <p className="py-2 text-xs text-text-muted">
              Audio uses 48 kHz stereo. Enable sound playback in the remote client as well.
            </p>
          </CategorySection>
        )}

        {activeCategory === "clipboard" && (
          <CategorySection title="Clipboard">
            <ClipboardSettings config={config} save={updateConfig} />
          </CategorySection>
        )}

        {activeCategory === "general" && (
          <CategorySection title="General">
            <SettingRow label="Log level">
              <Select
                value={config.log_level}
                onValueChange={(v) => updateConfig("log_level", v)}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="trace">trace</SelectItem>
                  <SelectItem value="debug">debug</SelectItem>
                  <SelectItem value="info">info</SelectItem>
                  <SelectItem value="warn">warn</SelectItem>
                  <SelectItem value="error">error</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>

            <SettingRow label="Start at login" description="Start the service automatically when you sign in">
              <Switch
                checked={config.autostart}
                onCheckedChange={handleAutostart}
              />
            </SettingRow>

            <SettingRow label="Theme">
              <Select
                value={theme}
                onValueChange={(v) => {
                  if (v) setTheme(v as "system" | "light" | "dark");
                }}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="system">Follow system</SelectItem>
                  <SelectItem value="light">Light</SelectItem>
                  <SelectItem value="dark">Dark</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>
          </CategorySection>
        )}
      </div>
    </div>
  );
}

function NumericSettingInput({ label, value, min, max, save }: {
  label: string; value: number; min: number; max: number;
  save: (value: number) => Promise<boolean>;
}) {
  const [draft, setDraft] = useState(String(value));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => { setDraft(String(value)); }, [value]);
  const commit = async () => {
    if (saving || draft === String(value)) return;
    const next = /^\d+$/.test(draft) ? Number(draft) : NaN;
    if (!Number.isSafeInteger(next) || next < min || next > max) {
      setError(`${label} must be a whole number between ${min} and ${max}.`);
      return;
    }
    setError(null);
    setSaving(true);
    try {
      if (!(await save(next))) setDraft(String(value));
    } finally {
      setSaving(false);
    }
  };
  return <div className="w-[120px]">
    <Input type="number" aria-label={label} aria-invalid={error !== null}
      min={min} max={max} step={1} value={draft} disabled={saving}
      onChange={event => { setDraft(event.target.value); setError(null); }}
      onBlur={() => void commit()}
      onKeyDown={event => { if (event.key === "Enter") event.currentTarget.blur(); }}
      className="h-7 w-full text-xs" />
    {error && <p role="alert" className="mt-1 text-xs text-red-600 dark:text-red-400">{error}</p>}
  </div>;
}

function CategorySection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <h2 className="text-sm font-medium text-text mb-3">{title}</h2>
      <div className="space-y-1.5">{children}</div>
    </div>
  );
}

export default Settings;
