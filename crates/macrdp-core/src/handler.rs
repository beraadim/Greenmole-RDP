use ironrdp_server::{ClientInputContext, KeyboardEvent, MouseEvent, RdpServerInputHandler};
pub use macrdp_capture::MouseCoordMapper;
use macrdp_input::{KeyboardInjector, MouseButton, MouseInjector, RemoteSessionWake};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::callbacks::InputDebugState;

mod keyboard_device;
mod translation;
use keyboard_device::KeyboardDevice;

/// Bridges RDP input events to macOS CGEvent injection
pub struct MacInputHandler {
    keyboard: Option<Box<dyn KeyboardDevice>>,
    mouse: Option<MouseInjector>,
    wake: Option<RemoteSessionWake>,
    last_mouse_x: f64,
    last_mouse_y: f64,
    last_sync_lock_bits: Option<u8>,
    coord_mapper: MouseCoordMapper,
    client_context: ClientInputContext,
    modifier_state: ModifierState,
    suppressed_ctrl_for_altgr: SuppressedCtrlState,
    pending_dead_key: Option<char>,
    translated_key_releases: HashMap<(u8, bool), Vec<u16>>,
    debug_state: Arc<Mutex<InputDebugState>>,
    active_connection: Arc<Mutex<Option<crate::callbacks::ConnectionInfo>>>,
    display_controller: Option<macrdp_display::DisplayController>,
    session_events: Option<SessionEvents>,
}

/// Records one real connection; resize reactivation does not reset its duration.
struct SessionEvents {
    handler: Arc<dyn crate::callbacks::ServerEventHandler>,
    gfx: Arc<Mutex<ironrdp_server::gfx::GfxState>>,
    audio_stats: Arc<macrdp_audio::AudioStats>,
    started_at: Option<std::time::Instant>,
}

#[derive(Debug, Default, Clone, Copy)]
struct ModifierState {
    caps_lock: bool,
    caps_lock_pressed: bool,
    left_shift: bool,
    right_shift: bool,
    left_ctrl: bool,
    right_ctrl: bool,
    left_alt: bool,
    right_alt: bool,
    left_win: bool,
    right_win: bool,
}

impl ModifierState {
    fn shift(self) -> bool {
        self.left_shift || self.right_shift
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct SuppressedCtrlState {
    left: bool,
    right: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextTranslation {
    Unicode(char),
    Dead(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyboardTranslationProfile {
    GuacamoleUs,
    NorwegianPhysical,
}

impl MacInputHandler {
    pub fn new(
        coord_mapper: MouseCoordMapper,
        debug_state: Arc<Mutex<InputDebugState>>,
        active_connection: Arc<Mutex<Option<crate::callbacks::ConnectionInfo>>>,
        display_controller: Option<macrdp_display::DisplayController>,
    ) -> Self {
        let keyboard = KeyboardInjector::new()
            .map(|keyboard| Box::new(keyboard) as Box<dyn KeyboardDevice>)
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
            last_sync_lock_bits: None,
            coord_mapper,
            client_context: ClientInputContext::default(),
            modifier_state: ModifierState::default(),
            suppressed_ctrl_for_altgr: SuppressedCtrlState::default(),
            pending_dead_key: None,
            translated_key_releases: HashMap::new(),
            debug_state,
            active_connection,
            display_controller,
            session_events: None,
        }
    }

    pub fn with_events(
        mut self,
        handler: Arc<dyn crate::callbacks::ServerEventHandler>,
        gfx: Arc<Mutex<ironrdp_server::gfx::GfxState>>,
        audio_stats: Arc<macrdp_audio::AudioStats>,
    ) -> Self {
        self.session_events = Some(SessionEvents {
            handler,
            gfx,
            audio_stats,
            started_at: None,
        });
        self
    }

    fn reset_session_input(&mut self, reason: &str) {
        if let Some(keyboard) = &self.keyboard {
            if let Err(error) = keyboard.release_all() {
                tracing::warn!(%error, reason, "Failed to release held remote keys");
            }
        }
        if let Some(mouse) = &mut self.mouse {
            if let Err(error) = mouse.release_all() {
                tracing::warn!(%error, reason, "Failed to release held remote mouse buttons");
            }
        }
        self.modifier_state = ModifierState::default();
        self.suppressed_ctrl_for_altgr = SuppressedCtrlState::default();
        self.pending_dead_key = None;
        self.translated_key_releases.clear();
        self.last_sync_lock_bits = None;
        self.set_last_translation(None);
    }

    fn translation_profile_name(&self) -> &'static str {
        match self.translation_profile() {
            Some(KeyboardTranslationProfile::GuacamoleUs) => "guacamole_us",
            Some(KeyboardTranslationProfile::NorwegianPhysical) => "norwegian_physical",
            None => "none",
        }
    }

    fn map_right_alt_to_option(&self) -> bool {
        let layout = self.client_context.keyboard_layout;
        let is_non_us_layout = layout != 0 && layout != 0x0409;
        let is_guacamole = self.client_context.client_name.contains("Guacamole");
        is_non_us_layout || is_guacamole
    }

    fn treat_right_ctrl_as_altgr_companion(&self) -> bool {
        matches!(
            self.translation_profile(),
            Some(KeyboardTranslationProfile::NorwegianPhysical)
        )
    }

    fn translation_profile(&self) -> Option<KeyboardTranslationProfile> {
        if self.client_context.client_name.contains("Guacamole") {
            Some(KeyboardTranslationProfile::GuacamoleUs)
        } else if self.client_context.keyboard_layout == 0x0414 {
            Some(KeyboardTranslationProfile::NorwegianPhysical)
        } else {
            None
        }
    }

    fn inject_unicode_sequence(
        kb: &dyn KeyboardDevice,
        chars: &[u16],
        pressed: bool,
    ) -> anyhow::Result<()> {
        if pressed {
            kb.release_transient_modifiers()?;
            for ch in chars {
                kb.inject_unicode(*ch, true)?;
            }
        } else {
            for ch in chars.iter().rev() {
                kb.inject_unicode(*ch, false)?;
            }
            kb.release_transient_modifiers()?;
        }

        Ok(())
    }

    fn handle_translated_key_press(
        &mut self,
        code: u8,
        extended: bool,
    ) -> Option<anyhow::Result<()>> {
        let translation = self.translate_text_key(code, extended)?;
        let kb = self.keyboard.as_deref().expect("keyboard checked above");

        let mut emitted = Vec::new();
        let result = match translation {
            TextTranslation::Unicode(ch) => {
                if let Some(accent) = self.pending_dead_key.take() {
                    if let Some(composed) = Self::compose_dead_key(accent, ch) {
                        if let Some(unit) = Self::char_to_u16(composed) {
                            emitted.push(unit);
                            Self::inject_unicode_sequence(kb, &emitted, true)
                        } else {
                            Ok(())
                        }
                    } else {
                        let Some(accent_unit) = Self::char_to_u16(accent) else {
                            return Some(Ok(()));
                        };
                        let Some(ch_unit) = Self::char_to_u16(ch) else {
                            return Some(Ok(()));
                        };
                        emitted.extend([accent_unit, ch_unit]);
                        Self::inject_unicode_sequence(kb, &[accent_unit], true)
                            .and_then(|_| Self::inject_unicode_sequence(kb, &[accent_unit], false))
                            .and_then(|_| Self::inject_unicode_sequence(kb, &[ch_unit], true))
                    }
                } else if let Some(unit) = Self::char_to_u16(ch) {
                    emitted.push(unit);
                    Self::inject_unicode_sequence(kb, &emitted, true)
                } else {
                    Ok(())
                }
            }
            TextTranslation::Dead(accent) => {
                if let Some(previous) = self.pending_dead_key.replace(accent) {
                    if let Some(unit) = Self::char_to_u16(previous) {
                        Self::inject_unicode_sequence(kb, &[unit], true)
                            .and_then(|_| Self::inject_unicode_sequence(kb, &[unit], false))
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
        };

        if !emitted.is_empty() {
            self.translated_key_releases
                .insert((code, extended), emitted);
        }

        self.set_last_translation(match translation {
            TextTranslation::Unicode(ch) => Some(ch.to_string()),
            TextTranslation::Dead(ch) => Some(format!("dead:{ch}")),
        });

        Some(result)
    }

    fn update_modifier_state(&mut self, code: u8, extended: bool, pressed: bool) {
        match (code, extended) {
            (0x3A, false) => {
                if pressed && !self.modifier_state.caps_lock_pressed {
                    self.modifier_state.caps_lock = !self.modifier_state.caps_lock;
                    if let Some(bits) = &mut self.last_sync_lock_bits {
                        *bits = (*bits & !0x04) | (u8::from(self.modifier_state.caps_lock) << 2);
                    }
                }
                self.modifier_state.caps_lock_pressed = pressed;
            }
            (0x2A, false) => self.modifier_state.left_shift = pressed,
            (0x36, false) => self.modifier_state.right_shift = pressed,
            (0x1D, false) => self.modifier_state.left_ctrl = pressed,
            (0x1D, true) => self.modifier_state.right_ctrl = pressed,
            (0x38, false) => self.modifier_state.left_alt = pressed,
            (0x38, true) => self.modifier_state.right_alt = pressed,
            (0x5B, true) => self.modifier_state.left_win = pressed,
            (0x5C, true) => self.modifier_state.right_win = pressed,
            _ => {}
        }
    }

    fn set_last_translation(&self, translation: Option<String>) {
        let mut debug = self.debug_state.lock().unwrap();
        debug.last_translation = translation;
    }

    fn refresh_debug_state(&self, last_event: Option<String>) {
        let mut active_modifiers = Vec::new();
        if self.modifier_state.caps_lock {
            active_modifiers.push("caps_lock".to_string());
        }
        if self.modifier_state.left_shift {
            active_modifiers.push("left_shift".to_string());
        }
        if self.modifier_state.right_shift {
            active_modifiers.push("right_shift".to_string());
        }
        if self.modifier_state.left_ctrl {
            active_modifiers.push("left_ctrl".to_string());
        }
        if self.modifier_state.right_ctrl {
            active_modifiers.push("right_ctrl".to_string());
        }
        if self.modifier_state.left_alt {
            active_modifiers.push("left_alt".to_string());
        }
        if self.modifier_state.right_alt {
            active_modifiers.push("right_alt".to_string());
        }
        if self.modifier_state.left_win {
            active_modifiers.push("left_win".to_string());
        }
        if self.modifier_state.right_win {
            active_modifiers.push("right_win".to_string());
        }

        let mut suppressed_modifiers = Vec::new();
        if self.suppressed_ctrl_for_altgr.left {
            suppressed_modifiers.push("left_ctrl_altgr_companion".to_string());
        }
        if self.suppressed_ctrl_for_altgr.right {
            suppressed_modifiers.push("right_ctrl_altgr_companion".to_string());
        }

        let mut debug = self.debug_state.lock().unwrap();
        debug.active_modifiers = active_modifiers;
        debug.suppressed_modifiers = suppressed_modifiers;
        if let Some(kb) = &self.keyboard {
            let (native_modifiers, native_flags) = kb.current_modifier_flags();
            debug.native_modifiers = native_modifiers;
            debug.native_flags_hex = format!("{native_flags:#010x}");
        } else {
            debug.native_modifiers.clear();
            debug.native_flags_hex = "0x00000000".to_string();
        }
        debug.translation_profile = self.translation_profile_name().to_string();
        debug.pending_dead_key = self.pending_dead_key.map(|ch| ch.to_string());
        if let Some(last_event) = last_event {
            debug.last_event = Some(last_event);
        }
    }

    fn clear_suppressed_altgr_ctrls(&mut self) {
        if self.suppressed_ctrl_for_altgr.left {
            self.modifier_state.left_ctrl = false;
        }
        if self.suppressed_ctrl_for_altgr.right {
            self.modifier_state.right_ctrl = false;
        }
        self.suppressed_ctrl_for_altgr = SuppressedCtrlState::default();
        if let Some(kb) = &self.keyboard {
            if let Err(e) = kb.release_transient_modifiers() {
                tracing::warn!("Failed to hard-reset keyboard state after AltGr cleanup: {e}");
            }
        }
    }

    fn release_ctrl_companion_if_needed(&mut self, extended: bool) -> anyhow::Result<()> {
        if extended {
            self.suppressed_ctrl_for_altgr.right = true;
            self.modifier_state.right_ctrl = false;
        } else {
            self.suppressed_ctrl_for_altgr.left = true;
            self.modifier_state.left_ctrl = false;
        }
        // Keep protocol modifier bookkeeping independent of native injection,
        // including when accessibility is unavailable or tests use no device.
        if let Some(kb) = &self.keyboard {
            kb.inject_key(0x1D, extended, false)?;
        }
        Ok(())
    }

    fn handle_norwegian_altgr_press(&mut self) -> anyhow::Result<()> {
        if self.modifier_state.left_ctrl {
            self.release_ctrl_companion_if_needed(false)?;
        }
        if self.modifier_state.right_ctrl {
            self.release_ctrl_companion_if_needed(true)?;
        }
        Ok(())
    }
}

impl RdpServerInputHandler for MacInputHandler {
    fn on_connect(&mut self, context: ClientInputContext, peer_addr: Option<String>) {
        self.reset_session_input("before new session");
        self.client_context = context.clone();
        if let Some(mouse) = &self.mouse {
            if let Ok((x, y)) = mouse.current_position() {
                self.last_mouse_x = x;
                self.last_mouse_y = y;
            }
        }
        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_session_start() {
                tracing::warn!("Failed to wake display for new remote session: {e}");
            }
        }
        let info = crate::callbacks::ConnectionInfo {
            client_ip: peer_addr.unwrap_or_else(|| "unknown".to_string()),
            client_name: context.client_name,
            connected_at: chrono::Utc::now().to_rfc3339(),
        };
        *self.active_connection.lock().unwrap() = Some(info.clone());
        if let Some(events) = &mut self.session_events {
            events.started_at = Some(std::time::Instant::now());
            events
                .handler
                .on_connection(crate::callbacks::ConnectionEvent::Connected(info));
        }
        self.refresh_debug_state(Some("connection:connected".to_string()));
    }

    fn on_disconnect(&mut self) {
        // End all held input before virtual-display teardown changes geometry.
        self.reset_session_input("on disconnect");
        let disconnected = {
            // Manual window actions hold this lock through their queue send.
            // Once disconnect starts, no action can target a later connection
            // based on this connection's stale availability check.
            let mut active_connection = self.active_connection.lock().unwrap();
            if let Some(display) = &self.display_controller {
                if let Err(error) = crate::display::capture_lifecycle::queue_restoration(display) {
                    tracing::warn!(%error, "Failed to schedule physical display restoration");
                }
            }
            active_connection.take()
        };
        if let (Some(events), Some(info)) = (&mut self.session_events, disconnected) {
            let duration_secs = events
                .started_at
                .take()
                .map_or(0, |start| start.elapsed().as_secs());
            let (bytes_total, reason, final_metrics) = {
                let gfx = events.gfx.lock().unwrap();
                (
                    gfx.total_bytes_sent,
                    gfx.connection_outcome
                        .map(|outcome| outcome.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    crate::server::final_metrics(&gfx, &events.audio_stats),
                )
            };
            events
                .handler
                .on_connection(crate::callbacks::ConnectionEvent::Disconnected(
                    crate::callbacks::DisconnectionInfo {
                        client_ip: info.client_ip,
                        client_name: info.client_name,
                        connected_at: info.connected_at,
                        disconnected_at: chrono::Utc::now().to_rfc3339(),
                        duration_secs,
                        bytes_total,
                        reason,
                        final_metrics,
                    },
                ));
        }
        if let Some(wake) = &mut self.wake {
            wake.deactivate_display_sleep_assertion();
        }
        self.refresh_debug_state(Some("connection:disconnected".to_string()));
    }

    fn update_client_context(&mut self, context: ClientInputContext) {
        tracing::info!(
            client_name = context.client_name,
            keyboard_layout = format_args!("{:#06x}", context.keyboard_layout),
            "Updated client input context"
        );
        self.client_context = context;
        self.refresh_debug_state(Some(format!(
            "context:{}:{:#06x}",
            self.client_context.client_name, self.client_context.keyboard_layout
        )));
    }

    fn keyboard(&mut self, event: KeyboardEvent) {
        if self.keyboard.is_none() {
            return;
        }

        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_input("keyboard") {
                tracing::warn!("Failed to declare remote keyboard activity: {e}");
            }
        }

        let event_label = format!("{event:?}");
        let result = match event {
            KeyboardEvent::Pressed { code, extended } => {
                let caps_repeat =
                    code == 0x3A && !extended && self.modifier_state.caps_lock_pressed;
                self.update_modifier_state(code, extended, true);

                if caps_repeat
                    || (code == 0x3A && !extended && self.translation_profile().is_some())
                {
                    // Unicode translation owns the remote lock state; toggling
                    // the host's global lock would double-apply it to text.
                    Ok(())
                } else if extended
                    && code == 0x38
                    && matches!(
                        self.translation_profile(),
                        Some(KeyboardTranslationProfile::NorwegianPhysical)
                    )
                {
                    self.handle_norwegian_altgr_press()
                } else if code == 0x1D
                    && matches!(
                        self.translation_profile(),
                        Some(KeyboardTranslationProfile::NorwegianPhysical)
                    )
                    && self.modifier_state.right_alt
                {
                    self.release_ctrl_companion_if_needed(extended)
                } else if extended && code == 0x1D && self.treat_right_ctrl_as_altgr_companion() {
                    Ok(())
                } else if extended && code == 0x38 && self.translation_profile().is_some() {
                    Ok(())
                } else if let Some(result) = self.handle_translated_key_press(code, extended) {
                    result
                } else if extended && code == 0x38 && self.map_right_alt_to_option() {
                    let kb = self.keyboard.as_deref().expect("keyboard checked above");
                    kb.inject_keycode(0x3D, true)
                } else {
                    let kb = self.keyboard.as_deref().expect("keyboard checked above");
                    kb.inject_key(code, extended, true)
                }
            }
            KeyboardEvent::Released { code, extended } => {
                self.update_modifier_state(code, extended, false);
                let kb = self.keyboard.as_deref().expect("keyboard checked above");

                if code == 0x3A && !extended && self.translation_profile().is_some() {
                    Ok(())
                } else if code == 0x1D
                    && ((extended && self.suppressed_ctrl_for_altgr.right)
                        || (!extended && self.suppressed_ctrl_for_altgr.left))
                {
                    if extended {
                        self.suppressed_ctrl_for_altgr.right = false;
                    } else {
                        self.suppressed_ctrl_for_altgr.left = false;
                    }
                    Ok(())
                } else if let Some(chars) = self.translated_key_releases.remove(&(code, extended)) {
                    Self::inject_unicode_sequence(kb, &chars, false)
                } else if extended && code == 0x1D && self.treat_right_ctrl_as_altgr_companion() {
                    Ok(())
                } else if extended && code == 0x38 && self.translation_profile().is_some() {
                    self.clear_suppressed_altgr_ctrls();
                    Ok(())
                } else if extended && code == 0x38 && self.map_right_alt_to_option() {
                    kb.inject_keycode(0x3D, false)
                } else {
                    kb.inject_key(code, extended, false)
                }
            }
            KeyboardEvent::UnicodePressed(ch) => self
                .keyboard
                .as_deref()
                .expect("keyboard checked above")
                .inject_unicode(ch, true),
            KeyboardEvent::UnicodeReleased(ch) => self
                .keyboard
                .as_deref()
                .expect("keyboard checked above")
                .inject_unicode(ch, false),
            KeyboardEvent::Synchronize(flags) => {
                let kb = self.keyboard.as_deref().expect("keyboard checked above");
                let mut result = kb.release_all();
                let sync_bits = flags.bits();

                if let Some(previous_bits) = self.last_sync_lock_bits {
                    let changed = previous_bits ^ sync_bits;

                    if result.is_ok() && changed & 0x04 != 0 && self.translation_profile().is_none()
                    {
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
                    // The first synchronization is authoritative too. Only
                    // physical profiles use the host lock; translated text
                    // keeps its Caps state entirely inside this session.
                    if result.is_ok() && self.translation_profile().is_none() {
                        let native_caps = kb
                            .current_modifier_flags()
                            .0
                            .iter()
                            .any(|name| name == "caps_lock");
                        if native_caps != (sync_bits & 0x04 != 0) {
                            result = kb.tap_key(0x3A, false);
                        }
                    }
                    tracing::debug!(sync_bits, "Captured initial keyboard synchronize state");
                }

                self.last_sync_lock_bits = Some(sync_bits);
                self.modifier_state = ModifierState {
                    caps_lock: sync_bits & 0x04 != 0,
                    ..ModifierState::default()
                };
                self.suppressed_ctrl_for_altgr = SuppressedCtrlState::default();
                self.pending_dead_key = None;
                self.translated_key_releases.clear();
                result
            }
        };

        self.refresh_debug_state(Some(event_label));

        if let Err(e) = result {
            tracing::warn!("Keyboard injection failed: {e}");
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(m) = &mut self.mouse else { return };

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
        }
    }
}

impl Drop for MacInputHandler {
    fn drop(&mut self) {
        // Cancellation during server shutdown must still record the connection.
        let connected = self.active_connection.lock().unwrap().is_some();
        if connected {
            self.on_disconnect();
        }
        self.reset_session_input("on drop");
        self.refresh_debug_state(Some("drop".to_string()));
    }
}

#[cfg(test)]
mod tests;
