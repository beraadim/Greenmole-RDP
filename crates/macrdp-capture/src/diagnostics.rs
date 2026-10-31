//! Fixed capture geometry records, without window identities or image content.
//!
//! One stream record describes the requested source/output. Only the first
//! complete frame records SCK's content transform, bounding native callbacks to
//! one geometry event per stream even if a producer sends repeated frames.

use crate::{CaptureConfig, CapturePixelFormat, CapturedFrame, ContentRect};
use core_graphics::display::CGDisplay;
use screencapturekit::cm::CMSampleBuffer;
use std::sync::atomic::{AtomicBool, Ordering};

fn source_format(format: CapturePixelFormat) -> &'static str {
    match format {
        CapturePixelFormat::Bgra => "bgra",
        CapturePixelFormat::Nv12 => "nv12_unsampled",
    }
}

pub(crate) fn stream_prepared(
    config: &CaptureConfig,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
) {
    let bounds = CGDisplay::new(config.display_id).bounds();
    let cg_geometry_available = bounds.origin.x.is_finite()
        && bounds.origin.y.is_finite()
        && bounds.size.width.is_finite()
        && bounds.size.height.is_finite()
        && bounds.size.width > 0.0
        && bounds.size.height > 0.0;
    let value = |value| if cg_geometry_available { value } else { -1.0 };
    tracing::info!(target: "macrdp_transition",
        component = "capture_geometry", phase = "stream", state = "prepared",
        worker_pid = std::process::id(), display_id = config.display_id,
        source_width, source_height,
        requested_width = config.width, requested_height = config.height,
        output_width, output_height, source_format = source_format(config.pixel_format),
        cg_geometry_available,
        cg_origin_x = value(bounds.origin.x), cg_origin_y = value(bounds.origin.y),
        cg_logical_width = value(bounds.size.width), cg_logical_height = value(bounds.size.height),
        "Capture geometry stream prepared"
    );
}

pub(crate) struct FrameGeometryDiagnostics {
    recorded: AtomicBool,
    display_id: u32,
    format: CapturePixelFormat,
}

impl FrameGeometryDiagnostics {
    pub(crate) fn new(display_id: u32, format: CapturePixelFormat) -> Self {
        Self {
            recorded: AtomicBool::new(false),
            display_id,
            format,
        }
    }

    pub(crate) fn observe(&self, sample: &CMSampleBuffer, frame: &CapturedFrame) {
        if self.recorded.swap(true, Ordering::Relaxed) {
            return;
        }
        let points = rect_values(sample.content_rect().map(|rect| ContentRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }));
        let pixels = rect_values(frame.content_rect);
        let (scale_factor_available, scale_factor) = positive_value(sample.scale_factor());
        let (content_scale_available, content_scale) = positive_value(sample.content_scale());
        tracing::info!(target: "macrdp_transition",
            component = "capture_geometry", phase = "frame", state = "observed",
            worker_pid = std::process::id(), display_id = self.display_id,
            width = frame.width, height = frame.height, source_format = source_format(self.format),
            content_rect_available = points.available,
            content_rect_x = points.x, content_rect_y = points.y,
            content_rect_width = points.width, content_rect_height = points.height,
            content_pixels_available = pixels.available,
            content_pixels_x = pixels.x, content_pixels_y = pixels.y,
            content_pixels_width = pixels.width, content_pixels_height = pixels.height,
            scale_factor_available, scale_factor, content_scale_available, content_scale,
            "Capture geometry first frame observed"
        );
    }
}

#[derive(Debug, PartialEq)]
struct RectValues {
    available: bool,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn rect_values(rect: Option<ContentRect>) -> RectValues {
    match rect.filter(|rect| {
        rect.x.is_finite()
            && rect.y.is_finite()
            && rect.width.is_finite()
            && rect.height.is_finite()
            && rect.width > 0.0
            && rect.height > 0.0
    }) {
        Some(rect) => RectValues {
            available: true,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        },
        None => RectValues {
            available: false,
            x: -1.0,
            y: -1.0,
            width: -1.0,
            height: -1.0,
        },
    }
}

fn positive_value(value: Option<f64>) -> (bool, f64) {
    match value.filter(|value| value.is_finite() && *value > 0.0) {
        Some(value) => (true, value),
        None => (false, -1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_rejects_missing_or_nonfinite_values_but_preserves_negative_origins() {
        let good = ContentRect {
            x: -1.0,
            y: -1200.0,
            width: 1920.0,
            height: 1200.0,
        };
        let values = rect_values(Some(good));
        assert!(values.available);
        assert_eq!(values.x, -1.0);
        assert_eq!(values.y, -1200.0);
        assert!(!rect_values(None).available);
        for bad in [
            ContentRect {
                x: f64::NAN,
                ..good
            },
            ContentRect {
                y: f64::INFINITY,
                ..good
            },
            ContentRect { width: 0.0, ..good },
            ContentRect {
                height: -1.0,
                ..good
            },
        ] {
            let values = rect_values(Some(bad));
            assert!(!values.available);
            assert_eq!(values.x, -1.0);
            assert_eq!(values.width, -1.0);
        }
        for bad in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            assert_eq!(positive_value(bad), (false, -1.0));
        }
        assert_eq!(positive_value(Some(0.5)), (true, 0.5));
        assert_eq!(positive_value(Some(2.0)), (true, 2.0));
    }
}
