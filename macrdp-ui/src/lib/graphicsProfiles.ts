import { invoke } from "@tauri-apps/api/core";
import type { ConfigurationStatus } from "./types";

export interface GraphicsValues {
  frame_rate: number;
  bitrate_mbps: number;
  quality: string;
  encoder: string;
  chroma_mode: string;
}
export interface GraphicsProfile { id: string; name: string; builtin: boolean; settings: GraphicsValues }
export interface GraphicsProfiles { profiles: GraphicsProfile[]; active_profile: string | null; current: GraphicsValues }
export interface AppliedGraphicsProfile { profiles: GraphicsProfiles; configuration: ConfigurationStatus; live_applied: boolean; requires_reconnect: boolean }
export const graphicsProfilesApi = {
  get: () => invoke<GraphicsProfiles>("get_graphics_profiles"),
  save: (name: string) => invoke<GraphicsProfiles>("save_graphics_profile", { name }),
  remove: (id: string) => invoke<GraphicsProfiles>("delete_graphics_profile", { id }),
  apply: (id: string | null) => invoke<AppliedGraphicsProfile>("apply_graphics_profile", { id }),
};
