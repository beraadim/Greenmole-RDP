//! Select startup evidence explicitly; never export pixels or cache metadata.

use serde_json::{json, Map, Value};

pub(super) fn safe_fields(category: &str, source: &Value) -> Option<Map<String, Value>> {
    let (phases, states): (&[&str], &[&str]) = match category {
        "capture_health" => (&["baseline", "progress"], &["observed"]),
        "capture_geometry" => (&["stream", "frame"], &["prepared", "observed"]),
        "graphics_surface" => (&["created"], &["prepared"]),
        "graphics_startup" => (&["first_avc"], &["prepared"]),
        "graphics_cache" => (&["offer"], &["declined"]),
        "graphics_receive" => (&["envelope"], &["ignored", "rejected"]),
        "bitmap_transport" => (&["activation"], &["configured"]),
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
                "compared_sample_points",
            ] {
                unsigned(source, &mut fields, key);
            }
            if let Some(changed) = source["changed_sample_points"]
                .as_i64()
                .filter(|count| (-1..=256).contains(count))
            {
                fields.insert("changed_sample_points".into(), json!(changed));
            }
            for key in [
                "sample_available",
                "comparison_available",
                "timestamp_available",
                "timestamp_baseline_available",
                "timestamp_advanced",
                "timestamp_duplicate",
                "timestamp_regressed",
            ] {
                boolean(source, &mut fields, key);
            }
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
        "capture_geometry" => {
            for key in [
                "display_id",
                "source_width",
                "source_height",
                "requested_width",
                "requested_height",
                "output_width",
                "output_height",
            ] {
                unsigned(source, &mut fields, key);
            }
            for key in [
                "cg_geometry_available",
                "content_rect_available",
                "content_pixels_available",
                "scale_factor_available",
                "content_scale_available",
            ] {
                boolean(source, &mut fields, key);
            }
            // These describe capture/display geometry, never pointer positions.
            // Origins may legitimately be negative on an extended desktop.
            for key in [
                "cg_origin_x",
                "cg_origin_y",
                "cg_logical_width",
                "cg_logical_height",
                "content_rect_x",
                "content_rect_y",
                "content_rect_width",
                "content_rect_height",
                "content_pixels_x",
                "content_pixels_y",
                "content_pixels_width",
                "content_pixels_height",
                "scale_factor",
                "content_scale",
            ] {
                if let Some(value) = source[key].as_f64().filter(|value| value.is_finite()) {
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
        "bitmap_transport" => {
            for key in [
                "surface_command_id",
                "max_bitmap_update_bytes",
                "bitmap_wire_depth",
                "client_preferred_depth",
                "client_general_flags",
                "client_bitmap_drawing_flags",
                "activation_width",
                "activation_height",
            ] {
                unsigned(source, &mut fields, key);
            }
            for key in [
                "remotefx_image_mode",
                "surface_frame_markers",
                "bitmap_single_rectangle_updates",
                "bitmap_compression_header_present",
                "bitmap_alpha_skipped",
            ] {
                boolean(source, &mut fields, key);
            }
            fields.insert(
                "bitmap_codec".into(),
                json!(super::selection(
                    source["bitmap_codec"].as_str().unwrap_or(""),
                    &[
                        "surface-raw",
                        "rdp6-rle",
                        "remotefx",
                        "qoi",
                        "qoiz",
                        "encoding"
                    ],
                )),
            );
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
