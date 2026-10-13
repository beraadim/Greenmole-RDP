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
fn transport_and_quarantined_clipboard_events_keep_static_reasons_and_numeric_evidence() {
    let transport = json!({"level": "info", "message": "Connection transport ended",
        "fields": {"reason": "peer_reset", "socket_error_code": 54, "delivered_updates": 123,
            "last_update_bytes": 456, "last_write_ms": 1.2, "error": "private-client-data"}});
    let clipboard = json!({"level": "warn", "message": "Clipboard disabled for this connection; reconnect to restore clipboard",
        "fields": {"reason": "clipboard protocol input failed", "path": "private-file"}});
    let summary = summarize_bytes(format!("{transport}\n{clipboard}\n").as_bytes(), false);
    assert_eq!(summary["events"][0]["fields"]["reason"], "peer_reset");
    assert_eq!(summary["events"][0]["fields"]["socket_error_code"], 54);
    assert_eq!(
        summary["events"][1]["fields"]["reason"],
        "clipboard protocol input failed"
    );
    assert!(!summary.to_string().contains("private"));
}

#[test]
fn clipboard_and_capture_failures_keep_only_selected_diagnostic_fields() {
    let clipboard = json!({"level": "warn", "message": "Clipboard connection failed; reconnect to reset clipboard state",
        "fields": {"reason": "clipboard request or storage operation timed out", "path": "private-file"}});
    let capture = json!({"level": "warn", "message": "Native capture frame rate update failed",
        "fields": {"fps": 30, "attempt": 4, "retrying": false, "error": "private-error"}});
    let summary = summarize_bytes(format!("{clipboard}\n{capture}\n").as_bytes(), false);
    assert_eq!(summary["events"][0]["category"], "clipboard_failed");
    assert_eq!(
        summary["events"][0]["fields"]["reason"],
        "clipboard request or storage operation timed out"
    );
    assert_eq!(summary["events"][1]["fields"]["retrying"], false);
    assert_eq!(summary["events"][1]["fields"]["attempt"], 4);
    assert!(!summary.to_string().contains("private"));
    let mut metrics = macrdp_core::Metrics::default();
    metrics.requested_capture_frame_rate = Some(60);
    metrics.applied_frame_rate = Some(30);
    metrics.capture_frame_rate_retry_exhausted = true;
    let exported = safe_metrics(&metrics);
    assert_eq!(exported["requested_capture_frame_rate"], 60);
    assert_eq!(exported["applied_frame_rate"], 30);
    assert_eq!(exported["capture_frame_rate_retry_exhausted"], true);
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
fn selected_export_is_atomic_private_and_does_not_follow_an_existing_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("selected.json");
    std::fs::write(&target, b"old report").unwrap();
    write_selected_report(&target, b"new report").unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new report");
    assert!(write_selected_report(&target, &vec![0; EXPORT_BYTES + 1]).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"new report");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let outside = directory.path().join("other.json");
        std::fs::write(&outside, b"unrelated file").unwrap();
        std::fs::remove_file(&target).unwrap();
        symlink(&outside, &target).unwrap();
        write_selected_report(&target, b"chosen report").unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"unrelated file");
        assert_eq!(std::fs::read(&target).unwrap(), b"chosen report");
    }
}
