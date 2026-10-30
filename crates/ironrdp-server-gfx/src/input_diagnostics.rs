//! Payload-free input stages measured on the server's monotonic clock.
//! A returned posting call is not evidence that an application consumed it.

use std::time::Instant;

use super::timings::{TimingSummary, TimingWindow};

/// One native handler callback. Samples contain durations only and must remain
/// bounded by the producer; counters also cover samples omitted by that bound.
#[derive(Debug, Default)]
pub struct NativeInputObservation {
    pub callback_blocked: bool,
    pub callback_unavailable: bool,
    pub callback_failed: bool,
    pub post_attempts: u64,
    pub post_returns: u64,
    /// Preparation errors before a posting call; the OS posting API is void.
    pub post_errors: u64,
    pub post_samples_dropped: u64,
    pub last_post_at: Option<Instant>,
    pub post_ms: Vec<f64>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct InputTimingSnapshot {
    pub received_events: u64,
    pub untimed_events: u64,
    pub dispatched_events: u64,
    pub completed_dispatches: u64,
    pub dispatch_in_progress: bool,
    pub current_dispatch_age_ms: Option<f64>,
    pub native_callbacks: u64,
    pub blocked_callbacks: u64,
    pub unavailable_callbacks: u64,
    pub injection_failures: u64,
    pub post_attempts: u64,
    pub post_returns: u64,
    pub post_errors: u64,
    pub post_samples_dropped: u64,
    pub last_receive_age_ms: Option<f64>,
    pub last_dispatch_age_ms: Option<f64>,
    pub last_post_age_ms: Option<f64>,
    pub dispatch_wait: TimingSummary,
    pub handler: TimingSummary,
    pub native_post: TimingSummary,
    pub read_to_post: TimingSummary,
}

/// Replaced only at a real connection boundary. Resize activation keeps input
/// history so retained PDUs' wait time and injection progress remain visible.
#[derive(Debug, Default)]
pub struct InputDiagnostics {
    totals: InputTimingSnapshot,
    last_receive_at: Option<Instant>,
    last_dispatch_at: Option<Instant>,
    last_post_at: Option<Instant>,
    dispatch_received_at: Option<Instant>,
    dispatch_started_at: Option<Instant>,
    dispatch_wait: TimingWindow,
    handler: TimingWindow,
    native_post: TimingWindow,
    read_to_post: TimingWindow,
}

fn millis(later: Instant, earlier: Instant) -> f64 {
    later.saturating_duration_since(earlier).as_secs_f64() * 1000.0
}

impl InputDiagnostics {
    pub fn record_received(&mut self, received_at: Option<Instant>, count: u64) {
        self.totals.received_events = self.totals.received_events.saturating_add(count);
        if count == 0 {
            return;
        }
        if let Some(at) = received_at {
            self.last_receive_at = Some(self.last_receive_at.map_or(at, |last| last.max(at)));
        } else {
            self.last_receive_at = None;
            self.totals.untimed_events = self.totals.untimed_events.saturating_add(count);
        }
    }

    pub fn begin_dispatch(&mut self, received_at: Option<Instant>, started_at: Instant) {
        self.totals.dispatched_events = self.totals.dispatched_events.saturating_add(1);
        self.last_dispatch_at = Some(started_at);
        self.dispatch_received_at = received_at;
        self.dispatch_started_at = Some(started_at);
        if let Some(at) = received_at {
            self.dispatch_wait.record(millis(started_at, at));
        }
    }

    pub fn end_dispatch(&mut self, started_at: Instant, finished_at: Instant) {
        self.handler.record(millis(finished_at, started_at));
        self.totals.completed_dispatches = self.totals.completed_dispatches.saturating_add(1);
        self.dispatch_received_at = None;
        self.dispatch_started_at = None;
    }

    pub fn record_native_callback(&mut self, observation: NativeInputObservation) {
        self.totals.native_callbacks = self.totals.native_callbacks.saturating_add(1);
        self.totals.blocked_callbacks = self
            .totals
            .blocked_callbacks
            .saturating_add(u64::from(observation.callback_blocked));
        self.totals.unavailable_callbacks = self
            .totals
            .unavailable_callbacks
            .saturating_add(u64::from(observation.callback_unavailable));
        self.totals.injection_failures = self
            .totals
            .injection_failures
            .saturating_add(u64::from(observation.callback_failed));
        self.totals.post_attempts = self
            .totals
            .post_attempts
            .saturating_add(observation.post_attempts);
        self.totals.post_returns = self
            .totals
            .post_returns
            .saturating_add(observation.post_returns);
        self.totals.post_errors = self
            .totals
            .post_errors
            .saturating_add(observation.post_errors);
        self.totals.post_samples_dropped = self
            .totals
            .post_samples_dropped
            .saturating_add(observation.post_samples_dropped)
            .saturating_add(observation.post_ms.len().saturating_sub(64) as u64);
        // Enforce a bound here as well as at the native producer boundary.
        for ms in observation.post_ms.into_iter().take(64) {
            self.native_post.record(ms);
        }
        if let Some(at) = observation.last_post_at {
            self.last_post_at = Some(at);
            if let Some(received_at) = self.dispatch_received_at {
                self.read_to_post.record(millis(at, received_at));
            }
        }
    }

    pub fn snapshot(&self, now: Instant) -> InputTimingSnapshot {
        InputTimingSnapshot {
            dispatch_in_progress: self.dispatch_started_at.is_some(),
            current_dispatch_age_ms: self.dispatch_started_at.map(|at| millis(now, at)),
            last_receive_age_ms: self.last_receive_at.map(|at| millis(now, at)),
            last_dispatch_age_ms: self.last_dispatch_at.map(|at| millis(now, at)),
            last_post_age_ms: self.last_post_at.map(|at| millis(now, at)),
            dispatch_wait: self.dispatch_wait.summary(),
            handler: self.handler.summary(),
            native_post: self.native_post.summary(),
            read_to_post: self.read_to_post.summary(),
            ..self.totals
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn separate_queue_handler_and_native_stages_keep_brief_stalls() {
        let read = Instant::now();
        let dispatch = read + Duration::from_millis(150);
        let post = dispatch + Duration::from_millis(3);
        let mut stats = InputDiagnostics::default();
        stats.record_received(Some(read), 1);
        stats.begin_dispatch(Some(read), dispatch);
        let pending = stats.snapshot(dispatch + Duration::from_millis(2));
        assert!(pending.dispatch_in_progress);
        assert_eq!(pending.current_dispatch_age_ms, Some(2.0));
        assert_eq!(pending.completed_dispatches, 0);
        stats.record_native_callback(NativeInputObservation {
            post_attempts: 1,
            post_returns: 1,
            last_post_at: Some(post),
            post_ms: vec![0.2],
            ..Default::default()
        });
        stats.end_dispatch(dispatch, post + Duration::from_millis(2));
        let snapshot = stats.snapshot(post + Duration::from_millis(10));
        assert_eq!(snapshot.received_events, 1);
        assert_eq!(snapshot.dispatched_events, 1);
        assert_eq!(snapshot.completed_dispatches, 1);
        assert!(!snapshot.dispatch_in_progress);
        assert_eq!(snapshot.current_dispatch_age_ms, None);
        assert_eq!(snapshot.native_callbacks, 1);
        assert_eq!(snapshot.dispatch_wait.max_ms, Some(150.0));
        assert_eq!(snapshot.handler.max_ms, Some(5.0));
        assert_eq!(snapshot.native_post.max_ms, Some(0.2));
        assert_eq!(snapshot.read_to_post.max_ms, Some(153.0));
        assert_eq!(snapshot.last_receive_age_ms, Some(163.0));
        assert_eq!(snapshot.last_post_age_ms, Some(10.0));
    }

    #[test]
    fn untimed_activation_input_and_unavailable_injectors_do_not_claim_posting() {
        let now = Instant::now();
        let mut stats = InputDiagnostics::default();
        stats.record_received(None, 2);
        stats.begin_dispatch(None, now);
        stats.record_native_callback(NativeInputObservation {
            callback_unavailable: true,
            ..Default::default()
        });
        stats.end_dispatch(now, now);
        let snapshot = stats.snapshot(now);
        assert_eq!(snapshot.untimed_events, 2);
        assert_eq!(snapshot.unavailable_callbacks, 1);
        assert_eq!(snapshot.post_returns, 0);
        assert_eq!(snapshot.dispatch_wait.samples, 0);
        assert_eq!(snapshot.read_to_post.samples, 0);
        assert_eq!(snapshot.last_receive_age_ms, None);
        assert_eq!(snapshot.last_post_age_ms, None);
    }

    #[test]
    fn totals_survive_bounded_native_samples_and_new_connections_start_empty() {
        let mut stats = InputDiagnostics::default();
        for _ in 0..20 {
            stats.record_native_callback(NativeInputObservation {
                callback_failed: true,
                callback_blocked: true,
                post_attempts: 100,
                post_returns: 99,
                post_errors: 1,
                post_samples_dropped: 36,
                post_ms: vec![2.0; 64],
                ..Default::default()
            });
        }
        let snapshot = stats.snapshot(Instant::now());
        assert_eq!(snapshot.post_attempts, 2000);
        assert_eq!(snapshot.post_returns, 1980);
        assert_eq!(snapshot.post_errors, 20);
        assert_eq!(snapshot.native_post.samples, 1024);
        assert_eq!(snapshot.post_samples_dropped, 720);
        let fresh = InputDiagnostics::default().snapshot(Instant::now());
        assert_eq!(fresh.received_events, 0);
        assert_eq!(fresh.native_post.samples, 0);
    }
}
