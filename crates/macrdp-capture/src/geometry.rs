//! Coordinate geometry shared by the UI server and standalone server.

use std::sync::{Arc, Mutex};

/// Visible source content in output-frame **pixels**, including any padding offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ContentRect {
    /// SCK's frame contentRect is in surface points. Its scaleFactor converts
    /// those points to pixels. contentScale is already reflected in the rect;
    /// applying it again would scale the fitted content twice.
    pub fn from_surface_points(self, scale_factor: f64) -> Option<Self> {
        if !scale_factor.is_finite() || scale_factor <= 0.0 {
            return None;
        }
        let pixels = Self {
            x: self.x * scale_factor,
            y: self.y * scale_factor,
            width: self.width * scale_factor,
            height: self.height * scale_factor,
        };
        pixels.is_valid().then_some(pixels)
    }

    fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
    }
}

/// Maps the negotiated RDP desktop through the visible capture content to the
/// selected display's global CoreGraphics points. Encoder padding is excluded.
#[derive(Debug, Clone)]
pub struct MouseCoordMapper(Arc<Mutex<CoordState>>);

#[derive(Debug)]
struct CoordState {
    origin_x: f64,
    origin_y: f64,
    logical_w: f64,
    logical_h: f64,
    rdp_w: f64,
    rdp_h: f64,
    content: ContentRect,
}

impl MouseCoordMapper {
    pub fn new(logical_w: u16, logical_h: u16, rdp_w: u16, rdp_h: u16) -> Self {
        let rdp_w = f64::from(rdp_w.max(1));
        let rdp_h = f64::from(rdp_h.max(1));
        tracing::info!(
            logical_w,
            logical_h,
            rdp_w,
            rdp_h,
            "Mouse coordinate mapper initialized"
        );
        Self(Arc::new(Mutex::new(CoordState {
            origin_x: 0.0,
            origin_y: 0.0,
            logical_w: f64::from(logical_w.max(1)),
            logical_h: f64::from(logical_h.max(1)),
            rdp_w,
            rdp_h,
            content: ContentRect {
                x: 0.0,
                y: 0.0,
                width: rdp_w,
                height: rdp_h,
            },
        })))
    }

    /// Preserve subpoint precision and signed global coordinates through injection.
    pub fn map(&self, rdp_x: u16, rdp_y: u16) -> (f64, f64) {
        let s = self.0.lock().unwrap();
        let x = map_axis(rdp_x, s.content.x, s.content.width, s.origin_x, s.logical_w);
        let y = map_axis(
            rdp_y,
            s.content.y,
            s.content.height,
            s.origin_y,
            s.logical_h,
        );
        (x, y)
    }

    /// Apply RDP pixel deltas to a global CoreGraphics point position. Relative
    /// motion uses the same content scale as absolute motion, including Retina
    /// capture and letterboxing, and stays within the selected display.
    pub fn map_relative(&self, current: (f64, f64), dx: i32, dy: i32) -> (f64, f64) {
        let s = self.0.lock().unwrap();
        let apply = |value: f64, delta: i32, origin: f64, extent: f64, pixels: f64| {
            let current = if value.is_finite() { value } else { origin };
            (current + f64::from(delta) * extent / pixels)
                .clamp(origin, origin + (extent - 1.0).max(0.0))
        };
        (
            apply(current.0, dx, s.origin_x, s.logical_w, s.content.width),
            apply(current.1, dy, s.origin_y, s.logical_h, s.content.height),
        )
    }

    /// Set the selected display's global bounds before activating its capture stream.
    /// Invalid or unrepresentable bounds leave the previous mapping intact.
    pub fn update_display_bounds(
        &self,
        origin_x: f64,
        origin_y: f64,
        logical_width: f64,
        logical_height: f64,
    ) {
        if !valid_axis(origin_x, logical_width) || !valid_axis(origin_y, logical_height) {
            tracing::warn!(
                origin_x,
                origin_y,
                logical_width,
                logical_height,
                "Ignoring invalid mouse display bounds"
            );
            return;
        }
        let mut s = self.0.lock().unwrap();
        if (s.origin_x, s.origin_y, s.logical_w, s.logical_h)
            != (origin_x, origin_y, logical_width, logical_height)
        {
            s.origin_x = origin_x;
            s.origin_y = origin_y;
            s.logical_w = logical_width;
            s.logical_h = logical_height;
            tracing::info!(
                origin_x,
                origin_y,
                logical_width,
                logical_height,
                "Mouse coordinate mapper: selected display bounds changed"
            );
        }
    }

    /// Refresh CG bounds when starting a capture stream after a display-mode change.
    pub fn update_logical_size(&self, logical_w: u32, logical_h: u32) {
        if logical_w == 0 || logical_h == 0 {
            return;
        }
        let mut s = self.0.lock().unwrap();
        let width = f64::from(logical_w);
        let height = f64::from(logical_h);
        if !valid_axis(s.origin_x, width) || !valid_axis(s.origin_y, height) {
            return;
        }
        if (s.logical_w, s.logical_h) != (width, height) {
            s.logical_w = width;
            s.logical_h = height;
            tracing::info!(
                logical_w,
                logical_h,
                origin_x = s.origin_x,
                origin_y = s.origin_y,
                "Mouse coordinate mapper: CG display bounds changed"
            );
        }
    }

    /// Commit a negotiated desktop size when its capture stream is activated.
    pub fn update_rdp_size(&self, rdp_w: u16, rdp_h: u16) {
        let mut s = self.0.lock().unwrap();
        let width = f64::from(rdp_w.max(1));
        let height = f64::from(rdp_h.max(1));
        if (s.rdp_w, s.rdp_h) == (width, height) {
            return;
        }
        s.rdp_w = width;
        s.rdp_h = height;
        s.content = ContentRect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
        tracing::info!(
            rdp_w,
            rdp_h,
            logical_w = s.logical_w,
            logical_h = s.logical_h,
            "Mouse coordinate mapper: negotiated desktop activated"
        );
    }

    pub fn update_frame_content_rect(
        &self,
        frame_w: u32,
        frame_h: u32,
        content_rect: Option<&ContentRect>,
    ) {
        if frame_w == 0 || frame_h == 0 {
            return;
        }
        let frame_w = f64::from(frame_w);
        let frame_h = f64::from(frame_h);
        let rect = content_rect.copied().unwrap_or(ContentRect {
            x: 0.0,
            y: 0.0,
            width: frame_w,
            height: frame_h,
        });
        if !rect.is_valid() {
            return;
        }
        // Reject content entirely outside the output, but preserve the full
        // transform when partially clipped. Clipping the rectangle here would
        // incorrectly stretch the visible portion over the whole source display.
        if rect.x >= frame_w
            || rect.y >= frame_h
            || rect.x + rect.width <= 0.0
            || rect.y + rect.height <= 0.0
        {
            return;
        }
        let mut s = self.0.lock().unwrap();
        let content = ContentRect {
            x: rect.x * s.rdp_w / frame_w,
            y: rect.y * s.rdp_h / frame_h,
            width: rect.width * s.rdp_w / frame_w,
            height: rect.height * s.rdp_h / frame_h,
        };
        if !content.is_valid() {
            return;
        }
        if s.content != content {
            s.content = content;
            tracing::info!(
                frame_w,
                frame_h,
                ?rect,
                ?content,
                rdp_w = s.rdp_w,
                rdp_h = s.rdp_h,
                logical_w = s.logical_w,
                logical_h = s.logical_h,
                "Mouse coordinate mapper: visible frame geometry changed"
            );
        }
    }
}

fn valid_axis(origin: f64, extent: f64) -> bool {
    origin.is_finite()
        && extent.is_finite()
        && extent > 0.0
        && (origin + extent).is_finite()
        && origin + extent > origin
}

fn map_axis(input: u16, content_origin: f64, content_extent: f64, origin: f64, extent: f64) -> f64 {
    let fraction = ((f64::from(input) - content_origin) / content_extent).clamp(0.0, 1.0);
    // Display bounds are half-open. Keep clicks in padding on this display, while
    // allowing the last Retina pixel's fractional point instead of rounding it away.
    let upper = origin + extent;
    let last_inside = if upper == 0.0 {
        -f64::from_bits(1)
    } else if upper > 0.0 {
        f64::from_bits(upper.to_bits() - 1)
    } else {
        f64::from_bits(upper.to_bits() + 1)
    };
    (origin + fraction * extent).clamp(origin, last_inside)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_point(actual: (f64, f64), expected: (f64, f64)) {
        assert!(
            (actual.0 - expected.0).abs() < 1e-8,
            "x: {actual:?} != {expected:?}"
        );
        assert!(
            (actual.1 - expected.1).abs() < 1e-8,
            "y: {actual:?} != {expected:?}"
        );
    }

    #[test]
    fn retina_surface_points_are_converted_before_mapping() {
        let mapper = MouseCoordMapper::new(1920, 1200, 3840, 2400);
        let rect = ContentRect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1200.0,
        }
        .from_surface_points(2.0)
        .unwrap();
        mapper.update_frame_content_rect(3840, 2400, Some(&rect));
        assert_point(mapper.map(1920, 1200), (960.0, 600.0));
        assert_point(mapper.map(3839, 2399), (1919.5, 1199.5));
    }

    #[test]
    fn fitted_retina_content_maps_corners_center_and_padding() {
        let mapper = MouseCoordMapper::new(1920, 1200, 2560, 1305);
        // Already fitted by SCK; contentScale=0.54375 must not be applied again.
        let rect = ContentRect {
            x: 118.0,
            y: 0.0,
            width: 1044.0,
            height: 652.5,
        }
        .from_surface_points(2.0)
        .unwrap();
        mapper.update_frame_content_rect(2560, 1305, Some(&rect));
        assert_point(mapper.map(236, 0), (0.0, 0.0));
        assert_point(mapper.map(1280, 652), (960.0, 599.5402298850574));
        assert_point(
            mapper.map(2323, 1304),
            (1919.080459770115, 1199.080459770115),
        );
        assert_point(mapper.map(0, 652), (0.0, 599.5402298850574));
        let padded = mapper.map(2559, 652);
        assert!(padded.0 < 1920.0);
        assert_point(padded, (1920.0, 599.5402298850574));
    }

    #[test]
    fn letterbox_and_noninteger_scale_use_pixel_geometry() {
        let mapper = MouseCoordMapper::new(1920, 1080, 1920, 1200);
        let rect = ContentRect {
            x: 0.0,
            y: 40.0,
            width: 1280.0,
            height: 720.0,
        }
        .from_surface_points(1.5)
        .unwrap();
        mapper.update_frame_content_rect(1920, 1200, Some(&rect));
        assert_point(mapper.map(960, 600), (960.0, 540.0));
        assert_point(mapper.map(0, 60), (0.0, 0.0));
        assert_point(mapper.map(1919, 1139), (1919.0, 1079.0));
    }

    #[test]
    fn resize_and_full_frame_fallback_remove_old_padding() {
        let mapper = MouseCoordMapper::new(1920, 1200, 2560, 1305);
        let rect = ContentRect {
            x: 236.0,
            y: 0.0,
            width: 2088.0,
            height: 1305.0,
        };
        mapper.update_frame_content_rect(2560, 1305, Some(&rect));
        mapper.update_rdp_size(1920, 1200);
        assert_point(mapper.map(960, 600), (960.0, 600.0));
        mapper.update_frame_content_rect(2560, 1305, Some(&rect));
        mapper.update_frame_content_rect(3840, 2400, None);
        assert_point(mapper.map(480, 300), (480.0, 300.0));
    }

    #[test]
    fn invalid_metadata_does_not_corrupt_mapping() {
        let mapper = MouseCoordMapper::new(1920, 1200, 1920, 1200);
        for width in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            let rect = ContentRect {
                x: 0.0,
                y: 0.0,
                width,
                height: 1200.0,
            };
            assert!(rect.from_surface_points(2.0).is_none());
            mapper.update_frame_content_rect(1920, 1200, Some(&rect));
            assert_point(mapper.map(960, 600), (960.0, 600.0));
        }
        let tiny = MouseCoordMapper::new(0, 0, 0, 0).map(10, 10);
        assert!(tiny.0 >= 0.0 && tiny.0 < 1.0 && tiny.1 >= 0.0 && tiny.1 < 1.0);
    }

    #[test]
    fn new_capture_refreshes_host_display_mode() {
        let mapper = MouseCoordMapper::new(1920, 1200, 2560, 1600);
        mapper.update_logical_size(1280, 800);
        assert_point(mapper.map(1280, 800), (640.0, 400.0));
        mapper.update_logical_size(0, 0);
        assert_point(mapper.map(1280, 800), (640.0, 400.0));
    }

    #[test]
    fn frame_pixels_are_scaled_to_the_negotiated_desktop() {
        let mapper = MouseCoordMapper::new(1920, 1200, 1280, 800);
        let rect = ContentRect {
            x: 320.0,
            y: 200.0,
            width: 1920.0,
            height: 1200.0,
        };
        mapper.update_frame_content_rect(2560, 1600, Some(&rect));
        assert_point(mapper.map(160, 100), (0.0, 0.0));
        assert_point(mapper.map(640, 400), (960.0, 600.0));
        assert_point(mapper.map(1119, 699), (1918.0, 1198.0));
    }

    #[test]
    fn clipped_content_keeps_its_original_transform() {
        let mapper = MouseCoordMapper::new(2000, 1200, 1900, 1200);
        let rect = ContentRect {
            x: -100.0,
            y: 0.0,
            width: 2000.0,
            height: 1200.0,
        };
        mapper.update_frame_content_rect(1900, 1200, Some(&rect));
        assert_point(mapper.map(0, 0), (100.0, 0.0));
        let edge = mapper.map(1900, 1200);
        assert!(edge.0 < 2000.0 && edge.1 < 1200.0);
        assert_point(edge, (2000.0, 1200.0));
        let outside = ContentRect { x: 1900.0, ..rect };
        mapper.update_frame_content_rect(1900, 1200, Some(&outside));
        assert_point(mapper.map(0, 0), (100.0, 0.0));
    }

    #[test]
    fn invalid_scale_is_rejected_instead_of_guessing_units() {
        let rect = ContentRect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1200.0,
        };
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(rect.from_surface_points(scale).is_none());
        }
    }

    #[test]
    fn secondary_retina_display_keeps_negative_global_origin_and_subpoints() {
        let mapper = MouseCoordMapper::new(1920, 1200, 3840, 2400);
        mapper.update_display_bounds(-1920.0, -1200.0, 1920.0, 1200.0);
        let content = ContentRect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1200.0,
        }
        .from_surface_points(2.0)
        .unwrap();
        mapper.update_frame_content_rect(3840, 2400, Some(&content));

        assert_point(mapper.map(0, 0), (-1920.0, -1200.0));
        assert_point(mapper.map(1920, 1200), (-960.0, -600.0));
        assert_point(mapper.map(3839, 2399), (-0.5, -0.5));
        let outside = mapper.map(u16::MAX, u16::MAX);
        assert!(outside.0 < 0.0 && outside.1 < 0.0);
    }

    #[test]
    fn secondary_display_letterboxing_clamps_inside_global_bounds() {
        let mapper = MouseCoordMapper::new(1920, 1200, 2560, 1600);
        mapper.update_display_bounds(3840.0, 240.0, 1920.0, 1200.0);
        let content = ContentRect {
            x: 320.0,
            y: 200.0,
            width: 1920.0,
            height: 1200.0,
        };
        mapper.update_frame_content_rect(2560, 1600, Some(&content));

        assert_point(mapper.map(0, 0), (3840.0, 240.0));
        assert_point(mapper.map(1280, 800), (4800.0, 840.0));
        let lower_right = mapper.map(2559, 1599);
        assert!(lower_right.0 < 5760.0 && lower_right.1 < 1440.0);
        assert_point(lower_right, (5760.0, 1440.0));

        // A resize must retain the selected display's position in the global desktop.
        mapper.update_logical_size(1280, 800);
        mapper.update_rdp_size(1280, 800);
        assert_point(mapper.map(640, 400), (4480.0, 640.0));
    }

    #[test]
    fn clipped_secondary_display_keeps_transform_and_negative_upper_edge() {
        let mapper = MouseCoordMapper::new(2000, 1200, 1900, 1200);
        mapper.update_display_bounds(-3000.0, -2000.0, 2000.0, 1200.0);
        let content = ContentRect {
            x: -100.0,
            y: 0.0,
            width: 2000.0,
            height: 1200.0,
        };
        mapper.update_frame_content_rect(1900, 1200, Some(&content));

        assert_point(mapper.map(0, 0), (-2900.0, -2000.0));
        assert_point(mapper.map(900, 600), (-2000.0, -1400.0));
        let edge = mapper.map(u16::MAX, u16::MAX);
        assert!(edge.0 < -1000.0 && edge.1 < -800.0);
        assert_point(edge, (-1000.0, -800.0));
    }

    #[test]
    fn invalid_global_bounds_preserve_the_selected_display() {
        let mapper = MouseCoordMapper::new(1920, 1200, 1920, 1200);
        mapper.update_display_bounds(-1920.0, 240.0, 1920.0, 1200.0);
        for bounds in [
            (f64::NAN, 0.0, 1920.0, 1200.0),
            (0.0, f64::INFINITY, 1920.0, 1200.0),
            (0.0, 0.0, 0.0, 1200.0),
            (0.0, 0.0, 1920.0, -1.0),
            (0.0, 0.0, f64::INFINITY, 1200.0),
            (f64::MAX, 0.0, f64::MAX, 1200.0),
            (f64::MAX, 0.0, 1.0, 1200.0),
        ] {
            mapper.update_display_bounds(bounds.0, bounds.1, bounds.2, bounds.3);
            assert_point(mapper.map(960, 600), (-960.0, 840.0));
        }
    }
}

#[cfg(test)]
mod relative_tests {
    use super::*;

    #[test]
    fn relative_motion_scales_retina_letterboxing_and_clamps_negative_display() {
        let mapper = MouseCoordMapper::new(1000, 600, 2400, 1200);
        mapper.update_display_bounds(-1000.0, -600.0, 1000.0, 600.0);
        mapper.update_frame_content_rect(
            2400,
            1200,
            Some(&ContentRect {
                x: 200.0,
                y: 0.0,
                width: 2000.0,
                height: 1200.0,
            }),
        );
        assert_eq!(
            mapper.map_relative((-500.0, -300.0), 20, -10),
            (-490.0, -305.0)
        );
        assert_eq!(
            mapper.map_relative((-500.0, -300.0), i32::MAX, i32::MIN),
            (-1.0, -600.0)
        );
        assert_eq!(
            mapper.map_relative((f64::NAN, f64::NAN), 20, 10),
            (-990.0, -595.0)
        );
        mapper.update_display_bounds(0.0, 0.0, 1000.0, 600.0);
        assert_eq!(mapper.map_relative((-500.0, -300.0), 0, 0), (0.0, 0.0));
    }
}
