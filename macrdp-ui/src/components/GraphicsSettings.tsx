import { useRef, useState } from "react";
import type { UiConfig } from "../lib/types";
import { SettingRow } from "./SettingRow";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";

const bitratePresets = [2, 4, 8, 16, 32, 50];
const frameRatePresets = [15, 30, 60, 120];
type GraphicsKey = "quality" | "encoder" | "bitrate_mbps" | "frame_rate" | "chroma_mode";

export function GraphicsSettings({ config, running, pendingLive, save, loadSaved }: {
  config: UiConfig;
  running: boolean;
  pendingLive: string[];
  save: (key: keyof UiConfig, value: unknown) => Promise<boolean>;
  loadSaved: () => Promise<UiConfig>;
}) {
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);
  const [retryError, setRetryError] = useState<string | null>(null);
  // The ref closes the gap before React renders disabled controls, preventing
  // overlapping encoder/quality requests from reporting completion out of order.
  const inFlight = useRef(false);
  const videoDisabled = config.quality === "lossless" || config.encoder === "bitmap";

  const change = async (key: GraphicsKey, value: unknown) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setSaving(true);
    setFeedback(null);
    setRetryError(null);
    try {
      if (await save(key, value)) {
        setFeedback(!running
          ? "Saved for the next server start."
          : key === "chroma_mode"
            ? "Change accepted. Reconnect the client to use the new video color detail."
            : "Change accepted. It applies during the current connection.");
      }
    } finally {
      inFlight.current = false;
      setSaving(false);
    }
  };

  const retry = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setSaving(true);
    setFeedback(null);
    setRetryError(null);
    try {
      // A previous request may have saved its target before its error/refetch
      // reaches React. Reload durable preferences instead of retrying stale props.
      const saved = await loadSaved();
      for (const key of pendingLive) {
        if (key !== "quality" && key !== "encoder") continue;
        if (!(await save(key, saved[key]))) return;
      }
      setFeedback("Changes accepted. They apply during the current connection.");
    } catch (error) {
      setRetryError(`Could not load saved graphics settings: ${String(error)}`);
    } finally {
      inFlight.current = false;
      setSaving(false);
    }
  };

  return <>
    {pendingLive.length > 0 && <div role="status" className="mb-3 rounded-md border border-yellow-400/60 p-2 text-xs text-text-secondary">
      <p>Saved graphics changes have not been confirmed by the server.</p>
      <button type="button" disabled={saving} onClick={() => void retry()}
        className="mt-1 text-accent underline disabled:opacity-50">Retry pending changes</button>
    </div>}
    <SettingRow label="Image quality" description="Applies while connected; no server restart">
      <Select disabled={saving} value={config.quality} onValueChange={value => void change("quality", value)}>
        <SelectTrigger className="h-7 w-[180px] text-xs"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="low_latency">Performance</SelectItem>
          <SelectItem value="balanced">Balanced</SelectItem>
          <SelectItem value="high_quality">High quality</SelectItem>
          <SelectItem value="lossless">Lossless desktop</SelectItem>
        </SelectContent>
      </Select>
    </SettingRow>
    <p className="py-2 text-xs text-text-muted">
      {config.quality === "lossless"
        ? "Preserves captured colors with lossless image compression. Uses more bandwidth and may reduce frame rate."
        : config.quality === "high_quality"
          ? "Reduces compression artifacts. Uses more bandwidth and encoding time."
          : config.quality === "low_latency"
            ? "Prioritizes speed and smaller updates; fine text and gradients may show more artifacts."
            : "Balances image detail, bandwidth and responsiveness."}
      {config.quality === "lossless" && " In Guacamole, also enable its ‘Force lossless’ connection option to preserve image quality in the browser."}
    </p>
    <SettingRow label="Encoder" description="Applies while connected; no server restart">
      <Select disabled={saving} value={config.encoder} onValueChange={value => void change("encoder", value)}>
        <SelectTrigger className="h-7 w-[180px] text-xs"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="auto">Auto</SelectItem>
          <SelectItem value="software">Software encoding</SelectItem>
          <SelectItem value="hardware">Hardware acceleration</SelectItem>
          <SelectItem value="bitmap">Bitmap fallback</SelectItem>
        </SelectContent>
      </Select>
    </SettingRow>
    <p className="py-2 text-xs text-text-muted">
      {config.quality === "lossless"
        ? "Lossless desktop uses image updates. Your encoder selection is used when you choose another quality."
        : config.encoder === "bitmap"
          ? "Sends image updates instead of video. Image quality still controls compression."
          : "Uses video when supported by the client, with image updates available as a fallback."}
      {" "}After switching back from Bitmap fallback or Lossless desktop, reconnect Guacamole to resume browser video.
    </p>
    <SettingRow label="Video bitrate" description="Only used for video; image updates manage compression separately">
      <Select disabled={saving || videoDisabled} value={String(config.bitrate_mbps)}
        onValueChange={value => void change("bitrate_mbps", Number(value))}>
        <SelectTrigger className="h-7 w-[120px] text-xs"><SelectValue /></SelectTrigger>
        <SelectContent>
          {!bitratePresets.includes(config.bitrate_mbps) && <SelectItem value={String(config.bitrate_mbps)}>{config.bitrate_mbps} Mbps (custom)</SelectItem>}
          {bitratePresets.map(value => <SelectItem key={value} value={String(value)}>{value} Mbps</SelectItem>)}
        </SelectContent>
      </Select>
    </SettingRow>
    <SettingRow label="Frame rate">
      <Select disabled={saving} value={String(config.frame_rate)}
        onValueChange={value => void change("frame_rate", Number(value))}>
        <SelectTrigger className="h-7 w-[120px] text-xs"><SelectValue /></SelectTrigger>
        <SelectContent>
          {!frameRatePresets.includes(config.frame_rate) && <SelectItem value={String(config.frame_rate)}>{config.frame_rate} fps (custom)</SelectItem>}
          {frameRatePresets.map(value => <SelectItem key={value} value={String(value)}>{value} fps</SelectItem>)}
        </SelectContent>
      </Select>
    </SettingRow>
    <SettingRow label="Video color detail" description="Reconnect the client to apply; full color requires AVC444 support">
      <Select disabled={saving || videoDisabled} value={config.chroma_mode}
        onValueChange={value => void change("chroma_mode", value)}>
        <SelectTrigger className="h-7 w-[160px] text-xs"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="avc420">Standard (AVC420)</SelectItem>
          <SelectItem value="avc444">Full color (AVC444)</SelectItem>
        </SelectContent>
      </Select>
    </SettingRow>
    {retryError && <p role="alert" className="pt-2 text-xs text-red-600 dark:text-red-400">{retryError}</p>}
    {(saving || feedback) && <p role="status" className="pt-2 text-xs text-text-secondary">
      {saving ? "Saving graphics change…" : feedback}
    </p>}
  </>;
}
