use super::*;

#[test]
fn disconnect_releases_both_assertions_resets_throttle_and_cleanup_is_idempotent() {
    let mut lease = WakeLease {
        display_sleep_assertion_id: Some(101),
        user_activity_assertion_id: Some(102),
        last_user_activity: Some(Instant::now()),
    };
    let mut released = Vec::new();
    lease.release_with(|id| {
        released.push(id);
        K_IORETURN_SUCCESS
    });
    assert_eq!(released, [101, 102]);
    assert!(lease.display_sleep_assertion_id.is_none());
    assert!(lease.user_activity_assertion_id.is_none());
    assert!(lease.last_user_activity.is_none());
    // Drop uses the same release path. Already-ended sessions cannot release
    // stale IDs again, including after macOS has potentially reused an ID.
    lease.release_with(|_| panic!("released an assertion twice"));
}

#[test]
fn timed_out_activity_is_treated_as_released_even_without_display_sleep_ownership() {
    for expired in [K_IORETURN_NOT_FOUND, K_IORETURN_BAD_ARGUMENT] {
        let mut lease = WakeLease {
            user_activity_assertion_id: Some(201),
            last_user_activity: Some(Instant::now()),
            ..Default::default()
        };
        lease.release_with(|id| {
            assert_eq!(id, 201);
            expired
        });
        assert!(lease.user_activity_assertion_id.is_none());
        assert!(lease.last_user_activity.is_none());
        lease.release_with(|_| panic!("expired activity was released again"));
    }
}

#[test]
fn unexpected_release_failure_does_not_skip_other_assertion_and_only_failure_is_retried() {
    let mut lease = WakeLease {
        display_sleep_assertion_id: Some(301),
        user_activity_assertion_id: Some(302),
        last_user_activity: Some(Instant::now()),
    };
    let mut attempted = Vec::new();
    lease.release_with(|id| {
        attempted.push(id);
        if id == 301 {
            -1
        } else {
            K_IORETURN_SUCCESS
        }
    });
    assert_eq!(attempted, [301, 302]);
    assert_eq!(lease.display_sleep_assertion_id, Some(301));
    assert!(lease.user_activity_assertion_id.is_none());
    assert!(lease.last_user_activity.is_none());
    lease.release_with(|id| {
        assert_eq!(id, 301);
        K_IORETURN_SUCCESS
    });
    assert!(lease.display_sleep_assertion_id.is_none());
    lease.release_with(|_| panic!("successfully retried release repeated"));
}

#[test]
fn consecutive_sessions_do_not_reuse_ended_wake_leases() {
    let mut lease = WakeLease::default();
    let mut released = Vec::new();
    for session in 1..=3 {
        assert!(lease.display_sleep_assertion_id.is_none());
        assert!(lease.user_activity_assertion_id.is_none());
        assert!(lease.last_user_activity.is_none());
        lease.display_sleep_assertion_id = Some(session * 10);
        lease.user_activity_assertion_id = Some(session * 10 + 1);
        lease.last_user_activity = Some(Instant::now());
        lease.release_with(|id| {
            released.push(id);
            K_IORETURN_SUCCESS
        });
    }
    assert_eq!(released, [10, 11, 20, 21, 30, 31]);
    lease.release_with(|_| panic!("completed sessions retained ownership"));
}
