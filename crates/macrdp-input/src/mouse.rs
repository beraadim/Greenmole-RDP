use anyhow::Result;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, EventField};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use foreign_types::ForeignType;
use std::time::{Duration, Instant};

extern "C" {
    fn CGEventCreateMouseEvent(
        source: *mut core_graphics::sys::CGEventSource,
        event_type: CGEventType,
        point: CGPoint,
        button: u32,
    ) -> *mut core_graphics::sys::CGEvent;
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
mod click;
mod scroll;
use click::ClickState;
use scroll::ScrollState;

pub struct MouseInjector {
    state: MouseState,
    scroll: ScrollState,
}

struct MouseState {
    pressed: [bool; 5],
    position: (f64, f64),
    clicks: ClickState,
    double_click_interval: Duration,
}

impl Default for MouseState {
    fn default() -> Self {
        Self {
            pressed: [false; 5],
            position: (0.0, 0.0),
            clicks: ClickState::default(),
            double_click_interval: Duration::from_millis(500),
        }
    }
}

impl MouseInjector {
    pub fn new() -> Result<Self> {
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self {
            state: MouseState::default(),
            scroll: ScrollState::default(),
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
        if pressed {
            // Read on each new press so changes to the host's Accessibility
            // double-click speed take effect without restarting the server.
            self.state.double_click_interval = system_double_click_interval();
        }
        self.state
            .button_event(button, pressed, x, y, Self::post_mouse_event)
    }

    /// Release only buttons held by this remote session, at its last posted position.
    pub fn release_all(&mut self) -> Result<()> {
        self.scroll = ScrollState::default();
        self.state.release_all(Self::post_mouse_event)
    }

    fn post_mouse_event(
        event_type: CGEventType,
        button: u32,
        point: CGPoint,
        click_count: i64,
    ) -> Result<()> {
        let event = Self::create_mouse_event(event_type, button, point, click_count)?;
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    /// Separated from posting so native event fields can be tested without desktop input.
    fn create_mouse_event(
        event_type: CGEventType,
        button: u32,
        point: CGPoint,
        click_count: i64,
    ) -> Result<CGEvent> {
        let source = Self::source()?;
        // The Rust CGMouseButton enum only exposes 0..=2. CoreGraphics accepts
        // other button numbers through its C API; never manufacture invalid enums.
        let event_ref =
            unsafe { CGEventCreateMouseEvent(source.as_ptr(), event_type, point, button) };
        anyhow::ensure!(
            !event_ref.is_null(),
            "Failed to create mouse event: {event_type:?}"
        );
        let event = unsafe { CGEvent::from_ptr(event_ref) };
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_count);
        Ok(event)
    }

    /// Read the host cursor once when establishing a relative-input session.
    pub fn current_position(&self) -> Result<(f64, f64)> {
        let event = CGEvent::new(Self::source()?)
            .map_err(|_| anyhow::anyhow!("Failed to read cursor position"))?;
        let point = event.location();
        Ok((point.x, point.y))
    }

    /// RDP uses 120 rotation units per detent for both Guacamole and mstsc.
    pub fn scroll(&mut self, horizontal: i32, vertical: i32) -> Result<()> {
        self.scroll.scroll(horizontal, vertical, Self::post_scroll)
    }

    /// FreeRDP AINPUT uses signed 16.16 rotation units rather than integer units.
    pub fn scroll_high_resolution(&mut self, horizontal: i32, vertical: i32) -> Result<()> {
        self.scroll.scroll_fixed(
            i64::from(horizontal),
            i64::from(vertical),
            Self::post_scroll,
        )
    }

    fn post_scroll(x: i32, y: i32) -> Result<()> {
        unsafe {
            let event_ref =
                CGEventCreateScrollWheelEvent2(std::ptr::null_mut(), SCROLL_UNIT_LINE, 2, y, x, 0);
            anyhow::ensure!(!event_ref.is_null(), "Failed to create scroll event");
            let event = CGEvent::from_ptr(event_ref);
            event.post(CGEventTapLocation::HID);
        }
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
        mut post: impl FnMut(CGEventType, u32, CGPoint, i64) -> Result<()>,
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
            (CGEventType::LeftMouseDragged, MouseButton::Left as u32)
        } else if self.pressed[MouseButton::Right as usize] {
            (CGEventType::RightMouseDragged, MouseButton::Right as u32)
        } else if let Some(button) = [
            MouseButton::Middle,
            MouseButton::Button4,
            MouseButton::Button5,
        ]
        .into_iter()
        .find(|button| self.pressed[*button as usize])
        {
            (CGEventType::OtherMouseDragged, button as u32)
        } else {
            (CGEventType::MouseMoved, MouseButton::Left as u32)
        };
        post(
            event_type,
            button,
            point,
            self.clicks.count_for_drag(button as usize),
        )?;
        self.clicks.move_to((x, y));
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
        post: impl FnMut(CGEventType, u32, CGPoint, i64) -> Result<()>,
    ) -> Result<()> {
        self.button_event_at(button, pressed, x, y, Instant::now(), post)
    }

    fn button_event_at(
        &mut self,
        button: MouseButton,
        pressed: bool,
        x: f64,
        y: f64,
        now: Instant,
        mut post: impl FnMut(CGEventType, u32, CGPoint, i64) -> Result<()>,
    ) -> Result<()> {
        anyhow::ensure!(
            x.is_finite() && y.is_finite(),
            "Mouse position must be finite"
        );
        if pressed == self.pressed[button as usize] {
            return Ok(());
        }
        let point = CGPoint::new(x, y);
        let (event_type, cg_button) = match (button, pressed) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, MouseButton::Left as u32),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, MouseButton::Left as u32),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, MouseButton::Right as u32),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, MouseButton::Right as u32),
            (other, true) => (CGEventType::OtherMouseDown, other as u32),
            (other, false) => (CGEventType::OtherMouseUp, other as u32),
        };

        let mut clicks = self.clicks;
        let click_count = clicks.transition(
            button as usize,
            pressed,
            (x, y),
            now,
            self.double_click_interval,
        );
        post(event_type, cg_button, point, click_count)?;
        // Failed creation must not lose a held button or start a phantom drag.
        self.pressed[button as usize] = pressed;
        self.position = (x, y);
        self.clicks = clicks;
        tracing::trace!(?button, pressed, x, y, click_count, "Mouse button event");
        Ok(())
    }

    fn release_all(
        &mut self,
        post: impl FnMut(CGEventType, u32, CGPoint, i64) -> Result<()>,
    ) -> Result<()> {
        self.release_all_at(Instant::now(), post)
    }

    fn release_all_at(
        &mut self,
        now: Instant,
        mut post: impl FnMut(CGEventType, u32, CGPoint, i64) -> Result<()>,
    ) -> Result<()> {
        let mut first_error = None;
        let mut released = 0;
        for button in MouseButton::ALL {
            if self.pressed[button as usize] {
                let (x, y) = self.position;
                match self.button_event_at(button, false, x, y, now, &mut post) {
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
        self.clicks.cancel_sequence();
        first_error.map_or(Ok(()), Err)
    }
}

fn system_double_click_interval() -> Duration {
    let seconds = objc2_app_kit::NSEvent::doubleClickInterval();
    Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|interval| !interval.is_zero())
        .unwrap_or_else(|| Duration::from_millis(500))
}

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Button4,
    Button5,
}

impl MouseButton {
    const ALL: [Self; 5] = [
        Self::Left,
        Self::Right,
        Self::Middle,
        Self::Button4,
        Self::Button5,
    ];
}

#[cfg(test)]
mod click_tests;
#[cfg(test)]
mod tests;
