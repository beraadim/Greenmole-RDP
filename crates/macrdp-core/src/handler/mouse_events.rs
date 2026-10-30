use super::MacInputHandler;
use ironrdp_server::MouseEvent;
use macrdp_input::MouseButton;

impl MacInputHandler {
    /// Return whether injection failed; input values remain in this handler.
    pub(super) fn handle_mouse_event(&mut self, event: MouseEvent) -> bool {
        let Some(m) = &mut self.mouse else {
            return false;
        };

        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_input("mouse") {
                tracing::warn!("Failed to declare remote mouse activity: {e}");
            }
        }

        let result = match event {
            MouseEvent::Move { x, y } => {
                let (mx, my) = self.coord_mapper.map(x, y);
                m.move_to(mx, my).map(|()| {
                    self.last_mouse_x = mx;
                    self.last_mouse_y = my;
                })
            }
            MouseEvent::RelMove { x, y } => {
                let (mx, my) =
                    self.coord_mapper
                        .map_relative((self.last_mouse_x, self.last_mouse_y), x, y);
                m.move_to(mx, my).map(|()| {
                    self.last_mouse_x = mx;
                    self.last_mouse_y = my;
                })
            }
            MouseEvent::VerticalScroll { value } => m.scroll(0, i32::from(value)),
            MouseEvent::Scroll { x, y } => m.scroll(x, y),
            MouseEvent::ScrollHighResolution { x, y } => m.scroll_high_resolution(x, y),
            button => {
                let (button, pressed) = match button {
                    MouseEvent::LeftPressed => (MouseButton::Left, true),
                    MouseEvent::LeftReleased => (MouseButton::Left, false),
                    MouseEvent::RightPressed => (MouseButton::Right, true),
                    MouseEvent::RightReleased => (MouseButton::Right, false),
                    MouseEvent::MiddlePressed => (MouseButton::Middle, true),
                    MouseEvent::MiddleReleased => (MouseButton::Middle, false),
                    MouseEvent::Button4Pressed => (MouseButton::Button4, true),
                    MouseEvent::Button4Released => (MouseButton::Button4, false),
                    MouseEvent::Button5Pressed => (MouseButton::Button5, true),
                    MouseEvent::Button5Released => (MouseButton::Button5, false),
                    _ => unreachable!("movement and scroll handled above"),
                };
                m.button_event(button, pressed, self.last_mouse_x, self.last_mouse_y)
            }
        };

        if let Err(e) = result {
            tracing::warn!("Mouse injection failed: {e}");
            true
        } else {
            false
        }
    }
}
