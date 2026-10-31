use super::*;

#[test]
fn startup_events_preserve_typed_evidence_without_capture_or_cache_payloads() {
    let secret = "private-startup-payload-canary";
    let cases = [
        (
            "Capture startup pixel health",
            "capture_health",
            json!({
                "phase":"baseline", "state":"observed", "display_id":31,
                "source_format":"bgra", "width":1920, "height":1200,
                "sample_available":true, "sampled_pixels":256, "near_black_pixels":10,
                "near_black_luma_max":4, "luma_min":0.0, "luma_max":250.0, "luma_mean":80.5,
                "pixels":secret, "image_hash":secret,
            }),
        ),
        (
            "GFX surface setup prepared",
            "graphics_surface",
            json!({
                "phase":"created", "state":"prepared", "surface_id":0,
                "surface_generation":1, "width":1920, "height":1200,
                "frame_width":1920, "frame_height":1200, "avc_frame":true,
                "keyframe":true, "deleted_previous_surface":false, "payload":secret,
            }),
        ),
        (
            "First AVC frame prepared",
            "graphics_startup",
            json!({
                "phase":"first_avc", "state":"prepared", "surface_generation":1,
                "graphics_frame_id":1, "bitmap_dispatches":1, "main_bytes":70000,
                "keyframe":true, "main_has_sps":true, "main_has_pps":true,
                "main_has_idr":true, "aux_has_idr":false, "nal_data":secret,
            }),
        ),
        (
            "Persistent graphics cache offer declined with an empty import reply",
            "graphics_cache",
            json!({
                "phase":"offer", "state":"declined", "offer_count":20,
                "reply_count":0, "cache_keys":secret, "entry_metadata":secret,
            }),
        ),
    ];
    let mut records = String::new();
    for (message, _, fields) in &cases {
        records.push_str(&format!(
            "{}\n",
            json!({"level":"info", "message":message, "fields":fields})
        ));
    }
    let summary = summarize_bytes(records.as_bytes(), false);
    let events = summary["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        cases.len(),
        "startup INFO must survive normal export filtering"
    );
    assert!(!summary.to_string().contains(secret));
    for (event, (_, category, fields)) in events.iter().zip(&cases) {
        assert_eq!(event["category"], *category);
        for (key, value) in fields.as_object().unwrap() {
            if value != secret {
                assert_eq!(&event["fields"][key], value, "{category}:{key}");
            }
        }
    }
}

#[test]
fn startup_export_rejects_free_form_values_and_distinguishes_missing_pixel_samples() {
    let fields = json!({"phase":"baseline", "state":"observed", "source_format":"nv12_unsampled",
        "sample_available":false, "sampled_pixels":0, "luma_min":-1, "luma_max":-1,
        "luma_mean":-1, "width":"private-canary", "display_id":-1,
        "near_black_pixels":0.5, "worker_pid":[], "main_has_sps":"private-canary"});
    let safe = startup::safe_fields("capture_health", &fields).unwrap();
    assert_eq!(safe["sample_available"], false);
    assert_eq!(safe["luma_mean"], -1.0);
    assert!(!safe.contains_key("width"));
    assert!(!safe.contains_key("display_id"));
    assert!(!safe.contains_key("near_black_pixels"));
    assert!(!serde_json::to_string(&safe)
        .unwrap()
        .contains("private-canary"));
    let safe = startup::safe_fields(
        "graphics_startup",
        &json!({
            "phase":"private-canary", "state":"private-canary", "main_has_sps":"private-canary",
            "bitmap_dispatches":-1, "main_bytes":{}, "payload":"private-canary",
        }),
    )
    .unwrap();
    assert_eq!(
        safe,
        json!({"phase":"unknown", "state":"unknown"})
            .as_object()
            .unwrap()
            .clone()
    );
}

#[test]
fn input_timing_export_selects_only_numeric_aggregate_fields() {
    let secret = "private-input-payload-canary";
    let value = json!({
        "received_events": 12, "untimed_events": 1, "dispatched_events": 11,
        "completed_dispatches": 10, "dispatch_in_progress": true,
        "current_dispatch_age_ms": 70.0,
        "post_attempts": 10, "post_returns": 9, "post_errors": 1,
        "native_callbacks": 11, "blocked_callbacks": 1,
        "unavailable_callbacks": 2, "injection_failures": 3,
        "post_samples_dropped": 4,
        "last_receive_age_ms": 1.0, "last_dispatch_age_ms": null,
        "last_post_age_ms": 2.5,
        "dispatch_wait": {"samples": 11, "p50_ms": 0.1, "p95_ms": 5.0,
            "p99_ms": 12.0, "max_ms": 12.0, "raw_event": secret},
        "handler": {"samples": 11, "p50_ms": null, "p95_ms": null,
            "p99_ms": null, "max_ms": 25.0},
        "native_post": {"samples": 9, "p50_ms": 0.1, "p95_ms": 0.2,
            "p99_ms": 0.3, "max_ms": 0.3},
        "read_to_post": {"samples": 9, "p50_ms": 1.0, "p95_ms": 10.0,
            "p99_ms": 40.0, "max_ms": 40.0},
        "keys": secret, "text": secret, "coordinates": secret,
        "client_name": secret, "future_input_field": {"value": secret},
    });
    let exported = safe_input_timing(&value);
    assert!(!exported.to_string().contains(secret));
    assert_eq!(exported.as_object().unwrap().len(), 21);
    assert_eq!(exported["received_events"], 12);
    assert_eq!(exported["post_returns"], 9);
    assert_eq!(exported["completed_dispatches"], 10);
    assert_eq!(exported["dispatch_in_progress"], true);
    assert_eq!(exported["current_dispatch_age_ms"], 70.0);
    assert_eq!(exported["dispatch_wait"]["max_ms"], 12.0);
    assert_eq!(exported["read_to_post"]["max_ms"], 40.0);
    assert!(exported["last_dispatch_age_ms"].is_null());
    assert_eq!(exported["dispatch_wait"].as_object().unwrap().len(), 5);

    // Reject malformed values even under known names, rather than trusting
    // future serializer additions or a free-form object at the nesting boundary.
    let malformed = json!({"received_events": secret, "post_returns": -1,
        "dispatch_in_progress": secret, "completed_dispatches": secret,
        "post_attempts": 1.5, "last_receive_age_ms": -0.1,
        "last_dispatch_age_ms": secret, "handler": secret,
        "native_post": {"samples": secret, "p50_ms": -1, "max_ms": secret}});
    let exported = safe_input_timing(&malformed);
    assert!(!exported.to_string().contains(secret));
    assert_eq!(exported, json!({"native_post": {}}));
}

#[test]
fn metrics_export_keeps_input_and_graphics_progress_counters() {
    let mut metrics = macrdp_core::Metrics::default();
    metrics.input_timing.received_events = 120;
    metrics.input_timing.dispatched_events = 119;
    metrics.input_timing.post_returns = 118;
    metrics.input_timing.last_post_age_ms = Some(4.5);
    metrics.input_timing.read_to_post.samples = 118;
    metrics.input_timing.read_to_post.max_ms = Some(42.0);
    metrics.capture_frames_delivered = 3000;
    metrics.display_updates_sent = 1500;
    metrics.client_reported_decoded_frames = Some(1498);
    let exported = safe_metrics(&metrics);
    assert_eq!(exported["input_timing"]["received_events"], 120);
    assert_eq!(exported["input_timing"]["read_to_post"]["max_ms"], 42.0);
    assert_eq!(exported["capture_frames_delivered"], 3000);
    assert_eq!(exported["display_updates_sent"], 1500);
    assert_eq!(exported["client_reported_decoded_frames"], 1498);
}

#[test]
fn persisted_input_progress_keeps_stages_and_numbers_without_input_payloads() {
    let secret = "private-input-canary";
    let record = json!({"level": "info", "message": "Input pipeline progress",
        "fields": {"phase": "complete", "state": "observed", "received_events": 12,
            "completed_dispatches": 11, "dispatch_in_progress": true, "current_dispatch_age_ms": 60.0,
            "post_returns": 11, "post_errors": 1, "untimed_events": 2,
            "last_post_age_ms": -1.0, "read_to_post_max_ms": 85.0,
            "captures_delivered": 30, "display_updates_sent": 20,
            "client_reported_decoded_frames": 19,
            "keys": secret, "coordinates": secret, "text": secret,
            "native_error": secret, "future_field": secret}});
    let summary = summarize_bytes(format!("{record}\n").as_bytes(), false);
    let event = &summary["events"][0];
    assert_eq!(event["category"], "input_pipeline");
    assert_eq!(event["fields"]["phase"], "complete");
    assert_eq!(event["fields"]["received_events"], 12);
    assert_eq!(event["fields"]["completed_dispatches"], 11);
    assert_eq!(event["fields"]["dispatch_in_progress"], true);
    assert_eq!(event["fields"]["current_dispatch_age_ms"], 60.0);
    assert_eq!(event["fields"]["last_post_age_ms"], -1.0);
    assert_eq!(event["fields"]["read_to_post_max_ms"], 85.0);
    assert_eq!(event["fields"]["captures_delivered"], 30);
    assert_eq!(event["fields"]["client_reported_decoded_frames"], 19);
    assert!(!summary.to_string().contains(secret));
    let malformed = json!({"level": "info", "message": "Input pipeline progress",
        "fields": {"received_events": secret, "post_returns": -1,
            "read_to_post_max_ms": -2, "last_post_age_ms": secret,
            "phase": secret, "state": secret}});
    let summary = summarize_bytes(format!("{malformed}\n").as_bytes(), false);
    assert!(!summary.to_string().contains(secret));
    assert_eq!(
        summary["events"][0]["fields"],
        json!({"phase":"unknown", "state":"unknown"})
    );
}

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
fn keyboard_diagnostics_include_only_supported_preference_names() {
    let mut config = UiConfig::default();
    config.keyboard_layout = "no".into();
    config.keyboard_shortcuts = "windows".into();
    let value = safe_configuration(&config);
    assert_eq!(value["keyboard_layout"], "no");
    assert_eq!(value["keyboard_shortcuts"], "windows");
    config.keyboard_layout = "unsupported-private-value".into();
    config.keyboard_shortcuts = "unsupported-private-value".into();
    assert!(!safe_configuration(&config)
        .to_string()
        .contains("unsupported-private-value"));
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
    config.keyboard_layout = secret.into();
    config.keyboard_shortcuts = secret.into();
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
