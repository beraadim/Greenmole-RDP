//! Saved preferences and the settings accepted by the running service.
//! Configuration transitions hold the lifecycle lock; reporting never exposes
//! credential values or private paths from either snapshot.
use crate::{commands::stop_running_server, state::AppState, ui_config::UiConfig};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct ConfigurationStatus {
    pub running: bool,
    pub restart_required: bool,
    pub pending_restart: Vec<&'static str>,
}

/// Compare against the service snapshot, not the previous saved value: reverting
/// a setting cancels its pending restart, and unrelated live edits cannot hide it.
pub(crate) fn configuration_status(
    saved: &UiConfig,
    running: Option<&UiConfig>,
) -> ConfigurationStatus {
    let mut pending_restart = Vec::new();
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
    }
    ConfigurationStatus {
        running: running.is_some(),
        restart_required: !pending_restart.is_empty(),
        pending_restart,
    }
}

#[tauri::command]
pub async fn get_configuration_status(
    state: State<'_, Arc<AppState>>,
) -> Result<ConfigurationStatus, String> {
    let _lifecycle = state.lifecycle_lock.lock().await;
    let is_running = state
        .server_handle
        .lock()
        .await
        .as_ref()
        .is_some_and(|h| h.status().running);
    let saved = state.ui_config.lock().await;
    let running = state.running_config.lock().await;
    Ok(configuration_status(
        &saved,
        running.as_ref().filter(|_| is_running),
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
        .filter(|handle| handle.status().running)
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
    let live_result =
        if !restart_required {
            let guard = state.server_handle.lock().await;
            if let Some(handle) = guard.as_ref().filter(|handle| handle.status().running) {
                let config = state.ui_config.lock().await;
                match key.as_str() {
                    "frame_rate" => handle
                        .update_config(macrdp_core::ConfigUpdate::FrameRate(config.frame_rate)),
                    "bitrate_mbps" => handle.update_config(macrdp_core::ConfigUpdate::BitrateKbps(
                        config
                            .bitrate_mbps
                            .checked_mul(1000)
                            .ok_or("Bitrate is out of range")?,
                    )),
                    "log_level" => handle.update_config(macrdp_core::ConfigUpdate::LogLevel(
                        config.log_level.clone(),
                    )),
                    "show_cursor" => handle
                        .update_config(macrdp_core::ConfigUpdate::ShowCursor(config.show_cursor)),
                    "resolution" | "hidpi_scale" => handle.update_config(
                        macrdp_core::ConfigUpdate::Resolution(config.resolution.clone()),
                    ),
                    "encoder" => handle
                        .update_config(macrdp_core::ConfigUpdate::Encoder(config.encoder.clone())),
                    "chroma_mode" => handle.update_config(macrdp_core::ConfigUpdate::ChromaMode(
                        config.chroma_mode.clone(),
                    )),
                    "gfx_validation_mode" => handle.update_config(
                        macrdp_core::ConfigUpdate::GfxValidationMode(config.gfx_validation_mode),
                    ),
                    "username" | "password" => {
                        handle.update_config(macrdp_core::ConfigUpdate::Credentials {
                            username: config.username.clone(),
                            password: config.password.clone(),
                        })
                    }
                    _ => Ok(()),
                }
                .map_err(|error| error.to_string())
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };
    if let Err(error) = live_result {
        // Preferences are already durably saved. Stop the old listener rather than
        // leaving it accepting an obsolete password after a rejected live update.
        tracing::error!(%key, %error, "Saved setting could not be applied; stopping server");
        if let Err(stop_error) = stop_running_server(&app, state.inner()).await {
            tracing::error!(%stop_error, "Cleanup after failed settings update");
        }
        return Err(format!("Setting saved, but the server was stopped because it could not apply the change: {error}. Start the server to use the saved settings."));
    }

    if !restart_required {
        if let Some(running) = state.running_config.lock().await.as_mut() {
            running.set_field(&key, &value)?;
        }
    }
    let saved = state.ui_config.lock().await.clone();
    let running = state.running_config.lock().await;
    let status = configuration_status(
        &saved,
        running.as_ref().filter(|_| running_backend.is_some()),
    );
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pending_restart_survives_live_edits_and_clears_on_revert_or_start() {
        let mut running = UiConfig::default();
        let mut saved = running.clone();
        saved.set_field("audio_enabled", &json!(false)).unwrap();
        saved.set_field("port", &json!(13389)).unwrap();
        for _ in 0..2 {
            let status = configuration_status(&saved, Some(&running));
            assert_eq!(status.pending_restart, ["port", "audio_enabled"]);
        }
        saved.set_field("frame_rate", &json!(30)).unwrap();
        running.set_field("frame_rate", &json!(30)).unwrap();
        assert!(configuration_status(&saved, Some(&running)).restart_required);
        saved.set_field("port", &json!(running.port)).unwrap();
        assert_eq!(
            configuration_status(&saved, Some(&running)).pending_restart,
            ["audio_enabled"]
        );
        assert!(!configuration_status(&saved, Some(&saved)).restart_required);
        assert!(!configuration_status(&saved, None).restart_required);
        saved.audio_enabled = running.audio_enabled;
        assert!(!configuration_status(&saved, Some(&running)).restart_required);
    }

    #[test]
    fn virtual_resolution_and_legacy_preferences_are_handled_without_exposing_values() {
        let mut running = UiConfig::default();
        running.display_backend = macrdp_core::DisplayBackend::Virtual;
        let mut saved = running.clone();
        saved.resolution = "1920x1080".into();
        saved.tls_key_path = "private-key-path".into();
        saved.password = "private-password".into();
        let status = configuration_status(&saved, Some(&running));
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
            configuration_status(&saved, Some(&running)).pending_restart,
            ["resolution"]
        );
        let mut applied = running;
        applied
            .set_field("resolution", &json!("1920x1080"))
            .unwrap();
        assert!(!configuration_status(&saved, Some(&applied)).restart_required);
    }
}
