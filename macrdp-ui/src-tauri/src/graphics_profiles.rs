//! Named graphics-only preferences. A profile is saved once and sent through one
//! validated worker command; it never owns a display or changes session lifetime.
use crate::{
    configuration::{accepts_live_updates, configuration_status, ConfigurationStatus},
    state::AppState,
    ui_config::UiConfig,
};
use macrdp_core::{ConfigUpdate, GraphicsSettings};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};
use tauri::State;

const MAX_PROFILES: usize = 16;
const MAX_NAME_BYTES: usize = 96;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SavedGraphicsProfile {
    pub name: String,
    pub settings: GraphicsSettings,
}

#[derive(Clone, Serialize)]
pub struct GraphicsProfile {
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub settings: GraphicsSettings,
}

#[derive(Serialize)]
pub struct GraphicsProfiles {
    pub profiles: Vec<GraphicsProfile>,
    pub active_profile: Option<String>,
    pub current: GraphicsSettings,
}

#[derive(Serialize)]
pub struct AppliedGraphicsProfile {
    pub profiles: GraphicsProfiles,
    pub configuration: ConfigurationStatus,
    pub live_applied: bool,
    pub requires_reconnect: bool,
}

fn values(fps: u32, mbps: u32, quality: &str) -> GraphicsSettings {
    GraphicsSettings {
        frame_rate: fps,
        bitrate_mbps: mbps,
        quality: quality.into(),
        encoder: "auto".into(),
        chroma_mode: "avc420".into(),
    }
}

fn builtins() -> Vec<GraphicsProfile> {
    [
        ("mobile", "Mobile", values(30, 8, "balanced")),
        ("desktop", "Desktop", values(60, 32, "high_quality")),
        (
            "limited-bandwidth",
            "Limited bandwidth",
            values(15, 2, "low_latency"),
        ),
    ]
    .into_iter()
    .map(|(id, name, settings)| GraphicsProfile {
        id: format!("builtin:{id}"),
        name: name.into(),
        builtin: true,
        settings,
    })
    .collect()
}

fn checked_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_NAME_BYTES
        || trimmed.chars().count() > 48
        || trimmed.chars().any(char::is_control)
    {
        return Err("Profile names must contain 1–48 characters without control characters (at most 96 bytes)".into());
    }
    if builtins()
        .iter()
        .any(|p| p.name.to_lowercase() == trimmed.to_lowercase())
    {
        return Err("Choose a name other than Mobile, Desktop or Limited bandwidth".into());
    }
    Ok(trimmed.into())
}

pub(crate) fn validate_saved(profiles: &[SavedGraphicsProfile]) -> Result<(), String> {
    if profiles.len() > MAX_PROFILES {
        return Err("At most 16 saved graphics profiles are allowed".into());
    }
    let mut names = BTreeSet::new();
    for profile in profiles {
        let name = checked_name(&profile.name)?;
        if name != profile.name || !names.insert(name.to_lowercase()) {
            return Err("Saved graphics profile names must be distinct and trimmed".into());
        }
        profile.settings.validate().map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn settings(config: &UiConfig) -> GraphicsSettings {
    GraphicsSettings {
        frame_rate: config.frame_rate,
        bitrate_mbps: config.bitrate_mbps,
        quality: config.quality.clone(),
        encoder: config.encoder.clone(),
        chroma_mode: config.chroma_mode.clone(),
    }
}

fn snapshot(config: &UiConfig) -> GraphicsProfiles {
    let mut profiles = builtins();
    profiles.extend(config.graphics_profiles.iter().map(|p| GraphicsProfile {
        id: format!("saved:{}", p.name),
        name: p.name.clone(),
        builtin: false,
        settings: p.settings.clone(),
    }));
    let current = settings(config);
    let active_profile = config
        .graphics_profile
        .as_ref()
        .filter(|id| {
            profiles
                .iter()
                .any(|p| &p.id == *id && p.settings == current)
        })
        .cloned();
    GraphicsProfiles {
        profiles,
        active_profile,
        current,
    }
}

pub(crate) fn assign(config: &mut UiConfig, value: &GraphicsSettings) {
    config.frame_rate = value.frame_rate;
    config.bitrate_mbps = value.bitrate_mbps;
    config.quality = value.quality.clone();
    config.encoder = value.encoder.clone();
    config.chroma_mode = value.chroma_mode.clone();
}

fn needs_reconnect(previous: &UiConfig, next: &UiConfig) -> bool {
    previous.chroma_mode != next.chroma_mode
        || ((previous.encoder == "bitmap" || previous.quality == "lossless")
            && next.encoder != "bitmap"
            && next.quality != "lossless")
}

fn selected_candidate(config: &UiConfig, id: Option<&str>) -> Result<UiConfig, String> {
    let mut candidate = config.clone();
    if let Some(id) = id {
        let profile = snapshot(config)
            .profiles
            .into_iter()
            .find(|p| p.id == id)
            .ok_or("Unknown graphics profile")?;
        profile.settings.validate().map_err(|e| e.to_string())?;
        assign(&mut candidate, &profile.settings);
        candidate.graphics_profile = Some(profile.id);
    } else {
        settings(&candidate).validate().map_err(|e| e.to_string())?;
    }
    Ok(candidate)
}

fn save_current(config: &UiConfig, name: &str) -> Result<UiConfig, String> {
    let name = checked_name(name)?;
    let mut candidate = config.clone();
    let value = settings(config);
    value.validate().map_err(|e| e.to_string())?;
    if let Some(existing) = candidate
        .graphics_profiles
        .iter_mut()
        .find(|p| p.name.to_lowercase() == name.to_lowercase())
    {
        existing.settings = value;
        candidate.graphics_profile = Some(format!("saved:{}", existing.name));
    } else {
        if candidate.graphics_profiles.len() >= MAX_PROFILES {
            return Err("Delete a saved profile before adding another (maximum 16)".into());
        }
        candidate.graphics_profiles.push(SavedGraphicsProfile {
            name: name.clone(),
            settings: value,
        });
        candidate.graphics_profile = Some(format!("saved:{name}"));
    }
    validate_saved(&candidate.graphics_profiles)?;
    Ok(candidate)
}

fn remove_saved(config: &UiConfig, id: &str) -> Result<UiConfig, String> {
    let name = id
        .strip_prefix("saved:")
        .ok_or("Built-in profiles cannot be deleted")?;
    let mut candidate = config.clone();
    let old = candidate.graphics_profiles.len();
    candidate.graphics_profiles.retain(|p| p.name != name);
    if candidate.graphics_profiles.len() == old {
        return Err("Unknown saved graphics profile".into());
    }
    if candidate.graphics_profile.as_deref() == Some(id) {
        candidate.graphics_profile = None;
    }
    Ok(candidate)
}

fn persist_candidate(
    current: &mut UiConfig,
    candidate: UiConfig,
    persist: impl FnOnce(&UiConfig) -> Result<(), String>,
) -> Result<(), String> {
    persist(&candidate)?;
    *current = candidate;
    Ok(())
}

fn record_confirmation(
    running: Option<&mut UiConfig>,
    pending: &mut BTreeSet<&'static str>,
    candidate: &UiConfig,
    accepted: bool,
) {
    if accepted {
        for key in ["graphics_profile", "quality", "encoder"] {
            pending.remove(key);
        }
        if let Some(running) = running {
            assign(running, &settings(candidate));
            running.graphics_profile = candidate.graphics_profile.clone();
        }
    } else {
        pending.insert("graphics_profile");
    }
}

#[tauri::command]
pub async fn get_graphics_profiles(
    state: State<'_, Arc<AppState>>,
) -> Result<GraphicsProfiles, String> {
    let config = state.ui_config.lock().await;
    Ok(snapshot(&config))
}

#[tauri::command]
pub async fn save_graphics_profile(
    name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<GraphicsProfiles, String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    let mut config = state.ui_config.lock().await;
    let candidate = save_current(&config, &name)?;
    persist_candidate(&mut config, candidate, UiConfig::save)?;
    Ok(snapshot(&config))
}

#[tauri::command]
pub async fn delete_graphics_profile(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<GraphicsProfiles, String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    let mut config = state.ui_config.lock().await;
    let candidate = remove_saved(&config, &id)?;
    persist_candidate(&mut config, candidate, UiConfig::save)?;
    Ok(snapshot(&config))
}

/// None retries the entire current durable target after an ambiguous worker
/// acknowledgement, preserving manual overrides rather than reloading a preset.
#[tauri::command]
pub async fn apply_graphics_profile(
    id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<AppliedGraphicsProfile, String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    let (candidate, reconnect) = {
        let mut config = state.ui_config.lock().await;
        let candidate = selected_candidate(&config, id.as_deref())?;
        let reconnect = needs_reconnect(&config, &candidate);
        persist_candidate(&mut config, candidate.clone(), UiConfig::save)?;
        (candidate, reconnect)
    };
    let live = {
        let guard = state.server_handle.lock().await;
        if let Some(handle) = guard.as_ref().filter(|h| accepts_live_updates(&h.status())) {
            Some(
                handle
                    .update_config(ConfigUpdate::Graphics(settings(&candidate)))
                    .await
                    .map_err(|e| e.to_string()),
            )
        } else {
            None
        }
    };
    match live {
        Some(Ok(())) => {
            let mut running = state.running_config.lock().await;
            record_confirmation(
                running.as_mut(),
                &mut *state.unconfirmed_graphics.lock().await,
                &candidate,
                true,
            );
        }
        Some(Err(error)) => {
            record_confirmation(
                None,
                &mut *state.unconfirmed_graphics.lock().await,
                &candidate,
                false,
            );
            // Worker timeouts are ambiguous. Keep the durable target for one
            // idempotent retry; never stop/reconnect or roll it back mid-session.
            return Err(format!("Profile saved, but the server did not confirm it: {error}. Retry the saved graphics settings."));
        }
        None => {}
    }
    let active = state
        .server_handle
        .lock()
        .await
        .as_ref()
        .is_some_and(|h| accepts_live_updates(&h.status()));
    let running = state.running_config.lock().await;
    let unconfirmed = state.unconfirmed_graphics.lock().await;
    Ok(AppliedGraphicsProfile {
        profiles: snapshot(&candidate),
        configuration: configuration_status(
            &candidate,
            running.as_ref().filter(|_| active),
            &unconfirmed,
        ),
        live_applied: matches!(live, Some(Ok(()))),
        requires_reconnect: active && reconnect,
    })
}

#[cfg(test)]
mod tests;
