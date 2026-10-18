use super::*;

#[test]
fn supervised_recovery_exports_stage_age_and_result_without_free_form_text() {
    let mut supervision = macrdp_core::supervised::SupervisionDiagnostics {
        enabled: true,
        phase: "recovering",
        worker_pid: Some(42),
        native_operation: Some("set_frame_rate".into()),
        native_operation_elapsed_ms: 31_000,
        restart_count: 1,
        recovery_result: "restored",
    };
    let safe = safe_supervision(&supervision);
    assert_eq!(safe["native_operation"], "set_frame_rate");
    assert_eq!(safe["native_operation_elapsed_ms"], 31_000);
    assert_eq!(safe["recovery_result"], "restored");
    supervision.native_operation = Some("private-user-data".into());
    supervision.phase = "private-user-data";
    supervision.recovery_result = "private-user-data";
    assert!(!safe_supervision(&supervision)
        .to_string()
        .contains("private-user-data"));
    let event = json!({"level":"warn", "message":"Native capture recovery is still pending",
        "fields":{"operation":"set_frame_rate", "operation_elapsed_ms":30000,
            "recovery_elapsed_ms":3000, "error":"private-user-data"}});
    let safe = summarize_bytes(format!("{event}\n").as_bytes(), false);
    assert_eq!(safe["events"][0]["fields"]["operation"], "set_frame_rate");
    assert!(!safe.to_string().contains("private-user-data"));
}

#[test]
fn native_timeouts_and_keepalive_policy_preserve_only_operational_evidence() {
    let transport = json!({"level": "warn", "message": "Connection transport ended",
        "fields": {"reason": "transport_timeout", "socket_error_code": 60,
            "error": "private-route-data"}});
    let policy = json!({"level": "info", "message": "TCP keepalive configured",
        "fields": {"keepalive_idle_seconds": 60, "keepalive_interval_seconds": 15,
            "keepalive_retries": 4, "peer": "private-peer"}});
    let summary = summarize_bytes(format!("{transport}\n{policy}\n").as_bytes(), false);
    assert_eq!(
        summary["events"][0]["fields"]["reason"],
        "transport_timeout"
    );
    assert_eq!(summary["events"][1]["category"], "transport_policy");
    assert_eq!(summary["events"][1]["fields"]["keepalive_retries"], 4);
    assert!(!summary.to_string().contains("private"));
    let mut metrics = macrdp_core::Metrics::default();
    metrics.session_active = true;
    metrics.bitmap_update_budget_bytes = Some(32_768);
    metrics.connection_outcome = Some("transport_timeout".into());
    let exported = safe_metrics(&metrics);
    assert_eq!(exported["session_active"], true);
    assert_eq!(exported["bitmap_update_budget_bytes"], 32_768);
    assert_eq!(exported["connection_outcome"], "transport_timeout");
}

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
