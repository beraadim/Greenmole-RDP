use super::*;

#[test]
fn old_preferences_have_no_implicit_profile_and_builtins_are_graphics_only() {
    let mut config: UiConfig = toml::from_str("port=3395\nprivate_session=false").unwrap();
    assert!(config.graphics_profiles.is_empty());
    assert!(snapshot(&config).active_profile.is_none());
    let original = config.to_server_config();
    let candidate = selected_candidate(&config, Some("builtin:mobile")).unwrap();
    assert_eq!(candidate.frame_rate, 30);
    assert_eq!(candidate.bitrate_mbps, 8);
    assert_eq!(candidate.chroma_mode, "avc420");
    assert_eq!(candidate.port, config.port);
    assert_eq!(candidate.display_backend, config.display_backend);
    assert_eq!(candidate.private_session, config.private_session);
    assert_eq!(candidate.resolution, config.resolution);
    assert_eq!(candidate.username, config.username);
    assert_eq!(candidate.password, config.password);
    assert_eq!(original.port, candidate.to_server_config().port);
    config = candidate;
    assert_eq!(
        snapshot(&config).active_profile.as_deref(),
        Some("builtin:mobile")
    );
    config
        .set_field("frame_rate", &serde_json::json!(45))
        .unwrap();
    assert!(snapshot(&config).active_profile.is_none());
    assert_eq!(config.bitrate_mbps, 8);
}

#[test]
fn custom_profiles_preserve_manual_chroma_and_delete_without_changing_live_settings() {
    let mut config = UiConfig::default();
    config.chroma_mode = "avc444".into();
    config.frame_rate = 24;
    config = save_current(&config, "  My laptop  ").unwrap();
    assert_eq!(config.graphics_profiles[0].name, "My laptop");
    let serialized = toml::to_string(&config).unwrap();
    let decoded: UiConfig = toml::from_str(&serialized).unwrap();
    validate_saved(&decoded.graphics_profiles).unwrap();
    config
        .set_field("frame_rate", &serde_json::json!(60))
        .unwrap();
    config = selected_candidate(&config, Some("saved:My laptop")).unwrap();
    assert_eq!(config.frame_rate, 24);
    assert_eq!(config.chroma_mode, "avc444");
    let target = settings(&config);
    config = remove_saved(&config, "saved:My laptop").unwrap();
    assert_eq!(settings(&config), target);
    assert!(config.graphics_profile.is_none());
    assert!(remove_saved(&config, "builtin:mobile").is_err());
}

#[test]
fn profile_names_and_storage_are_bounded_and_overwrites_are_explicit() {
    let config = UiConfig::default();
    for invalid in [
        "",
        "  ",
        "Mobile",
        "desktop",
        "LIMITED BANDWIDTH",
        "x\ny",
        "x\0y",
    ] {
        assert!(save_current(&config, invalid).is_err());
    }
    assert!(save_current(&config, &"a".repeat(49)).is_err());
    let mut config = config;
    for i in 0..MAX_PROFILES {
        config = save_current(&config, &format!("Profile {i}")).unwrap();
    }
    assert!(save_current(&config, "Another").is_err());
    config.frame_rate = 72;
    config = save_current(&config, "profile 0").unwrap();
    assert_eq!(config.graphics_profiles.len(), MAX_PROFILES);
    assert_eq!(config.graphics_profiles[0].settings.frame_rate, 72);
    let mut duplicate = config.graphics_profiles.clone();
    duplicate[1].name = duplicate[0].name.to_uppercase();
    assert!(validate_saved(&duplicate).is_err());
    let mut invalid = config.graphics_profiles.clone();
    invalid[0].settings.frame_rate = 0;
    assert!(validate_saved(&invalid).is_err());
}

#[test]
fn failed_persistence_cannot_partially_replace_a_graphics_target() {
    let mut config = UiConfig::default();
    let old = settings(&config);
    let candidate = selected_candidate(&config, Some("builtin:mobile")).unwrap();
    let result = persist_candidate(&mut config, candidate, |_| Err("test write failed".into()));
    assert!(result.is_err());
    assert_eq!(settings(&config), old);
    assert!(config.graphics_profile.is_none());
    let candidate = selected_candidate(&config, Some("builtin:mobile")).unwrap();
    let mut calls = 0;
    persist_candidate(&mut config, candidate, |c| {
        calls += 1;
        assert_eq!(c.frame_rate, 30);
        assert_eq!(c.bitrate_mbps, 8);
        Ok(())
    })
    .unwrap();
    assert_eq!(calls, 1);
}

#[test]
fn timeout_keeps_all_durable_targets_pending_and_retry_uses_current_overrides() {
    let mut running = UiConfig::default();
    let old = settings(&running);
    let mut saved = selected_candidate(&running, Some("builtin:mobile")).unwrap();
    let mut pending = BTreeSet::new();
    record_confirmation(Some(&mut running), &mut pending, &saved, false);
    assert_eq!(settings(&running), old);
    assert!(pending.contains("graphics_profile"));
    let status = configuration_status(&saved, Some(&running), &pending);
    assert_eq!(status.pending_live, ["graphics_profile"]);
    assert!(!status.restart_required);
    saved
        .set_field("frame_rate", &serde_json::json!(45))
        .unwrap();
    let retry = selected_candidate(&saved, None).unwrap();
    assert_eq!(retry.frame_rate, 45);
    assert_eq!(retry.bitrate_mbps, 8);
    assert!(retry.graphics_profile.is_none());
    pending.insert("quality");
    pending.insert("unrelated");
    record_confirmation(Some(&mut running), &mut pending, &retry, true);
    assert_eq!(settings(&running), settings(&retry));
    assert!(!pending.contains("graphics_profile"));
    assert!(!pending.contains("quality"));
    assert!(pending.contains("unrelated"));
    assert!(configuration_status(&saved, Some(&running), &pending)
        .pending_live
        .is_empty());
}

#[test]
fn reconnect_feedback_covers_chroma_and_returning_from_terminal_browser_image_path() {
    let mut previous = UiConfig::default();
    let next = previous.clone();
    assert!(!needs_reconnect(&previous, &next));
    previous.chroma_mode = "avc444".into();
    assert!(needs_reconnect(&previous, &next));
    previous.chroma_mode = "avc420".into();
    previous.encoder = "bitmap".into();
    assert!(needs_reconnect(&previous, &next));
    previous.encoder = "auto".into();
    previous.quality = "lossless".into();
    assert!(needs_reconnect(&previous, &next));
    assert!(!needs_reconnect(&previous, &previous));
}

#[test]
fn profile_confirmation_cannot_acknowledge_pending_display_or_privacy_changes() {
    let mut running = UiConfig::default();
    let mut saved = running.clone();
    saved.display_backend = macrdp_core::DisplayBackend::Virtual;
    saved.private_session = true;
    let candidate = selected_candidate(&saved, Some("builtin:mobile")).unwrap();
    let mut pending = BTreeSet::new();
    record_confirmation(Some(&mut running), &mut pending, &candidate, true);
    assert_eq!(running.display_backend, macrdp_core::DisplayBackend::Main);
    assert!(!running.private_session);
    let status = configuration_status(&candidate, Some(&running), &pending);
    assert_eq!(
        status.pending_restart,
        ["display_backend", "private_session"]
    );
    assert!(status.pending_live.is_empty());
}
