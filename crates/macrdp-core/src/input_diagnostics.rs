//! Low-frequency, payload-free evidence for the server input pipeline.
//!
//! Emit from the existing metrics ticker, never from an input event callback.
//! Posting completion only means the CoreGraphics API returned; neither these
//! stages nor graphics acknowledgements prove application-level consumption.

use crate::Metrics;

/// Keep missing samples distinct from a genuinely zero-duration measurement.
/// JSON diagnostic exports use null; these flat log fields use -1 for absence.
fn measured(value: Option<f64>) -> f64 {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(-1.0)
}

pub(crate) fn log_progress(metrics: &Metrics, final_sample: bool) {
    if !metrics.session_active && !final_sample {
        return;
    }
    let input = &metrics.input_timing;
    // Enumerate safe fields instead of formatting Metrics or handler state:
    // future fields must not implicitly enter persisted diagnostics.
    tracing::info!(target: "macrdp_transition",
        component = "input_pipeline",
        phase = if final_sample { "complete" } else { "progress" },
        state = "observed",
        worker_pid = std::process::id(),
        sample_id = metrics.sample_id,
        received_events = input.received_events,
        untimed_events = input.untimed_events,
        dispatched_events = input.dispatched_events,
        completed_dispatches = input.completed_dispatches,
        dispatch_in_progress = input.dispatch_in_progress,
        current_dispatch_age_ms = measured(input.current_dispatch_age_ms),
        native_callbacks = input.native_callbacks,
        keyboard_events = input.keyboard_events,
        mouse_move_events = input.mouse_move_events,
        mouse_relative_move_events = input.mouse_relative_move_events,
        mouse_button_down_events = input.mouse_button_down_events,
        mouse_button_up_events = input.mouse_button_up_events,
        mouse_scroll_events = input.mouse_scroll_events,
        blocked_callbacks = input.blocked_callbacks,
        unavailable_callbacks = input.unavailable_callbacks,
        injection_failures = input.injection_failures,
        post_attempts = input.post_attempts,
        post_returns = input.post_returns,
        post_errors = input.post_errors,
        post_samples_dropped = input.post_samples_dropped,
        keyboard_posts = input.keyboard_posts,
        mouse_move_posts = input.mouse_move_posts,
        mouse_button_down_posts = input.mouse_button_down_posts,
        mouse_button_up_posts = input.mouse_button_up_posts,
        mouse_drag_posts = input.mouse_drag_posts,
        mouse_scroll_posts = input.mouse_scroll_posts,
        other_posts = input.other_posts,
        last_receive_age_ms = measured(input.last_receive_age_ms),
        last_dispatch_age_ms = measured(input.last_dispatch_age_ms),
        last_post_age_ms = measured(input.last_post_age_ms),
        dispatch_wait_p95_ms = measured(input.dispatch_wait.p95_ms),
        dispatch_wait_max_ms = measured(input.dispatch_wait.max_ms),
        handler_p95_ms = measured(input.handler.p95_ms),
        handler_max_ms = measured(input.handler.max_ms),
        native_post_p95_ms = measured(input.native_post.p95_ms),
        native_post_max_ms = measured(input.native_post.max_ms),
        read_to_post_p95_ms = measured(input.read_to_post.p95_ms),
        read_to_post_max_ms = measured(input.read_to_post.max_ms),
        captures_delivered = metrics.capture_frames_delivered,
        display_updates_sent = metrics.display_updates_sent,
        client_reported_decoded_frames = metrics.client_reported_decoded_frames,
        pending_acks = metrics.pending_acks,
        bytes_sent = metrics.bytes_sent,
        "Input pipeline progress"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{EnvFilter, Layer};

    struct Fields(Vec<String>);
    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.push(format!("{}={value:?}", field.name()));
        }
    }
    struct Capture(Arc<Mutex<Vec<Vec<String>>>>);
    impl<S: Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0);
        }
    }

    #[test]
    fn default_warning_logging_retains_safe_active_and_final_progress_only() {
        let records = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(crate::with_transition_diagnostics(EnvFilter::new("warn")))
            .with(Capture(records.clone()));
        let mut metrics = Metrics {
            encoder_mode: "private-canary-value".into(),
            transport_mode: "private-canary-value".into(),
            connection_outcome: Some("private-canary-value".into()),
            ..Default::default()
        };
        metrics.input_timing.received_events = 7;
        metrics.input_timing.post_returns = 5;
        metrics.input_timing.mouse_button_down_events = 3;
        metrics.input_timing.mouse_button_up_events = 2;
        metrics.input_timing.mouse_button_down_posts = 2;
        metrics.input_timing.mouse_button_up_posts = 1;
        metrics.input_timing.mouse_drag_posts = 4;
        tracing::subscriber::with_default(subscriber, || {
            log_progress(&metrics, false);
            metrics.session_active = true;
            log_progress(&metrics, false);
            metrics.session_active = false;
            log_progress(&metrics, true);
        });
        let records = records.lock().unwrap();
        assert_eq!(
            records.len(),
            2,
            "idle polling must not create progress records"
        );
        let text = format!("{records:?}");
        assert!(!text.contains("private-canary-value"));
        assert!(records[0].contains(&"received_events=7".into()));
        assert!(records[0].contains(&"post_returns=5".into()));
        assert!(records[0].contains(&"mouse_button_down_events=3".into()));
        assert!(records[0].contains(&"mouse_button_up_events=2".into()));
        assert!(records[0].contains(&"mouse_button_down_posts=2".into()));
        assert!(records[0].contains(&"mouse_button_up_posts=1".into()));
        assert!(records[0].contains(&"mouse_drag_posts=4".into()));
        assert!(records[0].contains(&"last_post_age_ms=-1.0".into()));
        assert!(records[0].contains(&"phase=\"progress\"".into()));
        assert!(records[1].contains(&"phase=\"complete\"".into()));
    }
}
