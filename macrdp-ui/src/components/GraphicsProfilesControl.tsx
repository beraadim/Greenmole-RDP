import { useEffect, useRef, useState } from "react";
import type { UiConfig } from "../lib/types";
import { graphicsProfilesApi, type GraphicsProfiles } from "../lib/graphicsProfiles";
import { Input } from "./ui/input";
import { Button } from "./ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";

export function GraphicsProfilesControl({ config, busy, pending, run, refresh }: {
  config: UiConfig;
  busy: boolean;
  pending: boolean;
  run: (operation: () => Promise<void>) => Promise<void>;
  refresh: () => Promise<void>;
}) {
  const [profiles, setProfiles] = useState<GraphicsProfiles | null>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<string | null>(null);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  useEffect(() => {
    let cancelled = false;
    graphicsProfilesApi.get().then(value => { if (!cancelled) setProfiles(value); })
      .catch(reason => { if (!cancelled) setError(String(reason)); });
    return () => { cancelled = true; };
  }, [config.frame_rate, config.bitrate_mbps, config.quality, config.encoder, config.chroma_mode]);

  const operate = (action: () => Promise<void>) => void run(async () => {
    setError(null); setFeedback(null);
    try { await action(); }
    catch (reason) {
      if (!mounted.current) return;
      setError(String(reason));
      // A timeout can follow durable persistence. Reload current preferences
      // and pending state; don't pretend the failed operation rolled back.
      await Promise.allSettled([refresh(), graphicsProfilesApi.get().then(value => { if (mounted.current) setProfiles(value); })]);
    }
  });
  const apply = (id: string | null) => operate(async () => {
    const result = await graphicsProfilesApi.apply(id);
    if (!mounted.current) return;
    setProfiles(result.profiles);
    setFeedback(!result.live_applied ? "Saved for the next server start." : result.requires_reconnect
      ? "Profile accepted. Reconnect an existing client to use the changed color detail or resume browser video."
      : "Profile accepted. Live-safe settings apply now; a browser already using image fallback needs reconnecting to resume video.");
    await refresh();
  });
  const selected = profiles?.profiles.find(profile => profile.id === profiles.active_profile);

  return <div className="mb-4 rounded-md border border-border p-3 space-y-2">
    <label className="block text-xs font-medium">Graphics profile</label>
    <Select disabled={busy || !profiles} value={profiles?.active_profile ?? "custom"}
      onValueChange={id => { if (id !== "custom") apply(id); }}>
      <SelectTrigger aria-label="Graphics profile" className="h-8 w-full max-w-[260px] text-xs"><SelectValue>{selected ? `${selected.name}${selected.builtin ? "" : " (saved)"}` : "Custom settings"}</SelectValue></SelectTrigger>
      <SelectContent>
        <SelectItem value="custom" disabled>Custom settings</SelectItem>
        {profiles?.profiles.map(profile => <SelectItem key={profile.id} value={profile.id}>{profile.name}{profile.builtin ? "" : " (saved)"}</SelectItem>)}
      </SelectContent>
    </Select>
    {selected && <p className="text-[11px] text-text-muted">{selected.settings.frame_rate} fps · {selected.settings.bitrate_mbps} Mbps · {selected.settings.chroma_mode.toUpperCase()}</p>}
    <p className="text-[11px] text-text-muted">Applies frame rate, bitrate, image quality, encoder and color detail together. Manual changes keep your overrides as Custom settings. Color detail applies on the next connection.</p>
    <div className="flex flex-wrap items-center gap-2">
      <Input aria-label="Saved graphics profile name" placeholder="Name these settings" maxLength={48} value={name}
        disabled={busy} onChange={event => setName(event.target.value)} className="h-8 w-[200px] text-xs" />
      <Button type="button" variant="outline" size="sm" disabled={busy || !name.trim()} onClick={() => operate(async () => {
        const value = await graphicsProfilesApi.save(name);
        if (!mounted.current) return;
        setProfiles(value); setName(""); setFeedback("Current graphics settings saved. Reusing a saved name updates that profile."); await refresh();
      })}>Save current</Button>

    </div>
    {profiles && profiles.profiles.some(profile => !profile.builtin) && <details className="text-xs text-text-secondary">
      <summary className="cursor-pointer">Manage saved profiles</summary>
      <div className="mt-2 space-y-1">
        {profiles.profiles.filter(profile => !profile.builtin).map(profile => <div key={profile.id} className="flex items-center justify-between gap-2">
          <span className="break-all">{profile.name}</span>
          <Button type="button" variant="outline" size="sm" disabled={busy} aria-label={`Delete saved profile ${profile.name}`} onClick={() => operate(async () => {
            const value = await graphicsProfilesApi.remove(profile.id);
            if (!mounted.current) return;
            setProfiles(value); setFeedback("Saved profile deleted. Current graphics settings are unchanged."); await refresh();
          })}>Delete</Button>
        </div>)}
      </div>
    </details>}
    {pending && <div role="status" className="rounded-md border border-yellow-400/60 p-2 text-xs">
      <p>Saved graphics settings have not been confirmed by the server. Your current connection remains available.</p>
      <button type="button" disabled={busy} onClick={() => apply(null)} className="mt-1 text-accent underline disabled:opacity-50">Retry saved graphics settings</button>
    </div>}
    {feedback && <p role="status" className="text-xs text-text-secondary">{feedback}</p>}
    {error && <p role="alert" className="break-words text-xs text-red-600 dark:text-red-400">{error}</p>}
  </div>;
}
