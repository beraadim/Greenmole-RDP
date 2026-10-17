//! Saved preferences and the settings accepted by the running service.
//! Configuration transitions hold the lifecycle lock; reporting never exposes
//! credential values or private paths from either snapshot.
use crate::{commands::stop_running_server, state::AppState, ui_config::UiConfig};
use serde::Serialize;
use std::{collections::BTreeSet, sync::Arc};
use tauri::State;

#[derive(Debug, Serialize)]
pub struct ConfigurationStatus {
    pub running: bool,
    pub restart_required: bool,
    pub pending_restart: Vec<&'static str>,
    pub pending_live: Vec<&'static str>,
}

/// Compare against the service snapshot, not the previous saved value: reverting
/// a setting cancels its pending restart, and unrelated live edits cannot hide it.
pub(crate) fn configuration_status(
    saved: &UiConfig,
    running: Option<&UiConfig>,
    unconfirmed: &BTreeSet<&'static str>,
) -> ConfigurationStatus {
    let mut pending_restart = Vec::new();
    let mut pending_live = Vec::new();
    if let Some(running) = running {
        let saved_values = serde_json::to_value(saved).expect("UiConfig is serializable");
        let running_values = serde_json::to_value(running).expect("UiConfig is serializable");
        for key in [
            "display_backend",
            "bind_address",
            "port",
            "idle_timeout_secs",
            "tls_cert_path",
            "tls_key_path",
            "tls_server_names",
            "audio_enabled",
            "clipboard_enabled",
            "clipboard_file_transfer",
            "clipboard_max_file_size_mb",
            "resolution",
        ] {
            // Even main-display resolution can be pending after selecting a
            // virtual display, editing resolution, then reverting the backend.
            // Accepted live resolution edits already update the running snapshot.
            if saved_values[key] != running_values[key] {
                pending_restart.push(key);
            }
        }
        for key in ["quality", "encoder"] {
            if saved_values[key] != running_values[key] || unconfirmed.contains(key) {
                pending_live.push(key);
            }
        }
    }
    ConfigurationStatus {
        running: running.is_some(),
        restart_required: !pending_restart.is_empty(),
        pending_restart,
        pending_live,
    }
}

#[tauri::command]
pub async fn get_configuration_status(
    state: State<'_, Arc<AppState>>,
) -> Result<ConfigurationStatus, String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    let accepts_updates = state
        .server_handle
        .lock()
        .await
        .as_ref()
        .is_some_and(|handle| accepts_live_updates(&handle.status()));
    let saved = state.ui_config.lock().await;
    let running = state.running_config.lock().await;
    let unconfirmed = state.unconfirmed_graphics.lock().await;
    Ok(configuration_status(
        &saved,
        running.as_ref().filter(|_| accepts_updates),
        &unconfirmed,
    ))
}

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let config = state.ui_config.lock().await.clone();
    serde_json::to_value(&config).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_config(
    app: tauri::AppHandle,
    key: String,
    value: serde_json::Value,
    state: State<'_, Arc<AppState>>,
) -> Result<ConfigurationStatus, String> {
    tracing::info!(%key, "Updating configuration");
    let _lifecycle = state.lifecycle_lock.lock().await;

    let running_backend = state
        .server_handle
        .lock()
        .await
        .as_ref()
        .filter(|handle| accepts_live_updates(&handle.status()))
        .map(|handle| handle.display_backend());
    let restart_required = {
        let mut config = state.ui_config.lock().await;
        let mut candidate = config.clone();
        candidate.set_field(&key, &value)?;
        candidate.save()?;
        let restart = candidate.requires_restart(&key, running_backend);
        *config = candidate;
        restart
    };

    if key == "log_level" {
        let level = {
            let config = state.ui_config.lock().await;
            config.log_level.clone()
        };
        let filter = format!("macrdp_ui={level},macrdp_core={level},{level}");
        state
            .log_reload
            .reload(tracing_subscriber::EnvFilter::new(&filter))
            .map_err(|e| format!("failed to reload log level: {e}"))?;
        tracing::info!(%level, "Reloaded tracing filter");
    }

    // Display ownership changes apply on the next server start. Persist them
    // without changing the current display or moving its windows mid-session.
    let live_result = if !restart_required {
        let guard = state.server_handle.lock().await;
        if let Some(handle) = guard
            .as_ref()
            .filter(|handle| accepts_live_updates(&handle.status()))
        {
            let config = state.ui_config.lock().await;
            match live_update(&config, &key)? {
                Some(update) => handle
                    .update_config(update)
                    .await
                    .map(|()| true)
                    .map_err(|error| error.to_string()),
                None => Ok(true),
            }
        } else {
            Ok(false)
        }
    } else {
        Ok(false)
    };
    if let Err(error) = &live_result {
        if is_live_graphics_setting(&key) {
            record_graphics_confirmation(
                &mut *state.unconfirmed_graphics.lock().await,
                &key,
                false,
            );
            // A supervisor timeout is ambiguous: its worker may still receive
            // the update. Keep the durable target and allow idempotent retry;
            // rolling it back here could disagree with the active worker.
            tracing::warn!(%key, %error, "Saved graphics setting was not confirmed; retaining active service");
            return Err(format!("Change saved, but the server did not confirm it: {error}. Retry the pending change below."));
        }
        // Preserve the existing fail-closed behavior for credentials: a saved
        // password must never leave a listener accepting an obsolete secret.
        tracing::error!(%key, %error, "Saved setting could not be applied; stopping server");
        if let Err(stop_error) = stop_running_server(&app, state.inner()).await {
            tracing::error!(%stop_error, "Cleanup after failed settings update");
        }
        return Err(format!("Setting saved, but the server was stopped because it could not apply the change: {error}. Start the server to use the saved settings."));
    }

    // A missing/stopped service did not accept anything. Never advance its old
    // snapshot just because a preference was saved or a request was rejected.
    if matches!(live_result, Ok(true)) {
        record_graphics_confirmation(&mut *state.unconfirmed_graphics.lock().await, &key, true);
        if let Some(running) = state.running_config.lock().await.as_mut() {
            running.set_field(&key, &value)?;
        }
    }
    let saved = state.ui_config.lock().await.clone();
    let running = state.running_config.lock().await;
    let unconfirmed = state.unconfirmed_graphics.lock().await;
    let status = configuration_status(
        &saved,
        running.as_ref().filter(|_| running_backend.is_some()),
        &unconfirmed,
    );
    Ok(status)
}

/// Keep IPC routing in one place so every graphics control uses the same
/// acknowledged live-update path as the supervisor and active display worker.
fn live_update(config: &UiConfig, key: &str) -> Result<Option<macrdp_core::ConfigUpdate>, String> {
    use macrdp_core::ConfigUpdate;
    Ok(match key {
        "frame_rate" => Some(ConfigUpdate::FrameRate(config.frame_rate)),
        "bitrate_mbps" => Some(ConfigUpdate::BitrateKbps(
            config
                .bitrate_mbps
                .checked_mul(1000)
                .ok_or("Bitrate is out of range")?,
        )),
        "quality" => Some(ConfigUpdate::Quality(config.quality.clone())),
        "encoder" => Some(ConfigUpdate::Encoder(config.encoder.clone())),
        "chroma_mode" => Some(ConfigUpdate::ChromaMode(config.chroma_mode.clone())),
        "gfx_validation_mode" => Some(ConfigUpdate::GfxValidationMode(config.gfx_validation_mode)),
        "log_level" => Some(ConfigUpdate::LogLevel(config.log_level.clone())),
        "show_cursor" => Some(ConfigUpdate::ShowCursor(config.show_cursor)),
        "resolution" | "hidpi_scale" => Some(ConfigUpdate::Resolution(config.resolution.clone())),
        "username" | "password" => Some(ConfigUpdate::Credentials {
            username: config.username.clone(),
            password: config.password.clone(),
        }),
        _ => None,
    })
}

/// A timeout can be followed by a late application. Comparing preferences
/// alone cannot prove a revert succeeded, so only an accepted retry clears it.
fn record_graphics_confirmation(
    unconfirmed: &mut BTreeSet<&'static str>,
    key: &str,
    accepted: bool,
) {
    let key = match key {
        "quality" => "quality",
        "encoder" => "encoder",
        _ => return,
    };
    if accepted {
        unconfirmed.remove(key);
    } else {
        unconfirmed.insert(key);
    }
}

fn is_live_graphics_setting(key: &str) -> bool {
    matches!(key, "quality" | "encoder")
}

/// A replacement worker must use preferences accepted during automatic
/// recovery, particularly a changed password. The supervisor retains the
/// service until recovery ends even though it temporarily has no listener.
pub(crate) fn accepts_live_updates(status: &macrdp_core::ServerStatus) -> bool {
    status.running || status.state == "recovering"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(saved: &UiConfig, running: Option<&UiConfig>) -> ConfigurationStatus {
        configuration_status(saved, running, &BTreeSet::new())
    }

    #[test]
    fn credentials_and_live_preferences_are_delivered_during_recovery() {
        let mut status = macrdp_core::ServerStatus::default();
        assert!(!accepts_live_updates(&status));
        status.state = "recovering".into();
        assert!(accepts_live_updates(&status));
        for state in ["stopping", "cleanup_pending", "error", "stopped"] {
            status.state = state.into();
            assert!(!accepts_live_updates(&status));
        }
        status.state = "running".into();
        status.running = true;
        assert!(accepts_live_updates(&status));
    }
    #[test]
    fn graphics_preferences_are_live_without_masking_pending_service_changes() {
        let running = UiConfig::default();
        let mut saved = running.clone();
        saved.quality = "lossless".into();
        saved.encoder = "bitmap".into();
        assert!(!status(&saved, Some(&running)).restart_required);
        saved.port += 1;
        assert_eq!(status(&saved, Some(&running)).pending_restart, ["port"]);
        assert!(matches!(live_update(&saved, "quality").unwrap(),
            Some(macrdp_core::ConfigUpdate::Quality(value)) if value == "lossless"));
        assert!(matches!(live_update(&saved, "encoder").unwrap(),
            Some(macrdp_core::ConfigUpdate::Encoder(value)) if value == "bitmap"));
        assert!(live_update(&saved, "port").unwrap().is_none());
    }

    #[test]
    fn unconfirmed_revert_remains_retryable_even_when_desired_matches_last_accepted() {
        let running = UiConfig::default();
        let mut desired = running.clone();
        let mut unconfirmed = BTreeSet::new();
        desired.quality = "high_quality".into();
        record_graphics_confirmation(&mut unconfirmed, "quality", false);
        desired.quality = running.quality.clone();
        record_graphics_confirmation(&mut unconfirmed, "quality", false);
        assert_eq!(desired.quality, running.quality);
        assert_eq!(
            configuration_status(&desired, Some(&running), &unconfirmed).pending_live,
            ["quality"]
        );
        // Stopping hides the notice but cannot turn an ambiguous revert into
        // an acknowledgement. A fresh successful service start clears the set.
        assert!(configuration_status(&desired, None, &unconfirmed)
            .pending_live
            .is_empty());
        assert!(unconfirmed.contains("quality"));
        record_graphics_confirmation(&mut unconfirmed, "encoder", true);
        assert!(unconfirmed.contains("quality"));
        record_graphics_confirmation(&mut unconfirmed, "quality", true);
        assert!(configuration_status(&desired, Some(&running), &unconfirmed)
            .pending_live
            .is_empty());
        record_graphics_confirmation(&mut unconfirmed, "encoder", false);
        unconfirmed.clear();
        assert!(configuration_status(&desired, Some(&running), &unconfirmed)
            .pending_live
            .is_empty());
    }

    #[test]
    fn unconfirmed_graphics_targets_remain_pending_and_can_be_retried() {
        let mut running = UiConfig::default();
        let mut saved = running.clone();
        saved.encoder = "bitmap".into();
        saved.quality = "high_quality".into();
        let pending = status(&saved, Some(&running));
        assert_eq!(pending.pending_live, ["quality", "encoder"]);
        assert!(!pending.restart_required);
        // A rejected/timed-out request leaves the accepted snapshot untouched.
        assert_eq!(
            status(&saved, Some(&running)).pending_live,
            pending.pending_live
        );
        // Re-sending the same durable target routes a fresh live request.
        assert!(matches!(live_update(&saved, "quality").unwrap(),
            Some(macrdp_core::ConfigUpdate::Quality(value)) if value == "high_quality"));
        running.quality = saved.quality.clone();
        assert_eq!(status(&saved, Some(&running)).pending_live, ["encoder"]);
        running.encoder = saved.encoder.clone();
        assert!(status(&saved, Some(&running)).pending_live.is_empty());
        assert!(status(&saved, None).pending_live.is_empty());
        assert!(is_live_graphics_setting("quality") && is_live_graphics_setting("encoder"));
        assert!(!is_live_graphics_setting("username") && !is_live_graphics_setting("password"));
    }
    use serde_json::json;

    #[test]
    fn pending_restart_survives_live_edits_and_clears_on_revert_or_start() {
        let mut running = UiConfig::default();
        let mut saved = running.clone();
        saved.set_field("audio_enabled", &json!(false)).unwrap();
        saved.set_field("port", &json!(13389)).unwrap();
        for _ in 0..2 {
            let status = status(&saved, Some(&running));
            assert_eq!(status.pending_restart, ["port", "audio_enabled"]);
        }
        saved.set_field("frame_rate", &json!(30)).unwrap();
        running.set_field("frame_rate", &json!(30)).unwrap();
        assert!(status(&saved, Some(&running)).restart_required);
        saved.set_field("port", &json!(running.port)).unwrap();
        assert_eq!(
            status(&saved, Some(&running)).pending_restart,
            ["audio_enabled"]
        );
        assert!(!status(&saved, Some(&saved)).restart_required);
        assert!(!status(&saved, None).restart_required);
        saved.audio_enabled = running.audio_enabled;
        assert!(!status(&saved, Some(&running)).restart_required);
    }

    #[test]
    fn virtual_resolution_and_legacy_preferences_are_handled_without_exposing_values() {
        let mut running = UiConfig::default();
        running.display_backend = macrdp_core::DisplayBackend::Virtual;
        let mut saved = running.clone();
        saved.resolution = "1920x1080".into();
        saved.tls_key_path = "private-key-path".into();
        saved.password = "private-password".into();
        let status = status(&saved, Some(&running));
        assert_eq!(status.pending_restart, ["tls_key_path", "resolution"]);
        let text = serde_json::to_string(&status).unwrap();
        assert!(!text.contains("private"));
        let legacy: UiConfig = toml::from_str("max_connections = 3").unwrap();
        assert!(!serde_json::to_value(legacy)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("max_connections"));
        assert!(saved.set_field("max_connections", &json!(3)).is_err());
    }

    #[test]
    fn reverting_backend_keeps_a_previously_deferred_resolution_visible() {
        let running = UiConfig::default();
        let mut saved = running.clone();
        saved
            .set_field("display_backend", &json!("virtual"))
            .unwrap();
        assert!(saved.requires_restart("resolution", Some(running.display_backend)));
        saved.set_field("resolution", &json!("1920x1080")).unwrap();
        saved.set_field("display_backend", &json!("main")).unwrap();
        assert_eq!(
            status(&saved, Some(&running)).pending_restart,
            ["resolution"]
        );
        let mut applied = running;
        applied
            .set_field("resolution", &json!("1920x1080"))
            .unwrap();
        assert!(!status(&saved, Some(&applied)).restart_required);
    }
}
