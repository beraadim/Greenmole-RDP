//! Independent UI configuration with persistent storage at ~/.macrdp/config-ui.toml

use std::path::{Path, PathBuf};

use macrdp_core::DisplayBackend;
use serde::{Deserialize, Serialize};

/// UI preferences and the in-memory credential. `load()` supplies a Keychain-backed
/// password; defaults alone intentionally cannot start an authenticated server.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    // ── Server ──────────────────────────────────────────────
    pub port: u16,
    pub frame_rate: u32,
    pub bitrate_mbps: u32,
    pub quality: String,
    pub encoder: String,
    pub chroma_mode: String,
    pub gfx_validation_mode: bool,
    /// Optional named graphics preferences; never applied automatically.
    pub graphics_profiles: Vec<crate::graphics_profiles::SavedGraphicsProfile>,
    pub graphics_profile: Option<String>,
    pub bind_address: String,
    pub idle_timeout_secs: u64,

    // ── Auth ────────────────────────────────────────────────
    pub username: String,
    pub password: String,
    pub tls_cert_path: String,
    pub tls_key_path: String,
    pub tls_server_names: Vec<String>,

    // ── Display ─────────────────────────────────────────────
    pub display_backend: DisplayBackend,
    pub reuse_virtual_display: bool,
    pub private_session: bool,
    pub keyboard_layout: String,
    pub keyboard_shortcuts: String,
    #[serde(alias = "hidpi_scale")]
    pub resolution: String,
    pub show_cursor: bool,

    // Audio and clipboard channels are configured at service startup.
    pub audio_enabled: bool,
    pub clipboard_enabled: bool,
    pub clipboard_file_transfer: bool,
    pub clipboard_max_file_size_mb: u32,

    // ── Application ─────────────────────────────────────────
    pub log_level: String,
    pub theme: String,
    pub autostart: bool,
}

impl std::fmt::Debug for UiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiConfig")
            .field("port", &self.port)
            .field("password", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            port: 3389,
            frame_rate: 60,
            bitrate_mbps: 50,
            quality: "balanced".into(),
            encoder: "auto".to_string(),
            chroma_mode: "avc420".to_string(),
            gfx_validation_mode: false,
            graphics_profiles: Vec::new(),
            graphics_profile: None,
            bind_address: "0.0.0.0".to_string(),
            idle_timeout_secs: 1800,
            username: "macrdp".to_string(),
            password: String::new(),
            tls_cert_path: String::new(),
            tls_key_path: String::new(),
            tls_server_names: Vec::new(),
            display_backend: DisplayBackend::Main,
            reuse_virtual_display: false,
            private_session: false,
            keyboard_layout: "auto".into(),
            keyboard_shortcuts: "auto".into(),
            resolution: "auto".to_string(),
            show_cursor: false,
            audio_enabled: true,
            clipboard_enabled: true,
            clipboard_file_transfer: true,
            clipboard_max_file_size_mb: 100,
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

fn integer_in_range(
    value: &serde_json::Value,
    key: &str,
    minimum: u64,
    maximum: u64,
) -> Result<u64, String> {
    let number = value
        .as_u64()
        .ok_or_else(|| format!("{key} must be a nonnegative integer"))?;
    if !(minimum..=maximum).contains(&number) {
        return Err(format!("{key} must be between {minimum} and {maximum}"));
    }
    Ok(number)
}

impl UiConfig {
    fn validate_keyboard_preferences(&self) -> Result<(), String> {
        macrdp_core::config::keyboard::validate_layout(&self.keyboard_layout)
            .and_then(|_| {
                macrdp_core::config::keyboard::validate_shortcuts(&self.keyboard_shortcuts)
            })
            .map_err(|error| error.to_string())
    }

    /// Load configuration and migrate legacy plaintext passwords to Keychain.
    /// A locked or unavailable Keychain is an error, never permission to replace credentials.
    pub fn load() -> Result<Self, String> {
        Self::load_with_store(
            &config_path(),
            || macrdp_core::credentials::load_desktop_password().map_err(|e| e.to_string()),
            |password| {
                macrdp_core::credentials::save_desktop_password(password).map_err(|e| e.to_string())
            },
            || macrdp_core::credentials::delete_desktop_password().map_err(|e| e.to_string()),
        )
    }

    fn load_with_store(
        path: &Path,
        load_password: impl FnOnce() -> Result<Option<String>, String>,
        save_password: impl Fn(&str) -> Result<(), String>,
        delete_password: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut config = if path.exists() {
            macrdp_core::credentials::protect_config_file(path).map_err(|e| e.to_string())?;
            let content = std::fs::read_to_string(path).map_err(|e| format!("read config: {e}"))?;
            // TOML errors may include the source line containing a password.
            toml::from_str::<Self>(&content)
                .map_err(|_| "Invalid configuration file".to_string())?
        } else {
            Self::default()
        };
        config.validate_keyboard_preferences()?;
        crate::graphics_profiles::validate_saved(&config.graphics_profiles)?;
        let previous_password = load_password()?;
        if config.password.is_empty() {
            config.password = match previous_password.clone() {
                Some(password) if !password.is_empty() => password,
                _ => macrdp_core::credentials::generate_password().map_err(|e| e.to_string())?,
            };
        }
        config.save_with_store(
            path,
            previous_password.as_deref(),
            save_password,
            delete_password,
        )?;
        Ok(config)
    }

    /// Keep the password in Keychain; the TOML file contains preferences only.
    pub fn save(&self) -> Result<(), String> {
        let previous =
            macrdp_core::credentials::load_desktop_password().map_err(|e| e.to_string())?;
        self.save_with_store(
            &config_path(),
            previous.as_deref(),
            |password| {
                macrdp_core::credentials::save_desktop_password(password).map_err(|e| e.to_string())
            },
            || macrdp_core::credentials::delete_desktop_password().map_err(|e| e.to_string()),
        )
    }

    fn save_with_store(
        &self,
        path: &Path,
        previous_password: Option<&str>,
        save_password: impl Fn(&str) -> Result<(), String>,
        delete_password: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.validate_keyboard_preferences()?;
        crate::graphics_profiles::validate_saved(&self.graphics_profiles)?;
        if self.private_session && self.display_backend != DisplayBackend::Virtual {
            return Err("Guarded private sessions require the virtual display backend".into());
        }
        if self.reuse_virtual_display && self.display_backend != DisplayBackend::Virtual {
            return Err("Reusing a virtual display requires the virtual display backend".into());
        }
        if self.password.is_empty() || self.username.trim().is_empty() {
            return Err("A nonempty username and password are required".to_string());
        }
        let mut preferences =
            toml::Value::try_from(self).map_err(|e| format!("serialize config: {e}"))?;
        preferences
            .as_table_mut()
            .ok_or("Configuration is not a table")?
            .remove("password");
        let content =
            toml::to_string_pretty(&preferences).map_err(|e| format!("serialize config: {e}"))?;
        let preferences_changed =
            std::fs::read(path).map_or(true, |current| current != content.as_bytes());
        let password_changed = previous_password != Some(self.password.as_str());
        if password_changed {
            save_password(&self.password)?;
        }
        // Password-only updates have no subsequent filesystem operation that can fail.
        if preferences_changed {
            if let Err(error) =
                macrdp_core::credentials::write_private_file(path, content.as_bytes())
            {
                if password_changed {
                    let rollback = match previous_password {
                        Some(previous) => save_password(previous),
                        None => delete_password(),
                    };
                    if let Err(rollback_error) = rollback {
                        return Err(format!("Preferences were not saved ({error}); credential rollback also failed ({rollback_error})"));
                    }
                }
                return Err(error.to_string());
            }
        }
        Ok(())
    }

    /// Update a single field by key name from a JSON value.
    /// Listener, channel policy, idle timeout, display backend and private-session changes require a restart. Changing the initial
    /// virtual display resolution also requires a restart so `auto` can resume
    /// following client requests. Virtual display port changes require a restart
    /// to avoid creating overlapping display owners. Other settings use their live update
    /// paths; these flags never restart a display during an ordinary client resize.
    pub fn set_field(&mut self, key: &str, value: &serde_json::Value) -> Result<bool, String> {
        let previous_graphics = crate::graphics_profiles::settings(self);
        match key {
            "keyboard_layout" => {
                let layout = value.as_str().ok_or("keyboard_layout must be a string")?;
                macrdp_core::config::keyboard::validate_layout(layout)
                    .map_err(|error| error.to_string())?;
                self.keyboard_layout = layout.into();
            }
            "keyboard_shortcuts" => {
                let shortcuts = value
                    .as_str()
                    .ok_or("keyboard_shortcuts must be a string")?;
                macrdp_core::config::keyboard::validate_shortcuts(shortcuts)
                    .map_err(|error| error.to_string())?;
                self.keyboard_shortcuts = shortcuts.into();
            }
            "port" => {
                self.port = integer_in_range(value, key, 1, u16::MAX as u64)? as u16;
            }
            "frame_rate" => {
                self.frame_rate = integer_in_range(value, key, 1, 240)? as u32;
            }
            "bitrate_mbps" => {
                self.bitrate_mbps = integer_in_range(value, key, 1, 1000)? as u32;
            }
            "encoder" => {
                let encoder = value.as_str().ok_or("encoder must be a string")?;
                if !matches!(encoder, "auto" | "software" | "hardware" | "bitmap") {
                    return Err("Unknown encoder selection".into());
                }
                self.encoder = encoder.into();
            }
            "quality" => {
                let quality = value.as_str().ok_or("quality must be a string")?;
                if !matches!(
                    quality,
                    "low_latency" | "balanced" | "high_quality" | "lossless"
                ) {
                    return Err("Unknown image quality preset".into());
                }
                self.quality = quality.into();
            }
            "chroma_mode" => {
                let chroma = value.as_str().ok_or("chroma_mode must be a string")?;
                if !matches!(chroma, "avc420" | "avc444") {
                    return Err("Unknown video color detail selection".into());
                }
                self.chroma_mode = chroma.into();
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
            "idle_timeout_secs" => {
                self.idle_timeout_secs =
                    value.as_u64().ok_or("idle_timeout_secs must be a number")?;
            }
            "username" => {
                let username = value
                    .as_str()
                    .ok_or("username must be a string")?
                    .to_string();
                if username.trim().is_empty() {
                    return Err("Username cannot be empty".to_string());
                }
                self.username = username;
            }
            "password" => {
                let password = value
                    .as_str()
                    .ok_or("password must be a string")?
                    .to_string();
                if password.is_empty() {
                    return Err("Password cannot be empty".to_string());
                }
                self.password = password;
            }
            "display_backend" => {
                let backend = serde_json::from_value(value.clone())
                    .map_err(|_| "display_backend must be main or virtual")?;
                if self.private_session && backend != DisplayBackend::Virtual {
                    return Err(
                        "Disable guarded private sessions before selecting the main display".into(),
                    );
                }
                if self.reuse_virtual_display && backend != DisplayBackend::Virtual {
                    return Err(
                        "Disable virtual display reuse before selecting the main display".into(),
                    );
                }
                self.display_backend = backend;
            }
            "reuse_virtual_display" => {
                let enabled = value
                    .as_bool()
                    .ok_or("reuse_virtual_display must be a boolean")?;
                if enabled && self.display_backend != DisplayBackend::Virtual {
                    return Err(
                        "Select the virtual display before enabling virtual display reuse".into(),
                    );
                }
                self.reuse_virtual_display = enabled;
            }
            "private_session" => {
                let enabled = value.as_bool().ok_or("private_session must be a boolean")?;
                if enabled && self.display_backend != DisplayBackend::Virtual {
                    return Err(
                        "Select the virtual display before enabling guarded private sessions"
                            .into(),
                    );
                }
                self.private_session = enabled;
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
            "audio_enabled" | "clipboard_enabled" | "clipboard_file_transfer" => {
                let enabled = value
                    .as_bool()
                    .ok_or_else(|| format!("{key} must be a boolean"))?;
                match key {
                    "audio_enabled" => self.audio_enabled = enabled,
                    "clipboard_enabled" => self.clipboard_enabled = enabled,
                    _ => self.clipboard_file_transfer = enabled,
                }
            }
            "clipboard_max_file_size_mb" => {
                self.clipboard_max_file_size_mb = integer_in_range(
                    value,
                    key,
                    1,
                    macrdp_core::config::MAX_CLIPBOARD_FILE_SIZE_MB as u64,
                )? as u32;
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
            "tls_cert_path" | "tls_key_path" => {
                let path = value
                    .as_str()
                    .ok_or("Certificate paths must be strings")?
                    .trim();
                if path.contains('\0') {
                    return Err("Invalid certificate path".into());
                }
                if key == "tls_cert_path" {
                    self.tls_cert_path = path.to_string();
                } else {
                    self.tls_key_path = path.to_string();
                }
            }
            "tls_server_names" => {
                let names: Vec<String> = serde_json::from_value(value.clone())
                    .map_err(|_| "Server names must be a list of hostnames or IP addresses")?;
                macrdp_core::tls::validate_server_names(&names)
                    .map_err(|error| error.to_string())?;
                self.tls_server_names = names;
            }
            "autostart" => {
                self.autostart = value.as_bool().ok_or("autostart must be a boolean")?;
            }
            _ => return Err(format!("unknown config key: {key}")),
        }

        if crate::graphics_profiles::settings(self) != previous_graphics {
            self.graphics_profile = None;
        }
        Ok(self.requires_restart(key, None))
    }

    /// Listener and identity changes apply together at a serialized service restart.
    /// Virtual display ownership also cannot change in the middle of a session.
    pub fn requires_restart(&self, key: &str, running_backend: Option<DisplayBackend>) -> bool {
        matches!(
            key,
            "display_backend"
                | "reuse_virtual_display"
                | "private_session"
                | "keyboard_layout"
                | "keyboard_shortcuts"
                | "bind_address"
                | "port"
                | "idle_timeout_secs"
                | "tls_cert_path"
                | "tls_key_path"
                | "tls_server_names"
                | "audio_enabled"
                | "clipboard_enabled"
                | "clipboard_file_transfer"
                | "clipboard_max_file_size_mb"
        ) || (matches!(key, "resolution" | "hidpi_scale" | "port")
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
            reuse_virtual_display: self.reuse_virtual_display,
            private_session: self.private_session,
            keyboard_layout: self.keyboard_layout.clone(),
            keyboard_shortcuts: self.keyboard_shortcuts.clone(),
            width: 0,  // auto-detect
            height: 0, // auto-detect
            username: Some(self.username.clone()),
            password: if self.password.is_empty() {
                None
            } else {
                Some(self.password.clone())
            },
            cert_path: (!self.tls_cert_path.is_empty()).then(|| PathBuf::from(&self.tls_cert_path)),
            key_path: (!self.tls_key_path.is_empty()).then(|| PathBuf::from(&self.tls_key_path)),
            managed_tls_directory: Some(macrdp_core::tls::desktop_managed_tls_directory()),
            tls_server_names: self.tls_server_names.clone(),
            idle_timeout_secs: self.idle_timeout_secs,
            log_level: Some(self.log_level.clone()),
            quality: Some(self.quality.clone()),
            encoder: Some(self.encoder.clone()),
            chroma_mode: Some(self.chroma_mode.clone()),
            gfx_validation_mode: Some(self.gfx_validation_mode),
            resolution: Some(self.resolution.clone()),
            show_cursor: Some(self.show_cursor),
            bitrate_mbps: Some(self.bitrate_mbps),
            audio: macrdp_core::AudioConfig {
                enabled: self.audio_enabled,
                ..macrdp_core::AudioConfig::default()
            },
            clipboard: macrdp_core::ClipboardConfig {
                enabled: self.clipboard_enabled,
                file_transfer: self.clipboard_file_transfer,
                max_file_size_mb: self.clipboard_max_file_size_mb,
            },
        }
    }
}

#[cfg(test)]
mod keyboard_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retained_display_policy_migrates_and_persists_without_credentials() {
        let mut config: UiConfig = toml::from_str("username = 'fixture-user'").unwrap();
        assert!(!config.reuse_virtual_display);
        assert!(config
            .set_field("reuse_virtual_display", &json!(true))
            .is_err());
        config.password = "fixture-secret".into();
        config
            .set_field("display_backend", &json!("virtual"))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.toml");
        for enabled in [true, false] {
            assert!(config
                .set_field("reuse_virtual_display", &json!(enabled))
                .unwrap());
            for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
                assert!(config.requires_restart("reuse_virtual_display", Some(backend)));
            }
            config
                .save_with_store(&path, Some(&config.password), |_| Ok(()), || Ok(()))
                .unwrap();
            let content = std::fs::read_to_string(&path).unwrap();
            let saved: UiConfig = toml::from_str(&content).unwrap();
            assert_eq!(saved.reuse_virtual_display, enabled);
            assert_eq!(saved.to_server_config().reuse_virtual_display, enabled);
            assert!(!content.contains("fixture-secret"));
        }
    }

    #[test]
    fn retained_display_updates_reject_invalid_values_without_changing_policy() {
        let mut config = UiConfig::default();
        config
            .set_field("display_backend", &json!("virtual"))
            .unwrap();
        config
            .set_field("reuse_virtual_display", &json!(true))
            .unwrap();
        let original = serde_json::to_value(&config).unwrap();
        for value in [json!("false"), json!(0), json!(null)] {
            assert!(config.set_field("reuse_virtual_display", &value).is_err());
            assert_eq!(serde_json::to_value(&config).unwrap(), original);
        }
        assert!(config.set_field("display_backend", &json!("main")).is_err());
        assert_eq!(serde_json::to_value(&config).unwrap(), original);
        config
            .set_field("reuse_virtual_display", &json!(false))
            .unwrap();
        config.set_field("display_backend", &json!("main")).unwrap();
        // Reject inconsistent hand-edited policy before any credential write.
        config.reuse_virtual_display = true;
        config.password = "fixture-secret".into();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.toml");
        assert!(config
            .save_with_store(
                &path,
                Some(&config.password),
                |_| panic!("credential write"),
                || Ok(())
            )
            .is_err());
        assert!(!path.exists());
    }

    #[test]
    fn private_sessions_migrate_persist_and_require_a_restart_in_both_directions() {
        let mut config: UiConfig = toml::from_str("username = 'fixture-user'").unwrap();
        assert!(!config.private_session);
        config.password = "fixture-secret".into();
        config
            .set_field("display_backend", &json!("virtual"))
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.toml");
        for enabled in [true, false] {
            assert!(config
                .set_field("private_session", &json!(enabled))
                .unwrap());
            for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
                assert!(config.requires_restart("private_session", Some(backend)));
            }
            config
                .save_with_store(&path, Some(&config.password), |_| Ok(()), || Ok(()))
                .unwrap();
            let content = std::fs::read_to_string(&path).unwrap();
            let saved: UiConfig = toml::from_str(&content).unwrap();
            assert_eq!(saved.private_session, enabled);
            assert_eq!(saved.to_server_config().private_session, enabled);
            assert!(!content.contains("fixture-secret"));
        }
    }

    #[test]
    fn invalid_private_session_updates_do_not_weaken_saved_policy() {
        let mut config = UiConfig::default();
        assert!(config.set_field("private_session", &json!(true)).is_err());
        assert!(!config.private_session);
        config
            .set_field("display_backend", &json!("virtual"))
            .unwrap();
        config.set_field("private_session", &json!(true)).unwrap();
        let original = serde_json::to_value(&config).unwrap();
        for value in [json!("false"), json!(0), json!(null)] {
            assert!(config.set_field("private_session", &value).is_err());
            assert_eq!(serde_json::to_value(&config).unwrap(), original);
        }
        assert!(config.set_field("display_backend", &json!("main")).is_err());
        assert_eq!(serde_json::to_value(&config).unwrap(), original);
        config.set_field("private_session", &json!(false)).unwrap();
        config.set_field("display_backend", &json!("main")).unwrap();

        // A hand-edited preference file must not persist an unsupported pairing.
        config.private_session = true;
        config.password = "fixture-secret".into();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.toml");
        assert!(config
            .save_with_store(&path, Some(&config.password), |_| Ok(()), || Ok(()))
            .is_err());
        assert!(!path.exists());
    }

    #[test]
    fn quality_defaults_migrate_and_presets_persist_without_requiring_restart() {
        let legacy: UiConfig = toml::from_str("username = 'fixture-user'").unwrap();
        assert_eq!(legacy.quality, "balanced");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.toml");
        let mut config = legacy;
        config.password = "fixture-secret".into();
        for preset in ["low_latency", "balanced", "high_quality", "lossless"] {
            assert!(!config.set_field("quality", &json!(preset)).unwrap());
            for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
                assert!(!config.requires_restart("quality", Some(backend)));
            }
            config
                .save_with_store(&path, Some(&config.password), |_| Ok(()), || Ok(()))
                .unwrap();
            let saved: UiConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(saved.quality, preset);
            assert_eq!(saved.to_server_config().quality.as_deref(), Some(preset));
        }
        for value in [json!("unrecognized"), json!(true), json!(null)] {
            assert!(config.set_field("quality", &value).is_err());
            assert_eq!(config.quality, "lossless");
        }
    }

    #[test]
    fn bitmap_encoder_round_trips_and_graphics_updates_reject_unknown_values() {
        let mut config = UiConfig::default();
        for encoder in ["auto", "software", "hardware", "bitmap"] {
            assert!(!config.set_field("encoder", &json!(encoder)).unwrap());
            let encoded = toml::to_string(&config).unwrap();
            let restored: UiConfig = toml::from_str(&encoded).unwrap();
            assert_eq!(restored.encoder, encoder);
            assert_eq!(
                restored.to_server_config().encoder.as_deref(),
                Some(encoder)
            );
        }
        let original = serde_json::to_value(&config).unwrap();
        for (key, values) in [
            (
                "encoder",
                [json!("BITMAP"), json!("unknown"), json!(0), json!(null)],
            ),
            (
                "chroma_mode",
                [json!("AVC444"), json!("unknown"), json!(true), json!(null)],
            ),
        ] {
            for value in values {
                assert!(config.set_field(key, &value).is_err());
                assert_eq!(serde_json::to_value(&config).unwrap(), original);
            }
        }
        config.set_field("quality", &json!("lossless")).unwrap();
        assert!(!config.set_field("encoder", &json!("hardware")).unwrap());
        assert!(!config.set_field("encoder", &json!("bitmap")).unwrap());
        assert_eq!(config.encoder, "bitmap");
    }

    #[test]
    fn sharing_settings_migrate_persist_and_reach_server_startup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config-ui.toml");
        std::fs::write(&path, "username = 'fixture-user'\n").unwrap();
        let mut config = UiConfig::load_with_store(
            &path,
            || Ok(Some("fixture-secret".into())),
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert!(config.audio_enabled && config.clipboard_enabled && config.clipboard_file_transfer);
        assert_eq!(config.clipboard_max_file_size_mb, 100);
        for (key, value) in [
            ("audio_enabled", json!(false)),
            ("clipboard_enabled", json!(false)),
            ("clipboard_file_transfer", json!(false)),
            ("clipboard_max_file_size_mb", json!(256)),
        ] {
            assert!(config.set_field(key, &value).unwrap());
            for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
                assert!(config.requires_restart(key, Some(backend)));
            }
        }
        config
            .save_with_store(&path, Some(&config.password), |_| Ok(()), || Ok(()))
            .unwrap();
        let restored: UiConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let core = restored.to_server_config();
        assert!(!core.audio.enabled && !core.clipboard.enabled && !core.clipboard.file_transfer);
        assert_eq!(core.clipboard.max_file_size_mb, 256);
        assert_eq!((core.audio.sample_rate, core.audio.channels), (48000, 2));
        assert!(restored.password.is_empty());
    }

    #[test]
    fn invalid_sharing_settings_leave_preferences_unchanged() {
        let mut config = UiConfig::default();
        let original = serde_json::to_value(&config).unwrap();
        for key in [
            "audio_enabled",
            "clipboard_enabled",
            "clipboard_file_transfer",
        ] {
            for value in [json!(1), json!("false"), json!(null)] {
                assert!(config.set_field(key, &value).is_err());
                assert_eq!(serde_json::to_value(&config).unwrap(), original);
            }
        }
        for value in [
            json!(0),
            json!(257),
            json!(u64::MAX),
            json!(-1),
            json!(1.5),
            json!(true),
            json!("100"),
        ] {
            assert!(config
                .set_field("clipboard_max_file_size_mb", &value)
                .is_err());
            assert_eq!(serde_json::to_value(&config).unwrap(), original);
        }
    }

    #[test]
    fn tls_preferences_round_trip_without_persisting_the_password() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config-ui.toml");
        let mut config = UiConfig::default();
        config.password = "tls-settings-test-password".into();
        assert!(config
            .set_field("tls_cert_path", &json!(" /tmp/test certificate.pem "))
            .unwrap());
        assert!(config
            .set_field("tls_key_path", &json!(" /tmp/test key.pem "))
            .unwrap());
        assert!(config
            .set_field(
                "tls_server_names",
                &json!(["rdp.example.test", "192.0.2.7", "2001:db8::7"])
            )
            .unwrap());
        config
            .save_with_store(&path, Some(&config.password), |_| Ok(()), || Ok(()))
            .unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains(&config.password));
        let restored = UiConfig::load_with_store(
            &path,
            || Ok(Some(config.password.clone())),
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();
        let core = restored.to_server_config();
        assert_eq!(
            core.cert_path,
            Some(PathBuf::from("/tmp/test certificate.pem"))
        );
        assert_eq!(core.key_path, Some(PathBuf::from("/tmp/test key.pem")));
        assert_eq!(core.tls_server_names, config.tls_server_names);
        assert!(core.validate().is_ok());
        for key in ["tls_cert_path", "tls_key_path", "tls_server_names"] {
            for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
                assert!(restored.requires_restart(key, Some(backend)));
            }
        }
    }

    #[test]
    fn invalid_tls_updates_do_not_mutate_existing_identity_settings() {
        let mut config = UiConfig::default();
        config.tls_cert_path = "/tmp/existing.pem".into();
        config.tls_key_path = "/tmp/existing-key.pem".into();
        config.tls_server_names = vec!["rdp.example.test".into()];
        let original = serde_json::to_value(&config).unwrap();
        for (key, value) in [
            ("tls_cert_path", json!(true)),
            ("tls_key_path", json!("/tmp/key\0.pem")),
            ("tls_server_names", json!("rdp.example.test")),
            ("tls_server_names", json!([1])),
            ("tls_server_names", json!(["https://rdp.example.test"])),
            ("tls_server_names", json!(["*.example.test"])),
            ("tls_server_names", json!([""])),
            ("tls_server_names", json!(vec!["rdp.example.test"; 33])),
        ] {
            assert!(config.set_field(key, &value).is_err());
            assert_eq!(serde_json::to_value(&config).unwrap(), original);
        }
    }

    #[test]
    fn tls_pair_can_be_edited_individually_but_incomplete_identity_cannot_start() {
        let mut config: UiConfig = toml::from_str("username = 'test-user'").unwrap();
        config.password = "test-password".into();
        let defaults = config.to_server_config();
        assert!(defaults.cert_path.is_none() && defaults.key_path.is_none());
        assert!(defaults.tls_server_names.is_empty());
        config
            .set_field("tls_cert_path", &json!("/tmp/cert.pem"))
            .unwrap();
        assert!(config.to_server_config().validate().is_err());
        config
            .set_field("tls_key_path", &json!("/tmp/key.pem"))
            .unwrap();
        assert!(config.to_server_config().validate().is_ok());
        config.set_field("tls_cert_path", &json!(" ")).unwrap();
        assert!(config.to_server_config().validate().is_err());
        config.set_field("tls_key_path", &json!("")).unwrap();
        assert!(config.to_server_config().validate().is_ok());
    }

    #[test]
    fn invalid_numeric_updates_do_not_truncate_or_mutate_settings() {
        let mut config = UiConfig::default();
        let before = serde_json::to_value(&config).unwrap();
        for (key, invalid) in [
            ("port", json!(65536u64)),
            ("port", json!(0)),
            ("frame_rate", json!(241)),
            ("frame_rate", json!(0)),
            ("bitrate_mbps", json!(1001)),
            ("bitrate_mbps", json!(0)),
            ("max_connections", json!(u32::MAX as u64 + 1)),
        ] {
            assert!(
                config.set_field(key, &invalid).is_err(),
                "{key} accepted invalid value"
            );
            assert_eq!(serde_json::to_value(&config).unwrap(), before);
        }
        for key in ["port", "frame_rate", "bitrate_mbps", "max_connections"] {
            for invalid in [
                json!(u64::MAX),
                json!(-1),
                json!(1.5),
                json!(true),
                json!("60"),
            ] {
                assert!(config.set_field(key, &invalid).is_err());
                assert_eq!(serde_json::to_value(&config).unwrap(), before);
            }
        }
        config.set_field("port", &json!(65535)).unwrap();
        config.set_field("frame_rate", &json!(240)).unwrap();
        config.set_field("bitrate_mbps", &json!(1000)).unwrap();
        assert_eq!(
            (config.port, config.frame_rate, config.bitrate_mbps),
            (65535, 240, 1000)
        );
    }

    #[test]
    fn listener_and_idle_changes_require_restart_for_both_display_backends() {
        let mut config = UiConfig::default();
        assert!(config
            .set_field("bind_address", &json!("127.0.0.1"))
            .unwrap());
        assert!(config.set_field("idle_timeout_secs", &json!(120)).unwrap());
        for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
            assert!(config.requires_restart("bind_address", Some(backend)));
            assert!(config.requires_restart("idle_timeout_secs", Some(backend)));
        }
    }

    #[test]
    fn loading_preserves_existing_numeric_values_for_explicit_validation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config-ui.toml");
        std::fs::write(&path, "frame_rate = 0\nbitrate_mbps = 1001\nport = 65535\n").unwrap();
        let config = UiConfig::load_with_store(
            &path,
            || Ok(Some("test-secret".into())),
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            (config.frame_rate, config.bitrate_mbps, config.port),
            (0, 1001, 65535)
        );
        assert!(config.to_server_config().validate().is_err());
    }

    #[test]
    fn legacy_password_migrates_only_after_successful_secret_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config-ui.toml");
        let legacy = "username = 'test-user'\npassword = 'migration-test-secret'\n";
        std::fs::write(&path, legacy).unwrap();
        assert!(UiConfig::load_with_store(
            &path,
            || Ok(None),
            |_| Err("Keychain locked".to_string()),
            || Ok(())
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);
        let stored = std::cell::RefCell::new(String::new());
        let config = UiConfig::load_with_store(
            &path,
            || Ok(None),
            |password| {
                *stored.borrow_mut() = password.to_string();
                Ok(())
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(config.password, *stored.borrow());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("password"));
        let restored = UiConfig::load_with_store(
            &path,
            || Ok(Some(stored.borrow().clone())),
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(restored.password, config.password);
        assert!(!format!("{config:?}").contains(&config.password));
    }

    #[test]
    fn keychain_read_failure_does_not_generate_or_overwrite_a_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config-ui.toml");
        std::fs::write(&path, "username = 'test-user'\n").unwrap();
        assert!(UiConfig::load_with_store(
            &path,
            || Err("Keychain locked".to_string()),
            |_| panic!("Must not overwrite unavailable credential"),
            || Ok(())
        )
        .is_err());
    }

    #[test]
    fn preferences_write_failure_restores_previous_keychain_state() {
        let dir = tempfile::tempdir().unwrap();
        for previous in [Some("previous-test-secret".to_string()), None] {
            let stored = std::cell::RefCell::new(previous.clone());
            let mut config = UiConfig::default();
            config.password = "replacement-test-secret".to_string();
            // Persisting a regular file over a directory fails after secret storage.
            assert!(config
                .save_with_store(
                    dir.path(),
                    previous.as_deref(),
                    |password| {
                        *stored.borrow_mut() = Some(password.to_string());
                        Ok(())
                    },
                    || {
                        *stored.borrow_mut() = None;
                        Ok(())
                    }
                )
                .is_err());
            assert_eq!(*stored.borrow(), previous);
        }
    }

    #[test]
    fn empty_credential_updates_leave_existing_values_unchanged() {
        let mut config = UiConfig::default();
        config.password = "test-secret".to_string();
        assert!(config.set_field("username", &json!(" ")).is_err());
        assert!(config.set_field("password", &json!("")).is_err());
        assert_eq!(config.password, "test-secret");
        assert_eq!(config.username, "macrdp");
    }

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
        assert!(config.set_field("port", &json!(3389)).unwrap());
        assert!(config.requires_restart("port", Some(DisplayBackend::Virtual)));
        assert!(config.requires_restart("resolution", Some(DisplayBackend::Virtual)));
        assert!(config.requires_restart("port", Some(DisplayBackend::Main)));
        assert!(!config.requires_restart("frame_rate", Some(DisplayBackend::Virtual)));
    }
}
