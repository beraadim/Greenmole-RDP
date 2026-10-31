//! Select only aggregate input counts and server-side timing summaries.

use super::safe_duration;
use serde_json::{json, Value};

/// Shared by structured metrics and persisted progress-event selection.
pub(super) const CLASSIFICATION_COUNTS: &[&str] = &[
    "keyboard_events",
    "mouse_move_events",
    "mouse_relative_move_events",
    "mouse_button_down_events",
    "mouse_button_up_events",
    "mouse_scroll_events",
    "keyboard_posts",
    "mouse_move_posts",
    "mouse_button_down_posts",
    "mouse_button_up_posts",
    "mouse_drag_posts",
    "mouse_scroll_posts",
    "other_posts",
];

/// Keep nested input diagnostics typed and explicitly selected even if future
/// metrics add raw input details. Never copy the entire serialized object.
pub(super) fn safe_input_timing(value: &Value) -> Value {
    let mut selected = serde_json::Map::new();
    for key in [
        "received_events",
        "untimed_events",
        "dispatched_events",
        "completed_dispatches",
        "native_callbacks",
        "blocked_callbacks",
        "unavailable_callbacks",
        "injection_failures",
        "post_attempts",
        "post_returns",
        "post_errors",
        "post_samples_dropped",
    ]
    .into_iter()
    .chain(CLASSIFICATION_COUNTS.iter().copied())
    {
        if let Some(count) = value.get(key).and_then(Value::as_u64) {
            selected.insert(key.into(), json!(count));
        }
    }
    for key in [
        "last_receive_age_ms",
        "last_dispatch_age_ms",
        "last_post_age_ms",
        "current_dispatch_age_ms",
    ] {
        if let Some(age) = value.get(key).filter(|age| safe_duration(age)) {
            selected.insert(key.into(), age.clone());
        }
    }
    if let Some(in_progress) = value.get("dispatch_in_progress").and_then(Value::as_bool) {
        selected.insert("dispatch_in_progress".into(), json!(in_progress));
    }
    for key in ["dispatch_wait", "handler", "native_post", "read_to_post"] {
        if let Some(timing) = value.get(key).filter(|timing| timing.is_object()) {
            let mut summary = serde_json::Map::new();
            if let Some(samples) = timing.get("samples").and_then(Value::as_u64) {
                summary.insert("samples".into(), json!(samples));
            }
            for field in ["p50_ms", "p95_ms", "p99_ms", "max_ms"] {
                if let Some(duration) = timing.get(field).filter(|duration| safe_duration(duration))
                {
                    summary.insert(field.into(), duration.clone());
                }
            }
            selected.insert(key.into(), Value::Object(summary));
        }
    }
    Value::Object(selected)
}
