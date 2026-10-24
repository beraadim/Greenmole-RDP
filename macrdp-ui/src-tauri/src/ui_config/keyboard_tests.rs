use super::*;
use macrdp_core::config::keyboard::{LAYOUTS, SHORTCUT_POLICIES};
use serde_json::json;

#[test]
fn keyboard_preferences_migrate_persist_and_reach_server_configuration() {
    let legacy: UiConfig = toml::from_str("port = 3389").unwrap();
    assert_eq!(legacy.keyboard_layout, "auto");
    assert_eq!(legacy.keyboard_shortcuts, "auto");
    for (key, values) in [
        ("keyboard_layout", LAYOUTS),
        ("keyboard_shortcuts", SHORTCUT_POLICIES),
    ] {
        for value in values {
            let mut config = legacy.clone();
            assert!(config.set_field(key, &json!(value)).unwrap());
            for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
                assert!(config.requires_restart(key, Some(backend)));
            }
            let restored: UiConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
            let server = restored.to_server_config();
            assert_eq!(server.keyboard_layout, config.keyboard_layout);
            assert_eq!(server.keyboard_shortcuts, config.keyboard_shortcuts);
        }
    }
}

#[test]
fn invalid_keyboard_edits_leave_both_preferences_unchanged() {
    let mut config = UiConfig::default();
    config.set_field("keyboard_layout", &json!("no")).unwrap();
    config
        .set_field("keyboard_shortcuts", &json!("windows"))
        .unwrap();
    for key in ["keyboard_layout", "keyboard_shortcuts"] {
        for invalid in [
            json!(null),
            json!(true),
            json!(1),
            json!([]),
            json!("unknown"),
            json!("auto "),
        ] {
            assert!(config.set_field(key, &invalid).is_err());
            assert_eq!(config.keyboard_layout, "no");
            assert_eq!(config.keyboard_shortcuts, "windows");
        }
    }
}

#[test]
fn invalid_saved_keyboard_preferences_never_touch_credentials_or_preferences() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    for field in ["keyboard_layout", "keyboard_shortcuts"] {
        let original = format!("{field} = 'unsupported'\n");
        std::fs::write(&path, &original).unwrap();
        let result = UiConfig::load_with_store(
            &path,
            || panic!("invalid preferences must not access Keychain"),
            |_| panic!("invalid preferences must not save a credential"),
            || panic!("invalid preferences must not delete a credential"),
        );
        assert!(result.unwrap_err().contains(field));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}
