use anyhow::Result;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use foreign_types::ForeignType;

extern "C" {
    fn CGEventCreateScrollWheelEvent2(
        source: *mut core_graphics::sys::CGEventSource,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> *mut core_graphics::sys::CGEvent;
}

/// ScrollEventUnit::Line
const SCROLL_UNIT_LINE: u32 = 1;
const WINDOWS_WHEEL_DELTA: i32 = 120;

fn normalize_scroll_lines(vertical: i16) -> i32 {
    let raw = i32::from(vertical);
    if raw == 0 {
        return 0;
    }

    let magnitude = raw.abs();
    if magnitude >= WINDOWS_WHEEL_DELTA {
        let detents = (magnitude + WINDOWS_WHEEL_DELTA - 1) / WINDOWS_WHEEL_DELTA;
        raw.signum() * detents
    } else {
        raw
    }
}

pub struct MouseInjector;

impl MouseInjector {
    pub fn new() -> Result<Self> {
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self)
    }

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))
    }

    pub fn move_to(&self, x: u16, y: u16) -> Result<()> {
        let point = CGPoint::new(x as f64, y as f64);
        let source = Self::source()?;
        let event =
            CGEvent::new_mouse_event(source, CGEventType::MouseMoved, point, CGMouseButton::Left)
                .map_err(|_| anyhow::anyhow!("Failed to create mouse move event"))?;

        event.post(CGEventTapLocation::HID);
        tracing::trace!(x, y, "Mouse moved");
        Ok(())
    }

    pub fn button_event(&self, button: MouseButton, pressed: bool, x: u16, y: u16) -> Result<()> {
        let point = CGPoint::new(x as f64, y as f64);
        let (event_type, cg_button) = match (button, pressed) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };

        let source = Self::source()?;
        let event = CGEvent::new_mouse_event(source, event_type, point, cg_button)
            .map_err(|_| anyhow::anyhow!("Failed to create mouse button event"))?;

        event.post(CGEventTapLocation::HID);
        tracing::trace!(?button, pressed, x, y, "Mouse button event");
        Ok(())
    }

    pub fn scroll(&self, vertical: i16) -> Result<()> {
        let normalized = normalize_scroll_lines(vertical);
        if normalized == 0 {
            return Ok(());
        }

        unsafe {
            let event_ref = CGEventCreateScrollWheelEvent2(
                std::ptr::null_mut(),
                SCROLL_UNIT_LINE,
                1,
                normalized,
                0,
                0,
            );
            if event_ref.is_null() {
                return Err(anyhow::anyhow!("Failed to create scroll event"));
            }
            let event = CGEvent::from_ptr(event_ref);
            event.post(CGEventTapLocation::HID);
        }
        tracing::trace!(vertical, normalized, "Mouse scroll");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[cfg(test)]
mod tests {
    use super::normalize_scroll_lines;

    #[test]
    fn keeps_small_scroll_values() {
        assert_eq!(normalize_scroll_lines(1), 1);
        assert_eq!(normalize_scroll_lines(-3), -3);
    }

    #[test]
    fn converts_windows_wheel_detents_to_lines() {
        assert_eq!(normalize_scroll_lines(120), 1);
        assert_eq!(normalize_scroll_lines(-120), -1);
        assert_eq!(normalize_scroll_lines(240), 2);
        assert_eq!(normalize_scroll_lines(-241), -3);
    }
}
