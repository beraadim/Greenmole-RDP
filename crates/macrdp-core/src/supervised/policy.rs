//! Restart limits and configuration replay are independent of native capture.
#[cfg(test)]
use super::protocol::Initialization;
use super::{MAX_RESTARTS, RESTART_WINDOW};
use crate::{ConfigUpdate, ServerConfig};
use std::collections::VecDeque;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

pub(super) fn apply_update(config: &mut ServerConfig, update: &ConfigUpdate) {
    match update {
        ConfigUpdate::Graphics(settings) => {
            config.frame_rate = settings.frame_rate;
            config.bitrate_mbps = Some(settings.bitrate_mbps);
            config.quality = Some(settings.quality.clone());
            config.encoder = Some(settings.encoder.clone());
            config.chroma_mode = Some(settings.chroma_mode.clone());
        }
        ConfigUpdate::FrameRate(value) => config.frame_rate = *value,
        ConfigUpdate::BitrateKbps(value) => config.bitrate_mbps = Some(value.div_ceil(1000)),
        ConfigUpdate::LogLevel(value) => config.log_level = Some(value.clone()),
        ConfigUpdate::ShowCursor(value) => config.show_cursor = Some(*value),
        ConfigUpdate::Resolution(value) => config.resolution = Some(value.clone()),
        ConfigUpdate::Encoder(value) => config.encoder = Some(value.clone()),
        ConfigUpdate::Quality(value) => config.quality = Some(value.clone()),
        ConfigUpdate::ChromaMode(value) => config.chroma_mode = Some(value.clone()),
        ConfigUpdate::GfxValidationMode(value) => config.gfx_validation_mode = Some(*value),
        ConfigUpdate::Credentials { username, password } => {
            config.username = Some(username.clone());
            config.password = Some(password.clone());
        }
    }
}

pub(super) fn allow_restart(restarts: &mut VecDeque<Instant>, now: Instant) -> bool {
    while restarts
        .front()
        .is_some_and(|time| now.duration_since(*time) >= RESTART_WINDOW)
    {
        restarts.pop_front();
    }
    if restarts.len() >= MAX_RESTARTS {
        return false;
    }
    restarts.push_back(now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_recovery_is_bounded_and_recovers_budget_after_stable_period() {
        let now = Instant::now();
        let mut history = VecDeque::new();
        assert!(allow_restart(&mut history, now));
        assert!(allow_restart(&mut history, now + Duration::from_secs(1)));
        assert!(!allow_restart(&mut history, now + Duration::from_secs(2)));
        assert!(allow_restart(&mut history, now + RESTART_WINDOW));
    }

    #[test]
    fn replacement_receives_updated_credentials_and_capture_configuration() {
        let mut config = ServerConfig::default();
        apply_update(
            &mut config,
            &ConfigUpdate::Credentials {
                username: "replacement-user".into(),
                password: "replacement-secret".into(),
            },
        );
        apply_update(&mut config, &ConfigUpdate::FrameRate(45));
        apply_update(&mut config, &ConfigUpdate::BitrateKbps(123_000));
        apply_update(&mut config, &ConfigUpdate::Quality("lossless".into()));
        apply_update(&mut config, &ConfigUpdate::Encoder("bitmap".into()));
        assert_eq!(config.username.as_deref(), Some("replacement-user"));
        assert_eq!(config.password.as_deref(), Some("replacement-secret"));
        assert_eq!(config.frame_rate, 45);
        assert_eq!(config.bitrate_mbps, Some(123));
        assert_eq!(config.quality.as_deref(), Some("lossless"));
        assert_eq!(config.encoder.as_deref(), Some("bitmap"));
    }

    #[test]
    fn grouped_graphics_recovery_replay_preserves_every_unrelated_preference() {
        let mut config = ServerConfig::default();
        config.port = 3395;
        config.reuse_virtual_display = true;
        config.private_session = true;
        config.display_backend = crate::DisplayBackend::Virtual;
        config.username = Some("fixture-user".into());
        config.password = Some("fixture-secret".into());
        let mut expected = serde_json::to_value(&config).unwrap();
        let settings = crate::GraphicsSettings {
            frame_rate: 30,
            bitrate_mbps: 8,
            quality: "balanced".into(),
            encoder: "auto".into(),
            chroma_mode: "avc444".into(),
        };
        for (key, value) in [
            ("frame_rate", serde_json::json!(30)),
            ("bitrate_mbps", serde_json::json!(8)),
            ("quality", serde_json::json!("balanced")),
            ("encoder", serde_json::json!("auto")),
            ("chroma_mode", serde_json::json!("avc444")),
        ] {
            expected[key] = value;
        }
        apply_update(&mut config, &ConfigUpdate::Graphics(settings));
        assert_eq!(serde_json::to_value(&config).unwrap(), expected);
    }

    #[test]
    fn helper_initialization_preserves_embedder_selected_tls_directory() {
        let directory = std::path::PathBuf::from("/private/example/macrdp/tls");
        let mut config = ServerConfig::default();
        config.managed_tls_directory = Some(directory.clone());
        let init = Initialization {
            managed_tls_directory: config.managed_tls_directory.clone(),
            config,
            baseline_display_ids: vec![],
            perf_enabled: false,
        };
        let bytes = serde_json::to_vec(&init).unwrap();
        let decoded: Initialization = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.managed_tls_directory, Some(directory));
    }
}
