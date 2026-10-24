import { useState } from "react";
import type { UiConfig } from "../lib/types";
import { SettingRow } from "./SettingRow";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";

const layouts: [UiConfig["keyboard_layout"], string][] = [
  ["auto", "Automatic (client layout)"],
  ["us", "English (US)"],
  ["uk", "English (UK)"],
  ["no", "Norwegian"],
  ["se", "Swedish"],
  ["dk", "Danish"],
  ["de", "German"],
  ["fr", "French"],
  ["physical", "Physical keys (host layout)"],
];

const shortcuts: [UiConfig["keyboard_shortcuts"], string][] = [
  ["auto", "Automatic"],
  ["windows", "Windows-friendly"],
  ["mac", "Native Mac"],
  ["physical", "Standard modifiers"],
];

const shortcutDescriptions: Record<UiConfig["keyboard_shortcuts"], string> = {
  auto: "Uses the Greenmole browser's shortcut preference when available. Other clients retain Windows-friendly shortcuts.",
  windows: "Preserves Ctrl+A/X/C/V and Ctrl+Enter as Mac Command shortcuts. Alt acts as Command; Windows acts as Option.",
  mac: "Preserves Mac Command, Option and Control shortcuts, including Control+C in Terminal.",
  physical: "Keeps Control unchanged. Windows/Super acts as Command; Alt acts as Option.",
};

export function KeyboardSettings({ config, save }: {
  config: UiConfig;
  save: (key: keyof UiConfig, value: unknown) => Promise<boolean>;
}) {
  const [saving, setSaving] = useState(false);
  const persist = async (key: keyof UiConfig, value: string) => {
    setSaving(true);
    try { return await save(key, value); }
    finally { setSaving(false); }
  };

  return (
    <>
      <SettingRow label="Remote keyboard layout"
        description="Automatic follows the layout reported by the RDP client. Override it only if the client reports the wrong layout.">
        <Select value={config.keyboard_layout} items={Object.fromEntries(layouts)} disabled={saving}
          onValueChange={(value) => { if (value !== null) void persist("keyboard_layout", value); }}>
          <SelectTrigger aria-label="Remote keyboard layout" className="h-7 w-[205px] text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {layouts.map(([value, label]) => <SelectItem key={value} value={value}>{label}</SelectItem>)}
          </SelectContent>
        </Select>
      </SettingRow>
      <SettingRow label="Shortcut preference" description={shortcutDescriptions[config.keyboard_shortcuts]}>
        <Select value={config.keyboard_shortcuts} items={Object.fromEntries(shortcuts)} disabled={saving}
          onValueChange={(value) => { if (value !== null) void persist("keyboard_shortcuts", value); }}>
          <SelectTrigger aria-label="Shortcut preference" className="h-7 w-[205px] text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {shortcuts.map(([value, label]) => <SelectItem key={value} value={value}>{label}</SelectItem>)}
          </SelectContent>
        </Select>
      </SettingRow>
      <p className="py-2 text-xs text-text-muted">
        Both settings require a service restart. Automatic preserves the existing
        Norwegian Windows mapping. Greenmole browser sessions normally report a US
        layout even when your local keyboard is Norwegian; leave that connection
        setting unchanged. Unsupported layouts use physical keys and the Mac's
        selected input source.
      </p>
    </>
  );
}
