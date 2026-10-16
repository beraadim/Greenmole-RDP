use crate::DisplayGeometry;

/// All coordinates are global, top-left-origin logical screen points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Frame {
    /// A usable titlebar lets the user move the window themselves. Preserve
    /// those windows on any surviving local display, including windows spanning
    /// monitors; a tiny offscreen sliver is not enough for practical recovery.
    pub fn has_accessible_titlebar(self, screens: &[Self]) -> bool {
        let titlebar_height = self.height.min(28.0);
        let minimum_width = self.width.min(80.0);
        let minimum_height = titlebar_height.min(16.0);
        screens.iter().any(|screen| {
            let width = (self.x + self.width).min(screen.x + screen.width) - self.x.max(screen.x);
            let height =
                (self.y + titlebar_height).min(screen.y + screen.height) - self.y.max(screen.y);
            width >= minimum_width && height >= minimum_height
        })
    }

    pub fn fits_inside(self, bounds: Self) -> bool {
        self.x >= bounds.x
            && self.y >= bounds.y
            && self.x + self.width <= bounds.x + bounds.width
            && self.y + self.height <= bounds.y + bounds.height
    }

    /// Preserve windows already fully visible. Relocate others from the display
    /// containing most of their area, leaving room for the primary menu bar.
    pub fn on_remote_display(self, target: Self, screens: &[Self], resizable: bool) -> Self {
        if self.fits_inside(target) {
            return self;
        }
        let source = screens
            .iter()
            .copied()
            .max_by(|left, right| {
                self.intersection(*left)
                    .total_cmp(&self.intersection(*right))
            })
            .filter(|screen| self.intersection(*screen) > 0.0)
            .unwrap_or(self);
        let inset = Self {
            x: target.x + 8.0,
            y: target.y + 32.0,
            width: (target.width - 16.0).max(1.0),
            height: (target.height - 40.0).max(1.0),
        };
        self.relocate(source, inset, resizable)
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
    fn manual_recovery_keeps_visible_windows_and_fits_stranded_windows() {
        let physical = rect(-1920.0, 100.0, 1920.0, 1080.0);
        let target = rect(0.0, 0.0, 1280.0, 720.0);
        let screens = [physical, target];
        let visible = rect(50.0, 40.0, 800.0, 600.0);
        assert_eq!(visible.on_remote_display(target, &screens, true), visible);
        for stranded in [
            rect(-1850.0, 140.0, 1500.0, 900.0),
            rect(-10000.0, 4000.0, 800.0, 600.0),
            rect(-200.0, -20.0, 600.0, 400.0),
        ] {
            let moved = stranded.on_remote_display(target, &screens, true);
            assert!(moved.fits_inside(target));
            assert!(moved.y >= 32.0);
        }
    }

    #[test]
    fn manual_recovery_does_not_resize_fixed_windows_or_use_pixel_dimensions() {
        let display = DisplayGeometry {
            display_id: 23,
            origin_x: -1280.0,
            origin_y: -500.0,
            logical_width: 1280.0,
            logical_height: 720.0,
            pixel_width: 2560,
            pixel_height: 1440,
        };
        let target = Frame::from(display);
        let moved = rect(100.0, 100.0, 1800.0, 1000.0).on_remote_display(target, &[], false);
        assert_eq!(moved, rect(-1272.0, -468.0, 1800.0, 1000.0));
        assert!(moved.close_to(moved.on_remote_display(target, &[target], false)));
    }

    #[test]
    fn readback_accepts_only_small_coordinate_rounding() {
        let expected = rect(-1920.0, 32.0, 800.0, 600.0);
        assert!(expected.close_to(rect(-1919.5, 32.5, 800.0, 600.0)));
        assert!(!expected.close_to(rect(-1900.0, 32.0, 800.0, 600.0)));
        assert!(!expected.close_to(rect(-1920.0, 32.0, 1000.0, 600.0)));
    }

    #[test]
    fn local_recovery_preserves_windows_on_secondary_and_spanning_displays() {
        let screens = [
            rect(-1920.0, 100.0, 1920.0, 1080.0),
            rect(0.0, 0.0, 1920.0, 1080.0),
        ];
        for frame in [
            rect(-1800.0, 150.0, 900.0, 600.0),
            rect(-200.0, 150.0, 800.0, 600.0),
            rect(300.0, 100.0, 800.0, 600.0),
        ] {
            assert!(frame.has_accessible_titlebar(&screens));
        }
    }

    #[test]
    fn local_recovery_identifies_stranded_titlebars_and_unusable_slivers() {
        let screens = [rect(0.0, 0.0, 1920.0, 1080.0)];
        for frame in [
            rect(-10000.0, 4000.0, 800.0, 600.0),
            rect(300.0, -200.0, 800.0, 600.0),
            rect(1910.0, 100.0, 800.0, 600.0),
            rect(300.0, 1070.0, 800.0, 600.0),
        ] {
            assert!(!frame.has_accessible_titlebar(&screens));
        }
        assert!(rect(500.0, 100.0, 40.0, 20.0).has_accessible_titlebar(&screens));
    }
}
