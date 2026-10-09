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

pub struct MouseInjector {
    state: MouseState,
}

#[derive(Default)]
struct MouseState {
    pressed: [bool; 3],
    position: (f64, f64),
}

impl MouseInjector {
    pub fn new() -> Result<Self> {
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self {
            state: MouseState::default(),
        })
    }

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))
    }

    /// Move in global CoreGraphics points, including negative secondary-display origins.
    pub fn move_to(&mut self, x: f64, y: f64) -> Result<()> {
        self.state.move_to(x, y, Self::post_mouse_event)
    }

    pub fn button_event(
        &mut self,
        button: MouseButton,
        pressed: bool,
        x: f64,
        y: f64,
    ) -> Result<()> {
        self.state
            .button_event(button, pressed, x, y, Self::post_mouse_event)
    }

    /// Release only buttons held by this remote session, at its last posted position.
    pub fn release_all(&mut self) -> Result<()> {
        self.state.release_all(Self::post_mouse_event)
    }

    fn post_mouse_event(
        event_type: CGEventType,
        button: CGMouseButton,
        point: CGPoint,
    ) -> Result<()> {
        let source = Self::source()?;
        let event = CGEvent::new_mouse_event(source, event_type, point, button)
            .map_err(|_| anyhow::anyhow!("Failed to create mouse event: {event_type:?}"))?;
        event.post(CGEventTapLocation::HID);
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

impl Drop for MouseInjector {
    fn drop(&mut self) {
        if let Err(error) = self.release_all() {
            tracing::warn!(%error, "Failed to release mouse buttons on drop");
        }
    }
}

impl MouseState {
    fn move_to(
        &mut self,
        x: f64,
        y: f64,
        mut post: impl FnMut(CGEventType, CGMouseButton, CGPoint) -> Result<()>,
    ) -> Result<()> {
        anyhow::ensure!(
            x.is_finite() && y.is_finite(),
            "Mouse position must be finite"
        );
        // RDP button packets also deliver their position before the transition.
        // A stationary release must not turn an ordinary click into a drag.
        if self.position == (x, y) && self.pressed.iter().any(|pressed| *pressed) {
            return Ok(());
        }
        let point = CGPoint::new(x, y);
        // macOS does not turn MouseMoved into a drag when a button is held.
        // Track remote buttons independently of the local hardware mouse. Prefer
        // the left button for chords so another held button cannot stop selection.
        let (event_type, button) = if self.pressed[MouseButton::Left as usize] {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if self.pressed[MouseButton::Right as usize] {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if self.pressed[MouseButton::Middle as usize] {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        post(event_type, button, point)?;
        self.position = (x, y);
        tracing::trace!(x, y, ?event_type, "Mouse moved");
        Ok(())
    }

    fn button_event(
        &mut self,
        button: MouseButton,
        pressed: bool,
        x: f64,
        y: f64,
        mut post: impl FnMut(CGEventType, CGMouseButton, CGPoint) -> Result<()>,
    ) -> Result<()> {
        anyhow::ensure!(
            x.is_finite() && y.is_finite(),
            "Mouse position must be finite"
        );
        let point = CGPoint::new(x, y);
        let (event_type, cg_button) = match (button, pressed) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };

        post(event_type, cg_button, point)?;
        // Failed creation must not lose a held button or start a phantom drag.
        self.pressed[button as usize] = pressed;
        self.position = (x, y);
        tracing::trace!(?button, pressed, x, y, "Mouse button event");
        Ok(())
    }

    fn release_all(
        &mut self,
        mut post: impl FnMut(CGEventType, CGMouseButton, CGPoint) -> Result<()>,
    ) -> Result<()> {
        let mut first_error = None;
        let mut released = 0;
        for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
            if self.pressed[button as usize] {
                let (x, y) = self.position;
                match self.button_event(button, false, x, y, &mut post) {
                    Ok(()) => released += 1,
                    Err(error) => {
                        // Keep failed buttons held for a later cleanup attempt,
                        // but do not let one failure prevent the other releases.
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        if released > 0 {
            tracing::debug!(released, "Released held remote mouse buttons");
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[cfg(test)]
mod tests;
