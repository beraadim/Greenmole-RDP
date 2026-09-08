use ironrdp_server::{ClientInputContext, KeyboardEvent, MouseEvent, RdpServerInputHandler};
use macrdp_capture::ContentRect;
use macrdp_input::{KeyboardInjector, MouseButton, MouseInjector, RemoteSessionWake};
use std::sync::{Arc, Mutex};

/// Maps RDP desktop coordinates to macOS logical coordinates.
///
/// Completely independent of capture/encode resolution — only cares about
/// the RDP desktop size (what the client thinks) and the macOS logical
/// display size (what CGEvent expects).
///
/// Formula: `mac = rdp × logical ÷ rdp_desktop`
#[derive(Debug, Clone)]
pub struct MouseCoordMapper(Arc<Mutex<CoordState>>);

#[derive(Debug)]
struct CoordState {
    logical_w: f64,
    logical_h: f64,
    rdp_w: f64,
    rdp_h: f64,
    content_x: f64,
    content_y: f64,
    content_w: f64,
    content_h: f64,
}

impl MouseCoordMapper {
    pub fn new(logical_w: u16, logical_h: u16, rdp_w: u16, rdp_h: u16) -> Self {
        tracing::info!(
            logical_w,
            logical_h,
            rdp_w,
            rdp_h,
            "MouseCoordMapper initialized"
        );
        Self(Arc::new(Mutex::new(CoordState {
            logical_w: logical_w as f64,
            logical_h: logical_h as f64,
            rdp_w: rdp_w.max(1) as f64,
            rdp_h: rdp_h.max(1) as f64,
            content_x: 0.0,
            content_y: 0.0,
            content_w: rdp_w.max(1) as f64,
            content_h: rdp_h.max(1) as f64,
        })))
    }

    pub fn map(&self, rdp_x: u16, rdp_y: u16) -> (u16, u16) {
        let s = self.0.lock().unwrap();
        let mac_x = ((rdp_x as f64 - s.content_x) * s.logical_w / s.content_w)
            .clamp(0.0, s.logical_w - 1.0) as u16;
        let mac_y = ((rdp_y as f64 - s.content_y) * s.logical_h / s.content_h)
            .clamp(0.0, s.logical_h - 1.0) as u16;
        (mac_x, mac_y)
    }

    pub fn update_rdp_size(&self, rdp_w: u16, rdp_h: u16) {
        let mut s = self.0.lock().unwrap();
        let old_w = s.rdp_w;
        let old_h = s.rdp_h;
        s.rdp_w = rdp_w.max(1) as f64;
        s.rdp_h = rdp_h.max(1) as f64;
        s.content_x = 0.0;
        s.content_y = 0.0;
        s.content_w = s.rdp_w;
        s.content_h = s.rdp_h;
        tracing::info!(
            old_rdp_w = old_w,
            old_rdp_h = old_h,
            new_rdp_w = rdp_w,
            new_rdp_h = rdp_h,
            logical_w = s.logical_w,
            logical_h = s.logical_h,
            "MouseCoordMapper: RDP desktop size updated"
        );
    }

    pub fn update_frame_content_rect(
        &self,
        frame_w: u32,
        frame_h: u32,
        content_rect: Option<&ContentRect>,
    ) {
        let mut s = self.0.lock().unwrap();
        let full_frame = ContentRect {
            x: 0.0,
            y: 0.0,
            width: frame_w.max(1) as f64,
            height: frame_h.max(1) as f64,
        };
        let rect = content_rect.unwrap_or(&full_frame);
        if frame_w == 0
            || frame_h == 0
            || !rect.x.is_finite()
            || !rect.y.is_finite()
            || !rect.width.is_finite()
            || !rect.height.is_finite()
            || rect.width <= 0.0
            || rect.height <= 0.0
        {
            return;
        }

        let frame_w = frame_w as f64;
        let frame_h = frame_h as f64;
        let x = rect.x.clamp(0.0, frame_w);
        let y = rect.y.clamp(0.0, frame_h);
        let width = rect.width.min(frame_w - x);
        let height = rect.height.min(frame_h - y);
        if width <= 0.0 || height <= 0.0 {
            return;
        }

        s.content_x = x * s.rdp_w / frame_w;
        s.content_y = y * s.rdp_h / frame_h;
        s.content_w = width * s.rdp_w / frame_w;
        s.content_h = height * s.rdp_h / frame_h;
    }
}

/// Bridges RDP input events to macOS CGEvent injection
pub struct MacInputHandler {
    keyboard: Option<KeyboardInjector>,
    mouse: Option<MouseInjector>,
    wake: Option<RemoteSessionWake>,
    last_mouse_x: u16,
    last_mouse_y: u16,
    coord_mapper: MouseCoordMapper,
}

impl MacInputHandler {
    pub fn new(coord_mapper: MouseCoordMapper) -> Self {
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
            last_mouse_x: 0,
            last_mouse_y: 0,
            coord_mapper,
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
