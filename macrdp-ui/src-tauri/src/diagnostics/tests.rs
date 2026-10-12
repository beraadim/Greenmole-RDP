use super::*;

#[test]
fn exports_selected_operational_fields_without_credentials_or_input_text() {
    let secret = "private-canary-value";
    let mut config = UiConfig::default();
    config.password = secret.into();
    config.username = secret.into();
    config.tls_key_path = secret.into();
    config.tls_cert_path = secret.into();
    config.bind_address = secret.into();
    config.tls_server_names = vec![secret.into()];
    config.encoder = secret.into();
    config.resolution = secret.into();
    let config_json = safe_configuration(&config).to_string();
    assert!(!config_json.contains(secret));
    assert!(!config_json.contains("password"));
    let event = json!({"level": "warn", "message": format!("Audio recovery counters {secret}"),
        "timestamp": secret, "target": secret,
        "fields": {"password": secret, "key": secret, "text": secret, "username": secret,
            "error": secret, "queue_age_ms": 250, "width": secret}});
    let log = format!("{event}\n");
    let summary = summarize_bytes(log.as_bytes(), false);
    assert!(!summary.to_string().contains(secret));
    assert_eq!(summary["events"][0]["category"], "audio_recovery");
    assert_eq!(summary["events"][0]["fields"]["queue_age_ms"], 250);
    let mut metrics = macrdp_core::Metrics::default();
    metrics.encoder_mode = secret.into();
    metrics.transport_mode = secret.into();
    metrics.bitrate_mode = secret.into();
    metrics.connection_outcome = Some(secret.into());
    assert!(!safe_metrics(&metrics).to_string().contains(secret));
}

#[test]
fn outcome_logs_export_only_known_reasons_and_typed_authentication_state() {
    let secret = "private-untrusted-error";
    let event = json!({"level": "info", "message": "Connection outcome",
        "fields": {"reason": secret, "authenticated": true, "peer": secret}});
    let value = summarize_bytes(format!("{event}\n").as_bytes(), false);
    assert!(!value.to_string().contains(secret));
    assert_eq!(value["events"][0]["fields"]["reason"], "unknown");
    assert_eq!(value["events"][0]["fields"]["authenticated"], true);
}

#[test]
fn log_summary_bounds_events_and_discards_partial_records() {
    let event = b"{\"level\":\"error\",\"message\":\"unknown user data\"}\n";
    let mut bytes = b"partial record\n".to_vec();
    for _ in 0..300 {
        bytes.extend_from_slice(event);
    }
    bytes.extend_from_slice(b"{\"level\":\"error\"}");
    let value = summarize_bytes(&bytes, true);
    assert_eq!(value["counts"]["error"], 300);
    assert_eq!(value["events"].as_array().unwrap().len(), MAX_EVENTS);
    assert!(!value.to_string().contains("unknown user data"));
}

#[test]
fn export_files_are_private_unique_and_size_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let first = write_report(directory.path(), b"first").unwrap();
    let second = write_report(directory.path(), b"second").unwrap();
    assert_ne!(first, second);
    assert_eq!(std::fs::read(&first).unwrap(), b"first");
    assert!(write_report(directory.path(), &vec![0; EXPORT_BYTES + 1]).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(first).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
