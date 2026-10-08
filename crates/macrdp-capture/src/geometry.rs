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
/// main display's CoreGraphics logical coordinates. Encoder padding is excluded.
#[derive(Debug, Clone)]
pub struct MouseCoordMapper(Arc<Mutex<CoordState>>);

#[derive(Debug)]
struct CoordState {
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

    pub fn map(&self, rdp_x: u16, rdp_y: u16) -> (u16, u16) {
        let s = self.0.lock().unwrap();
        let x = ((f64::from(rdp_x) - s.content.x) * s.logical_w / s.content.width)
            .clamp(0.0, s.logical_w - 1.0) as u16;
        let y = ((f64::from(rdp_y) - s.content.y) * s.logical_h / s.content.height)
            .clamp(0.0, s.logical_h - 1.0) as u16;
        (x, y)
    }

    /// Refresh CG bounds when starting a capture stream after a display-mode change.
    pub fn update_logical_size(&self, logical_w: u32, logical_h: u32) {
        if logical_w == 0 || logical_h == 0 {
            return;
        }
        let mut s = self.0.lock().unwrap();
        let width = f64::from(logical_w);
        let height = f64::from(logical_h);
        if (s.logical_w, s.logical_h) != (width, height) {
            s.logical_w = width;
            s.logical_h = height;
            tracing::info!(
                logical_w,
                logical_h,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(mapper.map(1920, 1200), (960, 600));
        assert_eq!(mapper.map(3839, 2399), (1919, 1199));
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
        assert_eq!(mapper.map(236, 0), (0, 0));
        assert_eq!(mapper.map(1280, 652), (960, 599));
        assert_eq!(mapper.map(2323, 1304), (1919, 1199));
        assert_eq!(mapper.map(0, 652), (0, 599));
        assert_eq!(mapper.map(2559, 652), (1919, 599));
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
        assert_eq!(mapper.map(960, 600), (960, 540));
        assert_eq!(mapper.map(0, 60), (0, 0));
        assert_eq!(mapper.map(1919, 1139), (1919, 1079));
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
        assert_eq!(mapper.map(960, 600), (960, 600));
        mapper.update_frame_content_rect(2560, 1305, Some(&rect));
        mapper.update_frame_content_rect(3840, 2400, None);
        assert_eq!(mapper.map(480, 300), (480, 300));
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
            assert_eq!(mapper.map(960, 600), (960, 600));
        }
        assert_eq!(MouseCoordMapper::new(0, 0, 0, 0).map(10, 10), (0, 0));
    }

    #[test]
    fn new_capture_refreshes_host_display_mode() {
        let mapper = MouseCoordMapper::new(1920, 1200, 2560, 1600);
        mapper.update_logical_size(1280, 800);
        assert_eq!(mapper.map(1280, 800), (640, 400));
        mapper.update_logical_size(0, 0);
        assert_eq!(mapper.map(1280, 800), (640, 400));
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
        assert_eq!(mapper.map(160, 100), (0, 0));
        assert_eq!(mapper.map(640, 400), (960, 600));
        assert_eq!(mapper.map(1119, 699), (1918, 1198));
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
        assert_eq!(mapper.map(0, 0), (100, 0));
        assert_eq!(mapper.map(1900, 1200), (1999, 1199));
        let outside = ContentRect { x: 1900.0, ..rect };
        mapper.update_frame_content_rect(1900, 1200, Some(&outside));
        assert_eq!(mapper.map(0, 0), (100, 0));
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
}
