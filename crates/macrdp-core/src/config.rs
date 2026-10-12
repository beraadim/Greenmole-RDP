//! Server configuration

use macrdp_display::DisplayBackend;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Audio forwarding configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub enabled: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_rate: 48000,
            channels: 2,
        }
    }
}

/// Clipboard synchronization configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    pub enabled: bool,
    pub file_transfer: bool,
    pub max_file_size_mb: u32,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            file_transfer: true,
            max_file_size_mb: 100,
        }
    }
}

/// Server configuration (loaded from TOML, shared between CLI and UI)
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub port: u16,
    /// Address to bind the RDP listener to (default: 0.0.0.0)
    pub bind_address: Option<String>,
    pub frame_rate: u32,
    /// Display to share: the main display or an experimental virtual display.
    pub display_backend: DisplayBackend,
    /// Resolution width (0 = auto-detect from display)
    pub width: u32,
    /// Resolution height (0 = auto-detect from display)
    pub height: u32,
    pub username: Option<String>,
    pub password: Option<String>,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
    /// Additional DNS names or IP addresses for the managed TLS certificate.
    /// Localhost and the system hostname are always included. Requires restart.
    pub tls_server_names: Vec<String>,
    pub idle_timeout_secs: u64,
    /// Log level: trace, debug, info, warn, error
    pub log_level: Option<String>,
    /// Video quality: low_latency, balanced, high_quality (default: high_quality)
    pub quality: Option<String>,
    /// H.264 encoder: software, hardware, auto (default: software)
    pub encoder: Option<String>,
    /// Chroma subsampling mode: "avc420" or "avc444" (default: "avc420")
    pub chroma_mode: Option<String>,
    /// Force negotiated GFX sessions to stay on uncompressed updates only.
    /// Intended for Guacamole compatibility validation; default is false.
    pub gfx_validation_mode: Option<bool>,
    /// Resolution: "auto", or "WxH" like "3840x2160" (default: "auto")
    #[serde(alias = "hidpi_scale")]
    pub resolution: Option<String>,
    /// Show cursor in capture (default: true)
    pub show_cursor: Option<bool>,
    /// Target bitrate in Mbps (default: auto-calculated)
    pub bitrate_mbps: Option<u32>,
    /// Audio forwarding configuration
    #[serde(default)]
    pub audio: AudioConfig,
    /// Clipboard synchronization configuration
    #[serde(default)]
    pub clipboard: ClipboardConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 3389,
            bind_address: None,
            frame_rate: 60,
            display_backend: DisplayBackend::Main,
            width: 0,
            height: 0,
            username: None,
            password: None,
            cert_path: None,
            key_path: None,
            tls_server_names: Vec::new(),
            idle_timeout_secs: 1800,
            log_level: None,
            quality: None,
            encoder: None,
            chroma_mode: None,
            gfx_validation_mode: None,
            resolution: None,
            show_cursor: None,
            bitrate_mbps: None,
            audio: AudioConfig::default(),
            clipboard: ClipboardConfig::default(),
        }
    }
}

impl ServerConfig {
    /// Reject unsafe/inconsistent settings before initializing the desktop or listener.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.port > 0, "port must be between 1 and 65535");
        anyhow::ensure!(
            self.cert_path.is_some() == self.key_path.is_some(),
            "cert_path and key_path must both be supplied for a custom TLS identity"
        );
        crate::tls::validate_server_names(&self.tls_server_names)?;
        anyhow::ensure!(
            self.username
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            "A nonempty RDP username is required"
        );
        anyhow::ensure!(
            self.password
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
            "A nonempty RDP password is required"
        );
        anyhow::ensure!(
            (1..=240).contains(&self.frame_rate),
            "frame_rate must be between 1 and 240"
        );
        anyhow::ensure!(
            self.bitrate_mbps
                .map_or(true, |value| value > 0 && value <= 1000),
            "bitrate_mbps must be between 1 and 1000"
        );
        anyhow::ensure!(
            matches!(self.audio.channels, 1 | 2),
            "audio channels must be 1 or 2"
        );
        anyhow::ensure!(
            (8000..=192000).contains(&self.audio.sample_rate),
            "Invalid audio sample rate"
        );
        Ok(())
    }

    /// Load config from a TOML file path, or use defaults if None.
    pub fn load_from_file(path: Option<&std::path::Path>) -> anyhow::Result<Self> {
        if let Some(path) = path {
            crate::credentials::protect_config_file(path)?;
            let content = std::fs::read_to_string(path)?;
            toml::from_str(&content)
                .map_err(|_| anyhow::anyhow!("Invalid server configuration file"))
        } else {
            let default_path = config_dir().join("config.toml");
            if default_path.exists() {
                crate::credentials::protect_config_file(&default_path)?;
                let content = std::fs::read_to_string(&default_path)?;
                toml::from_str(&content)
                    .map_err(|_| anyhow::anyhow!("Invalid server configuration file"))
            } else {
                Ok(ServerConfig::default())
            }
        }
    }
}

/// Returns the macrdp config directory.
/// Search order for existing `config.toml`: ./ -> ~/.config/macrdp -> native config dir.
/// Fallback when no config exists: native config dir -> ~/.config/macrdp -> ./.
pub fn config_dir() -> PathBuf {
    let candidates = config_dir_candidates();
    for dir in &candidates {
        if dir.join("config.toml").exists() {
            return dir.clone();
        }
    }

    if let Some(native) = dirs::config_dir() {
        return native.join("macrdp");
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".config").join("macrdp");
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn config_dir_candidates() -> Vec<PathBuf> {
    let mut dirs_list = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        dirs_list.push(cwd);
    } else {
        dirs_list.push(PathBuf::from("."));
    }
    if let Some(home) = dirs::home_dir() {
        dirs_list.push(home.join(".config").join("macrdp"));
    }
    if let Some(native) = dirs::config_dir() {
        dirs_list.push(native.join("macrdp"));
    }
    dirs_list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.port, 3389);
        assert_eq!(config.bind_address, None);
        assert_eq!(config.frame_rate, 60);
        assert_eq!(config.display_backend, DisplayBackend::Main);
        assert!(config.username.is_none());
    }

    #[test]
    fn authentication_and_encoder_limits_are_checked_before_startup() {
        let mut config = ServerConfig::default();
        assert!(config.validate().is_err());
        config.username = Some("macrdp".into());
        config.password = Some("fixture-password".into());
        assert!(config.validate().is_ok());
        config.port = 0;
        assert!(config.validate().is_err());
        config.port = 3389;
        config.frame_rate = 0;
        assert!(config.validate().is_err());
        config.frame_rate = 60;
        config.bitrate_mbps = Some(u32::MAX);
        assert!(config.validate().is_err());
        config.bitrate_mbps = Some(50);
        config.password = Some(String::new());
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_parse_toml_config() {
        let toml_str = r#"
            port = 13389
            bind_address = "127.0.0.1"
            frame_rate = 120
            username = "admin"
            password = "secret"
        "#;
        let config: ServerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 13389);
        assert_eq!(config.bind_address.as_deref(), Some("127.0.0.1"));
        assert_eq!(config.frame_rate, 120);
        assert_eq!(config.username.as_deref(), Some("admin"));
    }

    #[test]
    fn virtual_display_settings_round_trip_and_legacy_configs_keep_main_display() {
        let legacy: ServerConfig = toml::from_str("port = 13389").unwrap();
        assert_eq!(legacy.display_backend, DisplayBackend::Main);

        let virtual_config: ServerConfig = toml::from_str("display_backend = 'virtual'").unwrap();
        let saved = toml::to_string(&virtual_config).unwrap();
        let restored: ServerConfig = toml::from_str(&saved).unwrap();
        assert_eq!(restored.display_backend, DisplayBackend::Virtual);
        assert!(toml::from_str::<ServerConfig>("display_backend = 'unknown'").is_err());
    }
}
