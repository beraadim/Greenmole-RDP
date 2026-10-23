use clap::Parser;
use macrdp_core::{DisplayBackend, ServerConfig};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "Greenmole RDP", bin_name = "macrdp-server", about = "Greenmole RDP — macOS Remote Desktop Server", version)]
pub struct Cli {
    /// TCP port to listen on (defaults to the configuration file, or 3389).
    #[arg(short, long)]
    pub port: Option<u16>,
    /// Path to the shared server configuration.
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    /// Target frame rate.
    #[arg(long)]
    pub frame_rate: Option<u32>,
    /// Share the main display or an experimental virtual display.
    #[arg(long, value_parser = ["main", "virtual"])]
    pub display_backend: Option<String>,
    /// Experimentally keep the virtual display disabled between sessions. Requires virtual backend.
    #[arg(long, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    pub reuse_virtual_display: Option<bool>,
    /// Log level: trace, debug, info, warn, error.
    #[arg(long)]
    pub log_level: Option<String>,
    /// Collect bounded performance statistics and print a summary on exit.
    #[arg(long)]
    pub perf: bool,
    /// Record one metrics sample per second to a new private JSONL file (64 MiB limit).
    #[arg(long)]
    pub metrics_jsonl: Option<PathBuf>,
}

impl Cli {
    pub fn load_config(&self) -> anyhow::Result<ServerConfig> {
        let mut config = ServerConfig::load_from_file(self.config.as_deref())?;
        self.apply_overrides(&mut config);
        Ok(config)
    }

    fn apply_overrides(&self, config: &mut ServerConfig) {
        if let Some(port) = self.port {
            config.port = port;
        }
        if let Some(fps) = self.frame_rate {
            config.frame_rate = fps;
        }
        if let Some(backend) = &self.display_backend {
            config.display_backend = if backend == "virtual" {
                DisplayBackend::Virtual
            } else {
                DisplayBackend::Main
            };
        }
        if let Some(level) = &self.log_level {
            config.log_level = Some(level.clone());
        }
        if let Some(reuse) = self.reuse_virtual_display {
            config.reuse_virtual_display = reuse;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_can_explicitly_override_a_configured_port() {
        let mut config = ServerConfig {
            port: 13389,
            ..ServerConfig::default()
        };
        Cli::try_parse_from(["macrdp"])
            .unwrap()
            .apply_overrides(&mut config);
        assert_eq!(config.port, 13389);
        Cli::try_parse_from(["macrdp", "--port", "3389"])
            .unwrap()
            .apply_overrides(&mut config);
        assert_eq!(config.port, 3389);
    }

    #[test]
    fn display_backend_is_validated_and_applied() {
        assert!(Cli::try_parse_from(["macrdp", "--display-backend", "invalid"]).is_err());
        let mut config = ServerConfig::default();
        Cli::try_parse_from(["macrdp", "--display-backend", "virtual"])
            .unwrap()
            .apply_overrides(&mut config);
        assert_eq!(config.display_backend, DisplayBackend::Virtual);
    }

    #[test]
    fn retained_display_cli_can_enable_and_explicitly_disable_file_policy() {
        let mut config = ServerConfig::default();
        Cli::try_parse_from([
            "macrdp",
            "--display-backend",
            "virtual",
            "--reuse-virtual-display",
        ])
        .unwrap()
        .apply_overrides(&mut config);
        assert!(config.reuse_virtual_display);
        assert_eq!(config.display_backend, DisplayBackend::Virtual);
        Cli::try_parse_from(["macrdp"])
            .unwrap()
            .apply_overrides(&mut config);
        assert!(config.reuse_virtual_display);
        Cli::try_parse_from(["macrdp", "--reuse-virtual-display=false"])
            .unwrap()
            .apply_overrides(&mut config);
        assert!(!config.reuse_virtual_display);
        assert!(Cli::try_parse_from(["macrdp", "--reuse-virtual-display=invalid"]).is_err());
    }
}
