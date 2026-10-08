use ironrdp_server::{ClientInputContext, KeyboardEvent, MouseEvent, RdpServerInputHandler};
pub use macrdp_capture::MouseCoordMapper;
use macrdp_input::{KeyboardInjector, MouseButton, MouseInjector, RemoteSessionWake};

/// Bridges RDP input events to macOS CGEvent injection
pub struct MacInputHandler {
    keyboard: Option<KeyboardInjector>,
    mouse: Option<MouseInjector>,
    wake: Option<RemoteSessionWake>,
    last_mouse_x: f64,
    last_mouse_y: f64,
    coord_mapper: MouseCoordMapper,
    display_controller: Option<macrdp_display::DisplayController>,
}

impl MacInputHandler {
    pub fn new(
        coord_mapper: MouseCoordMapper,
        display_controller: Option<macrdp_display::DisplayController>,
    ) -> Self {
        let keyboard = KeyboardInjector::new()
            .map_err(|e| tracing::error!("Failed to create keyboard injector: {e}"))
            .ok();
        let mouse = MouseInjector::new()
            .map_err(|e| tracing::error!("Failed to create mouse injector: {e}"))
            .ok();

        if keyboard.is_none() || mouse.is_none() {
            tracing::warn!("Input injection may fail — ensure Accessibility permission is granted");
        }

        let wake = RemoteSessionWake::new()
            .map_err(|e| tracing::warn!("Failed to initialize remote wake support: {e}"))
            .ok();

        Self {
            keyboard,
            mouse,
            wake,
            last_mouse_x: 0.0,
            last_mouse_y: 0.0,
            coord_mapper,
            display_controller,
        }
    }
}

impl RdpServerInputHandler for MacInputHandler {
    fn on_connect(&mut self, _context: ClientInputContext, _peer_addr: Option<String>) {
        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_session_start() {
                tracing::warn!("Failed to wake display for new remote session: {e}");
            }
        }
    }

    fn on_disconnect(&mut self) {
        if let Some(display) = &self.display_controller {
            // This is queued before any following connection's activation.
            if let Err(error) = display.end_remote_session_async() {
                tracing::warn!(%error, "Failed to schedule physical display restoration");
            }
        }
        if let Some(wake) = &mut self.wake {
            wake.deactivate_display_sleep_assertion();
        }
    }

    fn keyboard(&mut self, event: KeyboardEvent) {
        let Some(kb) = &self.keyboard else { return };

        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_input("keyboard") {
                tracing::warn!("Failed to declare remote keyboard activity: {e}");
            }
        }

        let result = match event {
            KeyboardEvent::Pressed { code, extended } => kb.inject_key(code, extended, true),
            KeyboardEvent::Released { code, extended } => kb.inject_key(code, extended, false),
            KeyboardEvent::UnicodePressed(ch) => kb.inject_unicode(ch, true),
            KeyboardEvent::UnicodeReleased(ch) => kb.inject_unicode(ch, false),
            KeyboardEvent::Synchronize(_flags) => {
                tracing::debug!("Keyboard synchronize event (ignored)");
                Ok(())
            }
        };

        if let Err(e) = result {
            tracing::warn!("Keyboard injection failed: {e}");
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(m) = &self.mouse else { return };

        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_input("mouse") {
                tracing::warn!("Failed to declare remote mouse activity: {e}");
            }
        }

        let result = match event {
            MouseEvent::Move { x, y } => {
                let (mx, my) = self.coord_mapper.map(x, y);
                self.last_mouse_x = mx;
                self.last_mouse_y = my;
                m.move_to(mx, my)
            }
            MouseEvent::LeftPressed => m.button_event(
                MouseButton::Left,
                true,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::LeftReleased => m.button_event(
                MouseButton::Left,
                false,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::RightPressed => m.button_event(
                MouseButton::Right,
                true,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::RightReleased => m.button_event(
                MouseButton::Right,
                false,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::MiddlePressed => m.button_event(
                MouseButton::Middle,
                true,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::MiddleReleased => m.button_event(
                MouseButton::Middle,
                false,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::VerticalScroll { value } => m.scroll(value),
            _ => {
                tracing::trace!(?event, "Unhandled mouse event");
                Ok(())
            }
        };

        if let Err(e) = result {
            tracing::warn!("Mouse injection failed: {e}");
        }
    }
}

impl Drop for MacInputHandler {
    fn drop(&mut self) {
        if let Some(kb) = &self.keyboard {
            if let Err(e) = kb.release_transient_modifiers() {
                tracing::warn!("Failed to release transient modifiers on drop: {e}");
            }
        }
    }
}
