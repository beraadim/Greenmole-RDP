import { useEffect, useState } from "react";
import type { UiConfig } from "../lib/types";
import { SettingRow } from "./SettingRow";
import { Input } from "./ui/input";
import { Switch } from "./ui/switch";

export function ClipboardSettings({ config, save }: {
  config: UiConfig;
  save: (key: keyof UiConfig, value: unknown) => Promise<boolean>;
}) {
  const [limit, setLimit] = useState(String(config.clipboard_max_file_size_mb));
  const [saving, setSaving] = useState(false);
  useEffect(() => setLimit(String(config.clipboard_max_file_size_mb)), [config.clipboard_max_file_size_mb]);

  const persist = async (key: keyof UiConfig, value: unknown) => {
    setSaving(true);
    try { return await save(key, value); }
    finally { setSaving(false); }
  };

  return (
    <>
      <SettingRow label="Synchronize clipboard" description="Share copied text, images and files. Requires service restart.">
        <Switch aria-label="Synchronize clipboard" checked={config.clipboard_enabled}
          disabled={saving} onCheckedChange={(value) => persist("clipboard_enabled", value)} />
      </SettingRow>
      <SettingRow label="Allow file transfers" description="Copy files between this Mac and the remote client. Requires service restart.">
        <Switch aria-label="Allow file transfers" checked={config.clipboard_file_transfer}
          disabled={saving || !config.clipboard_enabled}
          onCheckedChange={(value) => persist("clipboard_file_transfer", value)} />
      </SettingRow>
      <SettingRow label="Maximum file size (MiB)" description="1–256 MiB per file; 256 MiB per incoming batch. Requires service restart.">
        <Input aria-label="Maximum file size in MiB" type="number" min={1} max={256} step={1}
          value={limit} disabled={saving || !config.clipboard_enabled || !config.clipboard_file_transfer}
          onChange={(event) => setLimit(event.target.value)}
          onBlur={async (event) => {
            if (!event.target.value || !event.target.validity.valid) {
              setLimit(String(config.clipboard_max_file_size_mb));
              return;
            }
            const value = Number(event.target.value);
            if (value !== config.clipboard_max_file_size_mb &&
              !await persist("clipboard_max_file_size_mb", value)) {
              setLimit(String(config.clipboard_max_file_size_mb));
            }
          }}
          onKeyDown={(event) => { if (event.key === "Enter") event.currentTarget.blur(); }}
          className="h-7 w-[120px] text-xs" />
      </SettingRow>
      <p className="py-2 text-xs text-text-muted">
        The client must also allow clipboard sharing and file transfers. Turning off
        clipboard synchronization keeps your file transfer preferences for later.
      </p>
    </>
  );
}
