use ironrdp_server::{ClientInputContext, KeyboardEvent, MouseEvent, RdpServerInputHandler};
use macrdp_input::{KeyboardInjector, MouseButton, MouseInjector};
use std::sync::{Arc, Mutex};

/// Maps RDP desktop coordinates to macOS logical coordinates.
///
/// Completely independent of capture/encode resolution — only cares about
/// the RDP desktop size (what the client thinks) and the macOS CG logical
/// display size (what CGEvent expects).
///
/// Formula: `mac = rdp × cg_logical ÷ rdp_desktop`
#[derive(Debug, Clone)]
pub struct MouseCoordMapper(Arc<Mutex<CoordState>>);

#[derive(Debug)]
struct CoordState {
    logical_w: f64,
    logical_h: f64,
    rdp_w: f64,
    rdp_h: f64,
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
        })))
    }

    pub fn map(&self, rdp_x: u16, rdp_y: u16) -> (u16, u16) {
        let s = self.0.lock().unwrap();
        // ScreenCaptureKit capture uses aspect-fit scaling when the client asks
        // for a different desktop shape. That can introduce horizontal or
        // vertical bars inside the negotiated RDP desktop, so map against the
        // active image rect rather than the full RDP surface.
        let scale = (s.rdp_w / s.logical_w).min(s.rdp_h / s.logical_h);
        let active_w = (s.logical_w * scale).max(1.0);
        let active_h = (s.logical_h * scale).max(1.0);
        let offset_x = ((s.rdp_w - active_w) * 0.5).max(0.0);
        let offset_y = ((s.rdp_h - active_h) * 0.5).max(0.0);

        let rel_x = ((rdp_x as f64) - offset_x).clamp(0.0, active_w);
        let rel_y = ((rdp_y as f64) - offset_y).clamp(0.0, active_h);

        let mac_x = (rel_x * s.logical_w / active_w).clamp(0.0, s.logical_w - 1.0) as u16;
        let mac_y = (rel_y * s.logical_h / active_h).clamp(0.0, s.logical_h - 1.0) as u16;
        (mac_x, mac_y)
    }

    pub fn update_rdp_size(&self, rdp_w: u16, rdp_h: u16) {
        let mut s = self.0.lock().unwrap();
        s.rdp_w = rdp_w.max(1) as f64;
        s.rdp_h = rdp_h.max(1) as f64;
        tracing::info!(
            new_rdp_w = rdp_w,
            new_rdp_h = rdp_h,
            logical_w = s.logical_w,
            logical_h = s.logical_h,
            "MouseCoordMapper: RDP desktop size updated"
        );
    }
}

/// Bridges RDP input events to macOS CGEvent injection
pub struct MacInputHandler {
    keyboard: Option<KeyboardInjector>,
    mouse: Option<MouseInjector>,
    last_mouse_x: u16,
    last_mouse_y: u16,
    last_sync_lock_bits: Option<u8>,
    coord_mapper: MouseCoordMapper,
    client_context: ClientInputContext,
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

        Self {
            keyboard,
            mouse,
            last_mouse_x: 0,
            last_mouse_y: 0,
            last_sync_lock_bits: None,
            coord_mapper,
            client_context: ClientInputContext::default(),
        }
    }

    fn map_right_alt_to_option(&self) -> bool {
        let layout = self.client_context.keyboard_layout;
        let is_non_us_layout = layout != 0 && layout != 0x0409;
        let is_guacamole = self.client_context.client_name.contains("Guacamole");
        is_non_us_layout || is_guacamole
    }
}

impl RdpServerInputHandler for MacInputHandler {
    fn update_client_context(&mut self, context: ClientInputContext) {
        tracing::info!(
            client_name = context.client_name,
            keyboard_layout = format_args!("{:#06x}", context.keyboard_layout),
            "Updated client input context"
        );
        self.client_context = context;
    }

    fn keyboard(&mut self, event: KeyboardEvent) {
        let Some(kb) = &self.keyboard else { return };

        let result = match event {
            KeyboardEvent::Pressed { code, extended } => {
                if extended && code == 0x38 && self.map_right_alt_to_option() {
                    kb.inject_keycode(0x3D, true)
                } else {
                    kb.inject_key(code, extended, true)
                }
            }
            KeyboardEvent::Released { code, extended } => {
                if extended && code == 0x38 && self.map_right_alt_to_option() {
                    kb.inject_keycode(0x3D, false)
                } else {
                    kb.inject_key(code, extended, false)
                }
            }
            KeyboardEvent::UnicodePressed(ch) => kb.inject_unicode(ch, true),
            KeyboardEvent::UnicodeReleased(ch) => kb.inject_unicode(ch, false),
            KeyboardEvent::Synchronize(flags) => {
                let mut result = kb.release_transient_modifiers();
                let sync_bits = flags.bits();

                if let Some(previous_bits) = self.last_sync_lock_bits {
                    let changed = previous_bits ^ sync_bits;

                    if result.is_ok() && changed & 0x04 != 0 {
                        result = kb.tap_key(0x3A, false); // Caps Lock
                    }
                    if result.is_ok() && changed & 0x02 != 0 {
                        result = kb.tap_key(0x45, false); // Num Lock / Clear
                    }
                    if result.is_ok() && changed & 0x01 != 0 {
                        result = kb.tap_key(0x46, false); // Scroll Lock / F14
                    }

                    if changed & 0x08 != 0 {
                        tracing::debug!(
                            sync_bits,
                            "KANA lock changed in synchronize event; no macOS mapping is applied"
                        );
                    }
                } else {
                    tracing::debug!(sync_bits, "Captured initial keyboard synchronize state");
                }

                self.last_sync_lock_bits = Some(sync_bits);
                result
            }
        };

        if let Err(e) = result {
            tracing::warn!("Keyboard injection failed: {e}");
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(m) = &self.mouse else { return };

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
