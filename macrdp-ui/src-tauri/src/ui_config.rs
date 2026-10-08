//! Independent UI configuration with persistent storage at ~/.macrdp/config-ui.toml

use std::path::PathBuf;

use macrdp_core::DisplayBackend;
use serde::{Deserialize, Serialize};

/// UI-specific configuration. All fields have sensible defaults so
/// `UiConfig::default()` is always usable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    // ── Server ──────────────────────────────────────────────
    pub port: u16,
    pub frame_rate: u32,
    pub bitrate_mbps: u32,
    pub encoder: String,
    pub chroma_mode: String,
    pub gfx_validation_mode: bool,
    pub bind_address: String,
    pub max_connections: u32,
    pub idle_timeout_secs: u64,

    // ── Auth ────────────────────────────────────────────────
    pub username: String,
    pub password: String,

    // ── Display ─────────────────────────────────────────────
    pub display_backend: DisplayBackend,
    #[serde(alias = "hidpi_scale")]
    pub resolution: String,
    pub show_cursor: bool,

    // ── Application ─────────────────────────────────────────
    pub log_level: String,
    pub theme: String,
    pub autostart: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            port: 3389,
            frame_rate: 60,
            bitrate_mbps: 50,
            encoder: "auto".to_string(),
            chroma_mode: "avc420".to_string(),
            gfx_validation_mode: false,
            bind_address: "0.0.0.0".to_string(),
            max_connections: 3,
            idle_timeout_secs: 1800,
            username: "macrdp".to_string(),
            password: String::new(),
            display_backend: DisplayBackend::Main,
            resolution: "auto".to_string(),
            show_cursor: false,
            log_level: "warn".to_string(),
            theme: "system".to_string(),
            autostart: false,
        }
    }
}

/// Returns the config file path: `~/.macrdp/config-ui.toml`
pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".macrdp")
        .join("config-ui.toml")
}

impl UiConfig {
    /// Load from `~/.macrdp/config-ui.toml`, creating defaults if the file
    /// does not exist or is unparseable.
    pub fn load() -> Result<Self, String> {
        let path = config_path();
        if !path.exists() {
            let cfg = Self::default();
            // Try to persist the defaults so the user has a file to edit.
            let _ = cfg.save();
            return Ok(cfg);
        }
        let content = std::fs::read_to_string(&path).map_err(|e| format!("read config: {e}"))?;
        toml::from_str(&content).map_err(|e| format!("parse config: {e}"))
    }

    /// Serialize to TOML and write to `~/.macrdp/config-ui.toml`.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
        }
        let content = toml::to_string_pretty(self).map_err(|e| format!("serialize config: {e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("write config: {e}"))
    }

    /// Update a single field by key name from a JSON value.
    /// Display backend changes require a restart. Changing the initial
    /// virtual display resolution also requires a restart so `auto` can resume
    /// following client requests. Virtual display port changes require a restart
    /// to avoid creating overlapping display owners. Other settings use their live update
    /// paths; these flags never restart a display during an ordinary client resize.
    pub fn set_field(&mut self, key: &str, value: &serde_json::Value) -> Result<bool, String> {
        match key {
            "port" => {
                self.port = value.as_u64().ok_or("port must be a number")? as u16;
            }
            "frame_rate" => {
                self.frame_rate = value.as_u64().ok_or("frame_rate must be a number")? as u32;
            }
            "bitrate_mbps" => {
                self.bitrate_mbps = value.as_u64().ok_or("bitrate_mbps must be a number")? as u32;
            }
            "encoder" => {
                self.encoder = value
                    .as_str()
                    .ok_or("encoder must be a string")?
                    .to_string();
            }
            "chroma_mode" => {
                self.chroma_mode = value
                    .as_str()
                    .ok_or("chroma_mode must be a string")?
                    .to_string();
            }
            "gfx_validation_mode" => {
                self.gfx_validation_mode = value
                    .as_bool()
                    .ok_or("gfx_validation_mode must be a boolean")?;
            }
            "bind_address" => {
                self.bind_address = value
                    .as_str()
                    .ok_or("bind_address must be a string")?
                    .to_string();
            }
            "max_connections" => {
                self.max_connections =
                    value.as_u64().ok_or("max_connections must be a number")? as u32;
            }
            "idle_timeout_secs" => {
                self.idle_timeout_secs =
                    value.as_u64().ok_or("idle_timeout_secs must be a number")?;
            }
            "username" => {
                self.username = value
                    .as_str()
                    .ok_or("username must be a string")?
                    .to_string();
            }
            "password" => {
                self.password = value
                    .as_str()
                    .ok_or("password must be a string")?
                    .to_string();
            }
            "display_backend" => {
                self.display_backend = serde_json::from_value(value.clone())
                    .map_err(|_| "display_backend must be main or virtual")?;
            }
            "resolution" | "hidpi_scale" => {
                self.resolution = value
                    .as_str()
                    .or_else(|| value.as_u64().map(|_| ""))
                    .ok_or("resolution must be a string")?
                    .to_string();
                if self.resolution.is_empty() {
                    self.resolution = value.as_u64().unwrap().to_string();
                }
            }
            "show_cursor" => {
                self.show_cursor = value.as_bool().ok_or("show_cursor must be a boolean")?;
            }
            "log_level" => {
                self.log_level = value
                    .as_str()
                    .ok_or("log_level must be a string")?
                    .to_string();
            }
            "theme" => {
                self.theme = value.as_str().ok_or("theme must be a string")?.to_string();
            }
            "autostart" => {
                self.autostart = value.as_bool().ok_or("autostart must be a boolean")?;
            }
            _ => return Err(format!("unknown config key: {key}")),
        }

        Ok(self.requires_restart(key, None))
    }

    /// A pending switch away from a virtual display must not permit port
    /// migration to replace its owner before the user restarts the service.
    pub fn requires_restart(&self, key: &str, running_backend: Option<DisplayBackend>) -> bool {
        key == "display_backend"
            || (matches!(key, "resolution" | "hidpi_scale" | "port")
                && (self.display_backend == DisplayBackend::Virtual
                    || running_backend == Some(DisplayBackend::Virtual)))
    }

    /// Convert to the core library's `ServerConfig` used to start the server.
    pub fn to_server_config(&self) -> macrdp_core::ServerConfig {
        macrdp_core::ServerConfig {
            port: self.port,
            bind_address: Some(self.bind_address.clone()),
            frame_rate: self.frame_rate,
            display_backend: self.display_backend,
            width: 0,  // auto-detect
            height: 0, // auto-detect
            username: Some(self.username.clone()),
            password: if self.password.is_empty() {
                None
            } else {
                Some(self.password.clone())
            },
            cert_path: None,
            key_path: None,
            idle_timeout_secs: self.idle_timeout_secs,
            log_level: Some(self.log_level.clone()),
            quality: None,
            encoder: Some(self.encoder.clone()),
            chroma_mode: Some(self.chroma_mode.clone()),
            gfx_validation_mode: Some(self.gfx_validation_mode),
            resolution: Some(self.resolution.clone()),
            show_cursor: Some(self.show_cursor),
            bitrate_mbps: Some(self.bitrate_mbps),
            audio: macrdp_core::AudioConfig::default(),
            clipboard: macrdp_core::ClipboardConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn display_settings_preserve_legacy_defaults_and_round_trip() {
        let mut config: UiConfig = toml::from_str("resolution = '1920x1080'").unwrap();
        assert_eq!(config.display_backend, DisplayBackend::Main);
        assert!(config
            .set_field("display_backend", &json!("virtual"))
            .unwrap());
        let saved = toml::to_string(&config).unwrap();
        let restored: UiConfig = toml::from_str(&saved).unwrap();
        let core = restored.to_server_config();
        assert_eq!(core.display_backend, DisplayBackend::Virtual);
        assert_eq!(core.resolution.as_deref(), Some("1920x1080"));
    }

    #[test]
    fn invalid_display_updates_leave_existing_selection_unchanged() {
        let mut config = UiConfig::default();
        assert!(config
            .set_field("display_backend", &json!("unknown"))
            .is_err());
        assert!(config.set_field("display_backend", &json!(1)).is_err());
        assert_eq!(config.display_backend, DisplayBackend::Main);
        assert!(toml::from_str::<UiConfig>("display_backend = 'unknown'").is_err());
    }

    #[test]
    fn virtual_resolution_settings_require_restart_but_live_settings_do_not() {
        let mut config = UiConfig::default();
        assert!(!config.set_field("resolution", &json!("1920x1080")).unwrap());
        config
            .set_field("display_backend", &json!("virtual"))
            .unwrap();
        assert!(config.set_field("resolution", &json!("1920x1080")).unwrap());
        assert!(config.set_field("resolution", &json!("auto")).unwrap());
        assert!(config.set_field("hidpi_scale", &json!(1)).unwrap());
        assert!(config.set_field("port", &json!(13389)).unwrap());
        assert!(!config.set_field("frame_rate", &json!(30)).unwrap());
        assert!(!config.set_field("show_cursor", &json!(true)).unwrap());
        config.set_field("display_backend", &json!("main")).unwrap();
        assert!(!config.set_field("port", &json!(3389)).unwrap());
        assert!(config.requires_restart("port", Some(DisplayBackend::Virtual)));
        assert!(config.requires_restart("resolution", Some(DisplayBackend::Virtual)));
        assert!(!config.requires_restart("port", Some(DisplayBackend::Main)));
        assert!(!config.requires_restart("frame_rate", Some(DisplayBackend::Virtual)));
    }
}
