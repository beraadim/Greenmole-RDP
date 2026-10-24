//! Shared keyboard preference validation for CLI files and desktop settings.
//!
//! Layout and shortcut policy are independent: changing the meaning of Control
//! must never implicitly change the alphabet or punctuation sent by the client.

pub const LAYOUTS: &[&str] = &["auto", "us", "uk", "no", "se", "dk", "de", "fr", "physical"];
pub const SHORTCUT_POLICIES: &[&str] = &["auto", "windows", "mac", "physical"];

pub fn validate_layout(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        LAYOUTS.contains(&value),
        "keyboard_layout must be auto, us, uk, no, se, dk, de, fr or physical"
    );
    Ok(())
}

pub fn validate_shortcuts(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        SHORTCUT_POLICIES.contains(&value),
        "keyboard_shortcuts must be auto, windows, mac or physical"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerConfig;

    #[test]
    fn legacy_configuration_retains_automatic_layout_and_shortcuts() {
        let config: ServerConfig = toml::from_str("port = 3389").unwrap();
        assert_eq!(config.keyboard_layout, "auto");
        assert_eq!(config.keyboard_shortcuts, "auto");
    }

    #[test]
    fn every_layout_and_shortcut_policy_survives_a_config_round_trip() {
        for layout in LAYOUTS {
            for shortcuts in SHORTCUT_POLICIES {
                let config = ServerConfig {
                    username: Some("fixture-user".into()),
                    password: Some("fixture-password".into()),
                    keyboard_layout: (*layout).into(),
                    keyboard_shortcuts: (*shortcuts).into(),
                    ..ServerConfig::default()
                };
                config.validate().unwrap();
                let restored: ServerConfig =
                    toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
                assert_eq!(restored.keyboard_layout, *layout);
                assert_eq!(restored.keyboard_shortcuts, *shortcuts);
            }
        }
    }

    #[test]
    fn unsupported_or_malformed_keyboard_preferences_are_rejected() {
        for value in ["", "AUTO", "no-no-qwerty", "00000414", "macos", "auto "] {
            assert!(validate_layout(value).is_err());
            assert!(validate_shortcuts(value).is_err());
        }
        for field in ["keyboard_layout", "keyboard_shortcuts"] {
            for value in ["1", "true", "[]"] {
                assert!(toml::from_str::<ServerConfig>(&format!("{field} = {value}")).is_err());
            }
        }
        let mut config = ServerConfig::default();
        config.keyboard_layout = "unsupported".into();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("keyboard_layout"));
        config.keyboard_layout = "auto".into();
        config.keyboard_shortcuts = "unsupported".into();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("keyboard_shortcuts"));
    }
}
