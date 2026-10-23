use super::*;
use macrdp_core::{ConnectionInfo, DisconnectionInfo};
use serde_json::json;

fn time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .to_rfc3339()
}
fn connected(start: i64) -> ConnectionEvent {
    ConnectionEvent::Connected(ConnectionInfo {
        client_ip: "private-address".into(),
        client_name: "private-name".into(),
        connected_at: time(start),
    })
}
fn disconnected(start: i64, end: i64, reason: &str) -> ConnectionEvent {
    ConnectionEvent::Disconnected(DisconnectionInfo {
        connected_at: time(start),
        disconnected_at: time(end),
        reason: reason.into(),
        ..Default::default()
    })
}
fn record(
    observer: &mut SessionStatusObserver,
    at: i64,
    component: &str,
    phase: &str,
    state: &str,
    id: u64,
    outcome: &str,
) {
    observer.record(
        1,
        &json!({"target":"macrdp_transition", "timestamp":time(at), "fields":{
        "component":component, "phase":phase, "state":state, "outcome":outcome,
        "worker_pid":10, "process_id":10, "owner_id":1, "operation_id":id, "transition_id":id,
        "retirement_id":id}})
        .to_string(),
    );
}
fn begin(observer: &mut SessionStatusObserver, start: i64) {
    record(
        observer,
        start - 1,
        "private_guard",
        "protection",
        "not_required",
        0,
        "",
    );
    record(observer, start, "session", "connection", "connected", 0, "");
    observer.connection(1, &connected(start));
}
fn bitmap(observer: &mut SessionStatusObserver, at: i64) {
    observer.metrics(
        1,
        &Metrics {
            timestamp_ms: at as u64,
            session_active: true,
            bytes_sent: 1024,
            sample_id: at as u64,
            transport_mode: "bitmap".into(),
            encoder_mode: "bitmap".into(),
            ..Default::default()
        },
    );
}
fn close(observer: &mut SessionStatusObserver, start: i64, end: i64) {
    record(
        observer,
        end - 2,
        "session",
        "connection",
        "disconnecting",
        0,
        "",
    );
    observer.connection(1, &disconnected(start, end, "client_disconnect"));
    record(
        observer,
        end + 1,
        "wake_lease",
        "cleanup",
        "complete",
        0,
        "",
    );
}
fn restored(observer: &mut SessionStatusObserver, at: i64, id: u64, disposition: &str) {
    record(
        observer,
        at,
        "native_capture",
        "restore_display",
        "begin",
        id,
        "",
    );
    record(observer, at + 1, "display", disposition, "begin", id, "");
    record(
        observer,
        at + 2,
        "display",
        disposition,
        "succeeded",
        id,
        "",
    );
    record(
        observer,
        at + 3,
        "native_capture",
        "restore_display",
        "end",
        id,
        "ok",
    );
}
#[test]
fn disconnect_and_metrics_do_not_invent_cleanup_success() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    assert!(observer.snapshot().current.is_none());
    begin(&mut observer, 1000);
    assert_eq!(
        observer.snapshot().current.unwrap().phase,
        "preparing_desktop"
    );
    record(
        &mut observer,
        1001,
        "encoder_startup",
        "startup",
        "begin",
        0,
        "",
    );
    assert_eq!(
        observer.snapshot().current.unwrap().phase,
        "waiting_encoder"
    );
    bitmap(&mut observer, 1002);
    assert_eq!(observer.snapshot().current.unwrap().phase, "streaming");
    close(&mut observer, 1000, 1100);
    let last = &observer.snapshot().recent[0];
    assert_eq!(last.outcome, "client_disconnect");
    assert_eq!(last.cleanup.native_encoder, Evidence::NotRequired);
    assert_eq!(last.cleanup.wake, Evidence::Complete);
    assert_eq!(last.cleanup.capture, Evidence::Unconfirmed);
    assert!(!last.cleanup.verified);
}
#[test]
fn verified_cleanup_distinguishes_retained_disabled_and_removed_display() {
    for (phase, expected) in [
        ("display_disable", "retained_disabled"),
        ("owner_retire", "removed"),
    ] {
        let mut observer = SessionStatusObserver::default();
        observer.begin_generation(1);
        begin(&mut observer, 2000);
        bitmap(&mut observer, 2001);
        close(&mut observer, 2000, 2100);
        restored(&mut observer, 2102, 3, phase);
        let last = &observer.snapshot().recent[0];
        assert!(last.cleanup.verified);
        assert_eq!(last.cleanup.display_disposition, expected);
    }
}
#[test]
fn incomplete_or_failed_cleanup_never_turns_green() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 3000);
    bitmap(&mut observer, 3001);
    close(&mut observer, 3000, 3100);
    // Completion without the exact operation's begin is not proof.
    record(
        &mut observer,
        3102,
        "native_capture",
        "restore_display",
        "end",
        9,
        "ok",
    );
    assert_eq!(
        observer.snapshot().recent[0].cleanup.display,
        Evidence::Unconfirmed
    );
    record(
        &mut observer,
        3103,
        "native_capture",
        "restore_display",
        "begin",
        10,
        "",
    );
    record(
        &mut observer,
        3104,
        "native_capture",
        "restore_display",
        "end",
        10,
        "error",
    );
    record(
        &mut observer,
        3105,
        "private_guard",
        "protection",
        "pending",
        0,
        "",
    );
    record(
        &mut observer,
        3106,
        "wake_lease",
        "cleanup",
        "pending",
        0,
        "",
    );
    let last = &observer.snapshot().recent[0];
    assert_eq!(last.cleanup.display, Evidence::Failed);
    assert_eq!(last.cleanup.private_guard, Evidence::Pending);
    assert_eq!(last.cleanup.wake, Evidence::Pending);
    assert!(!last.cleanup.verified);
}
#[test]
fn late_retirement_and_disconnect_only_update_the_matching_session() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 4000);
    record(
        &mut observer,
        4001,
        "video_encoder",
        "vt_create",
        "begin",
        5,
        "",
    );
    close(&mut observer, 4000, 4100);
    begin(&mut observer, 4200);
    bitmap(&mut observer, 4201);
    record(
        &mut observer,
        4202,
        "video_encoder",
        "retirement",
        "end",
        5,
        "ok",
    );
    observer.connection(1, &disconnected(4000, 4203, "peer_eof"));
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.current.unwrap().started_at_ms, 4200);
    assert_eq!(
        snapshot.recent[0].cleanup.native_encoder,
        Evidence::Complete
    );
    // The new session never inherits the previous session's restoration.
    record(
        &mut observer,
        4204,
        "native_capture",
        "restore_display",
        "end",
        7,
        "ok",
    );
    assert_eq!(
        observer.snapshot().current.unwrap().cleanup.display,
        Evidence::Unconfirmed
    );
}
#[test]
fn all_native_encoder_owners_must_retire_and_guard_must_report_completion() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    record(
        &mut observer,
        4999,
        "private_guard",
        "protection",
        "ready",
        0,
        "",
    );
    observer.connection(1, &connected(5000));
    record(
        &mut observer,
        5000,
        "session",
        "connection",
        "connected",
        0,
        "",
    );
    for id in [1, 2] {
        record(
            &mut observer,
            5001,
            "video_encoder",
            "vt_create",
            "begin",
            id,
            "",
        );
    }
    close(&mut observer, 5000, 5100);
    restored(&mut observer, 5101, 3, "owner_retire");
    record(
        &mut observer,
        5105,
        "video_encoder",
        "retirement",
        "end",
        1,
        "ok",
    );
    assert_eq!(
        observer.snapshot().recent[0].cleanup.native_encoder,
        Evidence::Pending
    );
    record(
        &mut observer,
        5106,
        "video_encoder",
        "retirement",
        "end",
        2,
        "ok",
    );
    assert!(!observer.snapshot().recent[0].cleanup.verified);
    record(
        &mut observer,
        5107,
        "private_guard",
        "protection",
        "complete",
        0,
        "",
    );
    assert!(observer.snapshot().recent[0].cleanup.verified);
}
#[test]
fn generation_reset_and_startup_reconstruction_cannot_verify_new_session() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 6000);
    // Display teardown during an initial failed/replaced preparation is not final cleanup.
    restored(&mut observer, 6001, 1, "owner_retire");
    assert_eq!(
        observer.snapshot().current.unwrap().cleanup.capture,
        Evidence::Unconfirmed
    );
    observer.begin_generation(2);
    observer.connection(2, &connected(6200));
    record(
        &mut observer,
        6201,
        "native_capture",
        "restore_display",
        "end",
        1,
        "ok",
    );
    observer.connection(1, &disconnected(6000, 6202, "client_disconnect"));
    let current = observer.snapshot().current.unwrap();
    assert_eq!(current.started_at_ms, 6200);
    assert!(!current.cleanup.verified);
    assert_eq!(observer.snapshot().recent[0].outcome, "server_restarted");
}
#[test]
fn history_and_output_are_bounded_and_exclude_untrusted_content() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    for index in 1..=30 {
        let at = index * 1000;
        begin(&mut observer, at);
        close(&mut observer, at, at + 100);
    }
    begin(&mut observer, 40_000);
    observer.connection(1, &disconnected(40_000, 40_100, "password=private-secret"));
    observer.record(
        1,
        &json!({"target":"macrdp_transition", "timestamp":time(40_101), "fields":{
        "component":"private-secret", "message":"private-secret"}})
        .to_string(),
    );
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.recent.len(), HISTORY_LIMIT);
    assert_eq!(snapshot.recent[0].outcome, "unknown");
    let encoded = serde_json::to_string(&snapshot).unwrap();
    for private in [
        "private-secret",
        "private-name",
        "private-address",
        "operation_id",
        "worker_pid",
    ] {
        assert!(!encoded.contains(private));
    }
}

#[test]
fn authenticated_marker_uses_exact_start_even_when_log_is_emitted_later() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    observer.connection(1, &connected(50_000));
    observer.record(
        1,
        &json!({"target":"macrdp_transition", "timestamp":time(50_007), "fields":{
        "component":"session", "phase":"connection", "state":"connected", "worker_pid":10,
        "session_started_unix_ms":50_000}})
        .to_string(),
    );
    bitmap(&mut observer, 50_008);
    close(&mut observer, 50_000, 50_100);
    restored(&mut observer, 50_101, 1, "owner_retire");
    assert_eq!(
        observer.snapshot().recent[0].cleanup.wake,
        Evidence::Complete
    );
}

#[test]
fn bounded_operation_overflow_is_sticky_and_cannot_verify_cleanup() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 60_000);
    // A pathological run with many reconfigured owners may exceed the observer's
    // bound. Keep the memory bound and report uncertainty, never an all-clear.
    for id in 1..=40 {
        record(
            &mut observer,
            60_001 + id as i64,
            "video_encoder",
            "vt_create",
            "begin",
            id,
            "",
        );
        record(
            &mut observer,
            60_001 + id as i64,
            "video_encoder",
            "retirement",
            "end",
            id,
            "ok",
        );
    }
    close(&mut observer, 60_000, 60_100);
    restored(&mut observer, 60_101, 99, "owner_retire");
    record(
        &mut observer,
        60_110,
        "video_encoder",
        "retirement",
        "end",
        1,
        "ok",
    );
    assert!(observer.recent[0].evidence_lost);
    assert!(observer.recent[0].operations.len() <= OPERATIONS_LIMIT);
    assert!(!observer.snapshot().recent[0].cleanup.verified);
}

#[test]
fn overlapping_restoration_requires_every_matching_completion_and_fresh_disposition() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 70_000);
    bitmap(&mut observer, 70_001);
    close(&mut observer, 70_000, 70_100);
    restored(&mut observer, 70_101, 1, "owner_retire");
    assert!(observer.snapshot().recent[0].cleanup.verified);
    record(
        &mut observer,
        70_110,
        "native_capture",
        "restore_display",
        "begin",
        2,
        "",
    );
    record(
        &mut observer,
        70_111,
        "native_capture",
        "restore_display",
        "begin",
        3,
        "",
    );
    assert_eq!(
        observer.snapshot().recent[0].cleanup.display_disposition,
        "unconfirmed"
    );
    record(
        &mut observer,
        70_112,
        "native_capture",
        "restore_display",
        "end",
        2,
        "ok",
    );
    assert_eq!(
        observer.snapshot().recent[0].cleanup.display,
        Evidence::Pending
    );
    assert!(!observer.snapshot().recent[0].cleanup.verified);
    record(
        &mut observer,
        70_113,
        "display",
        "display_disable",
        "begin",
        3,
        "",
    );
    record(
        &mut observer,
        70_114,
        "display",
        "display_disable",
        "succeeded",
        3,
        "",
    );
    record(
        &mut observer,
        70_115,
        "native_capture",
        "restore_display",
        "end",
        3,
        "ok",
    );
    assert!(observer.snapshot().recent[0].cleanup.verified);
    assert_eq!(observer.snapshot().recent[0].phase, "ended");
}

#[test]
fn unrelated_worker_cannot_supply_admission_or_cleanup_evidence() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    observer.record(1, &json!({"target":"macrdp_transition", "timestamp":time(79_999), "fields":{
        "component":"private_guard", "phase":"protection", "state":"not_required", "worker_pid":99}}).to_string());
    record(
        &mut observer,
        80_000,
        "session",
        "connection",
        "connected",
        0,
        "",
    );
    observer.connection(1, &connected(80_000));
    assert_eq!(
        observer.snapshot().current.unwrap().cleanup.private_guard,
        Evidence::Unconfirmed
    );
    bitmap(&mut observer, 80_001);
    close(&mut observer, 80_000, 80_100);
    for (component, phase, state, outcome) in [
        ("native_capture", "restore_display", "begin", ""),
        ("native_capture", "restore_display", "end", "ok"),
        ("display", "owner_retire", "begin", ""),
        ("display", "owner_retire", "succeeded", ""),
        ("video_encoder", "vt_create", "begin", ""),
        ("video_encoder", "retirement", "end", "ok"),
        ("private_guard", "protection", "complete", ""),
    ] {
        observer.record(1, &json!({"target":"macrdp_transition", "timestamp":time(80_101), "fields":{
            "component":component,"phase":phase,"state":state,"outcome":outcome,
            "worker_pid":99,"process_id":99,"owner_id":1,"operation_id":1,"retirement_id":1,"transition_id":1}}).to_string());
    }
    let last = &observer.snapshot().recent[0];
    assert_eq!(last.cleanup.private_guard, Evidence::Unconfirmed);
    assert_eq!(last.cleanup.capture, Evidence::Unconfirmed);
    assert_eq!(last.cleanup.display, Evidence::Unconfirmed);
    assert_eq!(last.cleanup.display_disposition, "unconfirmed");
    assert!(!last.cleanup.verified);
}

#[test]
fn contradictory_duplicate_restore_completion_keeps_failure_sticky() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 90_000);
    bitmap(&mut observer, 90_001);
    close(&mut observer, 90_000, 90_100);
    record(
        &mut observer,
        90_101,
        "native_capture",
        "restore_display",
        "begin",
        1,
        "",
    );
    record(
        &mut observer,
        90_102,
        "native_capture",
        "restore_display",
        "end",
        1,
        "error",
    );
    record(
        &mut observer,
        90_103,
        "native_capture",
        "restore_display",
        "end",
        1,
        "ok",
    );
    assert_eq!(
        observer.snapshot().recent[0].cleanup.display,
        Evidence::Failed
    );
    assert!(!observer.snapshot().recent[0].cleanup.verified);
}

fn diagnostic_loss(observer: &mut SessionStatusObserver, at: i64) {
    observer.record(
        1,
        &json!({"target":"macrdp_transition", "timestamp":time(at), "fields":{
        "component":"diagnostics", "phase":"queue", "state":"records_dropped", "worker_pid":10,
        "dropped_records":1}})
        .to_string(),
    );
}

#[test]
fn diagnostic_loss_before_connection_prevents_later_cleanup_verification() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    diagnostic_loss(&mut observer, 99_999);
    begin(&mut observer, 100_000);
    assert!(
        observer
            .snapshot()
            .current
            .unwrap()
            .cleanup
            .observation_limited
    );
    bitmap(&mut observer, 100_001);
    close(&mut observer, 100_000, 100_100);
    restored(&mut observer, 100_101, 1, "owner_retire");
    assert!(!observer.snapshot().recent[0].cleanup.verified);
}

#[test]
fn diagnostic_loss_during_connection_is_sticky_for_current_and_future_sessions() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 110_000);
    bitmap(&mut observer, 110_001);
    diagnostic_loss(&mut observer, 110_002);
    assert!(
        observer
            .snapshot()
            .current
            .unwrap()
            .cleanup
            .observation_limited
    );
    close(&mut observer, 110_000, 110_100);
    restored(&mut observer, 110_101, 1, "owner_retire");
    assert!(!observer.snapshot().recent[0].cleanup.verified);
    begin(&mut observer, 111_000);
    bitmap(&mut observer, 111_001);
    close(&mut observer, 111_000, 111_100);
    restored(&mut observer, 111_101, 2, "owner_retire");
    assert!(observer
        .snapshot()
        .recent
        .iter()
        .all(|s| s.cleanup.observation_limited && !s.cleanup.verified));
}

#[test]
fn diagnostic_loss_invalidates_recent_success_and_only_new_generation_clears_it() {
    let mut observer = SessionStatusObserver::default();
    observer.begin_generation(1);
    begin(&mut observer, 120_000);
    bitmap(&mut observer, 120_001);
    close(&mut observer, 120_000, 120_100);
    restored(&mut observer, 120_101, 1, "owner_retire");
    assert!(observer.snapshot().recent[0].cleanup.verified);
    diagnostic_loss(&mut observer, 120_110);
    assert!(observer.snapshot().recent[0].cleanup.observation_limited);
    assert!(!observer.snapshot().recent[0].cleanup.verified);
    // Repeated status/start signals from this same bridge cannot clear the gap.
    observer.begin_generation(1);
    assert!(observer.generation_observation_limited);
    observer.begin_generation(2);
    observer.connection(2, &connected(121_000));
    assert!(
        !observer
            .snapshot()
            .current
            .unwrap()
            .cleanup
            .observation_limited
    );
    assert!(observer.snapshot().recent[0].cleanup.observation_limited);
    // A delayed old bridge loss cannot contaminate this generation.
    diagnostic_loss(&mut observer, 121_001);
    assert!(
        !observer
            .snapshot()
            .current
            .unwrap()
            .cleanup
            .observation_limited
    );
}
