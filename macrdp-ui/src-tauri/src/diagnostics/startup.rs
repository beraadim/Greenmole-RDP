//! Select startup evidence explicitly; never export pixels or cache metadata.

use serde_json::{json, Map, Value};

pub(super) fn safe_fields(category: &str, source: &Value) -> Option<Map<String, Value>> {
    let (phases, states): (&[&str], &[&str]) = match category {
        "capture_health" => (&["baseline", "progress"], &["observed"]),
        "graphics_surface" => (&["created"], &["prepared"]),
        "graphics_startup" => (&["first_avc"], &["prepared"]),
        "graphics_cache" => (&["offer"], &["declined"]),
        "graphics_receive" => (&["envelope"], &["ignored", "rejected"]),
        _ => return None,
    };
    let mut fields = Map::new();
    for key in ["worker_pid", "width", "height"] {
        unsigned(source, &mut fields, key);
    }
    for (key, allowed) in [("phase", phases), ("state", states)] {
        fields.insert(
            key.into(),
            json!(super::selection(
                source[key].as_str().unwrap_or(""),
                allowed
            )),
        );
    }
    match category {
        "capture_health" => {
            for key in [
                "display_id",
                "elapsed_ms",
                "sampled_pixels",
                "near_black_pixels",
                "near_black_luma_max",
            ] {
                unsigned(source, &mut fields, key);
            }
            boolean(source, &mut fields, "sample_available");
            for key in ["luma_min", "luma_max", "luma_mean"] {
                if let Some(value) = source[key].as_f64().filter(|value| {
                    value.is_finite() && (*value == -1.0 || (0.0..=255.0).contains(value))
                }) {
                    fields.insert(key.into(), json!(value));
                }
            }
            fields.insert(
                "source_format".into(),
                json!(super::selection(
                    source["source_format"].as_str().unwrap_or(""),
                    &["bgra", "nv12_unsampled"],
                )),
            );
        }
        "graphics_surface" | "graphics_startup" => {
            for key in [
                "surface_id",
                "surface_generation",
                "graphics_frame_id",
                "frame_width",
                "frame_height",
                "encoded_width",
                "encoded_height",
                "bitmap_dispatches",
                "uncompressed_dispatches",
                "display_updates_sent",
                "main_bytes",
                "aux_bytes",
            ] {
                unsigned(source, &mut fields, key);
            }
            for key in [
                "avc_frame",
                "keyframe",
                "deleted_previous_surface",
                "avc444",
                "main_has_sps",
                "main_has_pps",
                "main_has_idr",
                "aux_has_sps",
                "aux_has_pps",
                "aux_has_idr",
            ] {
                boolean(source, &mut fields, key);
            }
        }
        "graphics_cache" => {
            for key in ["offer_count", "reply_count"] {
                unsigned(source, &mut fields, key);
            }
        }
        "graphics_receive" => {
            for key in [
                "cmd_id",
                "header_flags",
                "declared_bytes",
                "available_bytes",
                "stream_offset",
            ] {
                unsigned(source, &mut fields, key);
            }
        }
        _ => unreachable!(),
    }
    Some(fields)
}

fn unsigned(source: &Value, fields: &mut Map<String, Value>, key: &str) {
    if let Some(value) = source[key].as_u64() {
        fields.insert(key.into(), json!(value));
    }
}

fn boolean(source: &Value, fields: &mut Map<String, Value>, key: &str) {
    if let Some(value) = source[key].as_bool() {
        fields.insert(key.into(), json!(value));
    }
}
