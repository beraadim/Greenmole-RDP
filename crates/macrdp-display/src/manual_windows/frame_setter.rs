//! Window servers can clamp a resize to the monitor currently containing the
//! window. Moving first and retrying a grow is necessary to restore a larger
//! original frame after using a smaller remote display.
use super::geometry::Frame;

pub(super) trait Backend {
    type Error;
    fn size(&mut self, frame: Frame) -> Result<(), Self::Error>;
    fn position(&mut self, frame: Frame) -> Result<(), Self::Error>;
    fn frame(&mut self) -> Result<Frame, Self::Error>;
}

pub(super) fn set_frame<B: Backend>(
    backend: &mut B,
    desired: Frame,
    resizable: bool,
    mismatch: B::Error,
) -> Result<(), B::Error> {
    // Shrinking first lets oversized windows fit on the destination. Size
    // rejection must not prevent trying the position on a reachable monitor.
    if resizable {
        let _ = backend.size(desired);
    }
    let position = backend.position(desired);
    if backend.frame()?.close_to(desired) {
        return Ok(());
    }
    if !resizable {
        position?;
        return Err(mismatch);
    }
    // The first size may have been clamped by the source monitor. Retry on the
    // destination and correct app-driven position changes caused by resizing.
    // Each native implementation checks the shared deadline before every call.
    let _ = backend.size(desired);
    let position = backend.position(desired);
    if backend.frame()?.close_to(desired) {
        return Ok(());
    }
    position?;
    Err(mismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MonitorClampingApp {
        actual: Frame,
        writes: usize,
        size_writes: usize,
    }
    impl Backend for MonitorClampingApp {
        type Error = &'static str;
        fn size(&mut self, desired: Frame) -> Result<(), Self::Error> {
            self.writes += 1;
            self.size_writes += 1;
            let (width, height) = if self.actual.x < 0.0 {
                (1280.0_f64, 720.0_f64)
            } else {
                (2560.0_f64, 1600.0_f64)
            };
            self.actual.width = desired.width.min(width);
            self.actual.height = desired.height.min(height);
            Ok(())
        }
        fn position(&mut self, desired: Frame) -> Result<(), Self::Error> {
            self.writes += 1;
            self.actual.x = desired.x;
            self.actual.y = desired.y;
            Ok(())
        }
        fn frame(&mut self) -> Result<Frame, Self::Error> {
            Ok(self.actual)
        }
    }
    fn frame(x: f64, width: f64, height: f64) -> Frame {
        Frame {
            x,
            y: 40.0,
            width,
            height,
        }
    }

    #[test]
    fn restores_large_original_after_app_clamps_growth_on_smaller_remote_monitor() {
        let desired = frame(100.0, 2200.0, 1300.0);
        let start = frame(-1280.0, 1100.0, 650.0);
        // The previous size-then-position approach cannot restore the large
        // frame: this models an application constraining size to its screen.
        let mut old = MonitorClampingApp {
            actual: start,
            writes: 0,
            size_writes: 0,
        };
        old.size(desired).unwrap();
        old.position(desired).unwrap();
        assert!(!old.frame().unwrap().close_to(desired));

        let mut app = MonitorClampingApp {
            actual: start,
            writes: 0,
            size_writes: 0,
        };
        set_frame(&mut app, desired, true, "frame constrained").unwrap();
        assert_eq!(app.actual, desired);
        assert_eq!(app.size_writes, 2);
    }

    #[test]
    fn shrinking_to_remote_needs_no_retry_and_fixed_size_windows_never_resize() {
        let desired = frame(-1200.0, 900.0, 600.0);
        let mut app = MonitorClampingApp {
            actual: frame(100.0, 2200.0, 1300.0),
            writes: 0,
            size_writes: 0,
        };
        set_frame(&mut app, desired, true, "frame constrained").unwrap();
        assert_eq!(app.actual, desired);
        assert_eq!(app.size_writes, 1);
        let mut fixed = MonitorClampingApp {
            actual: frame(-1200.0, 900.0, 600.0),
            writes: 0,
            size_writes: 0,
        };
        let destination = frame(100.0, 900.0, 600.0);
        set_frame(&mut fixed, destination, false, "frame constrained").unwrap();
        assert_eq!(fixed.actual, destination);
        assert_eq!(fixed.size_writes, 0);
    }

    #[test]
    fn setter_stops_when_shared_deadline_backend_expires_and_propagates_error() {
        struct Expired;
        impl Backend for Expired {
            type Error = &'static str;
            fn size(&mut self, _: Frame) -> Result<(), Self::Error> {
                Err("expired")
            }
            fn position(&mut self, _: Frame) -> Result<(), Self::Error> {
                Err("expired")
            }
            fn frame(&mut self) -> Result<Frame, Self::Error> {
                Err("expired")
            }
        }
        assert_eq!(
            set_frame(&mut Expired, frame(100.0, 800.0, 600.0), true, "mismatch"),
            Err("expired")
        );
    }
}
