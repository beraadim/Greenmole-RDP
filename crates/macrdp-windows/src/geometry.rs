use macrdp_display::DisplayGeometry;

/// All coordinates are global, top-left-origin logical screen points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Frame {
    pub fn fits_inside(self, bounds: Self) -> bool {
        self.x >= bounds.x
            && self.y >= bounds.y
            && self.x + self.width <= bounds.x + bounds.width
            && self.y + self.height <= bounds.y + bounds.height
    }

    pub fn after_resize(self, previous: Self, target: Self, resizable: bool) -> Self {
        // macOS may already have moved windows when a display origin changes.
        // Keep a valid current arrangement instead of translating it twice.
        if self.fits_inside(target) {
            self
        } else {
            self.relocate(previous, target, resizable)
        }
    }
    pub fn close_to(self, other: Self) -> bool {
        [
            (self.x, other.x),
            (self.y, other.y),
            (self.width, other.width),
            (self.height, other.height),
        ]
        .iter()
        .all(|(a, b)| (a - b).abs() <= 2.0)
    }
    pub fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.0
            && self.height > 0.0
            && (self.x + self.width).is_finite()
            && (self.y + self.height).is_finite()
    }

    pub fn intersection(self, other: Self) -> f64 {
        (self.x + self.width)
            .min(other.x + other.width)
            .sub(self.x.max(other.x))
            .max(0.0)
            * (self.y + self.height)
                .min(other.y + other.height)
                .sub(self.y.max(other.y))
                .max(0.0)
    }

    /// Preserve window size where possible and its relative free-space position.
    /// Oversized windows shrink to fit; negative display origins remain valid.
    pub fn relocate(self, from: Self, to: Self, resizable: bool) -> Self {
        let width = if resizable {
            self.width.min(to.width)
        } else {
            self.width
        };
        let height = if resizable {
            self.height.min(to.height)
        } else {
            self.height
        };
        let axis = |position: f64,
                    origin: f64,
                    extent: f64,
                    size: f64,
                    new_origin: f64,
                    new_extent: f64,
                    new_size: f64| {
            let free = (extent - size).max(0.0);
            let fraction = if free > 0.0 {
                ((position - origin) / free).clamp(0.0, 1.0)
            } else {
                0.0
            };
            new_origin + fraction * (new_extent - new_size).max(0.0)
        };
        Self {
            x: axis(
                self.x, from.x, from.width, self.width, to.x, to.width, width,
            ),
            y: axis(
                self.y,
                from.y,
                from.height,
                self.height,
                to.y,
                to.height,
                height,
            ),
            width,
            height,
        }
    }
}

use std::ops::Sub;

impl From<DisplayGeometry> for Frame {
    fn from(value: DisplayGeometry) -> Self {
        Self {
            x: value.origin_x,
            y: value.origin_y,
            width: value.logical_width,
            height: value.logical_height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f64, y: f64, width: f64, height: f64) -> Frame {
        Frame {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn handles_negative_origins_and_preserves_relative_placement() {
        let window = rect(-1400.0, -200.0, 800.0, 600.0);
        let moved = window.relocate(
            rect(-1600.0, -300.0, 1600.0, 1000.0),
            rect(2560.0, 0.0, 1280.0, 720.0),
            true,
        );
        assert_eq!(moved, rect(2680.0, 30.0, 800.0, 600.0));
    }

    #[test]
    fn clamps_outside_windows_and_shrinks_oversized_windows() {
        let target = rect(2560.0, 0.0, 640.0, 480.0);
        assert_eq!(
            rect(-900.0, -500.0, 1920.0, 1080.0).relocate(
                rect(0.0, 0.0, 1920.0, 1080.0),
                target,
                true
            ),
            target
        );
    }

    #[test]
    fn fixed_size_windows_keep_size_and_titlebar_on_target() {
        assert_eq!(
            rect(20.0, 30.0, 900.0, 700.0).relocate(
                rect(0.0, 0.0, 1920.0, 1080.0),
                rect(-640.0, 100.0, 640.0, 480.0),
                false
            ),
            rect(-640.0, 100.0, 900.0, 700.0)
        );
    }

    #[test]
    fn validates_finite_positive_geometry_and_overflow() {
        assert!(rect(-1.5, 2.5, 100.0, 100.0).valid());
        for invalid in [
            rect(f64::NAN, 0.0, 100.0, 100.0),
            rect(0.0, 0.0, 0.0, 100.0),
            rect(f64::MAX, 0.0, f64::MAX, 100.0),
        ] {
            assert!(!invalid.valid());
        }
    }

    #[test]
    fn greatest_intersection_selects_real_source_display() {
        let window = rect(1800.0, 100.0, 800.0, 600.0);
        assert!(
            window.intersection(rect(1920.0, 0.0, 1920.0, 1080.0))
                > window.intersection(rect(0.0, 0.0, 1920.0, 1080.0))
        );
    }

    #[test]
    fn resize_preserves_a_frame_already_placed_by_macos() {
        let previous = rect(-1920.0, 0.0, 1920.0, 1080.0);
        let target = rect(-1280.0, 0.0, 1280.0, 720.0);
        let translated = rect(-1200.0, 30.0, 800.0, 600.0);
        assert_eq!(translated.after_resize(previous, target, true), translated);
        let outside = rect(-1850.0, 40.0, 1500.0, 900.0);
        assert!(outside
            .after_resize(previous, target, true)
            .fits_inside(target));
    }

    #[test]
    fn readback_accepts_only_small_coordinate_rounding() {
        let expected = rect(-1920.0, 32.0, 800.0, 600.0);
        assert!(expected.close_to(rect(-1919.5, 32.5, 800.0, 600.0)));
        assert!(!expected.close_to(rect(-1900.0, 32.0, 800.0, 600.0)));
        assert!(!expected.close_to(rect(-1920.0, 32.0, 1000.0, 600.0)));
    }
}
