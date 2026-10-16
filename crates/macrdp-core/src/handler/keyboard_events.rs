use super::{KeyboardTranslationProfile, MacInputHandler, ModifierState, SuppressedCtrlState};
use ironrdp_server::KeyboardEvent;

impl MacInputHandler {
    pub(super) fn handle_keyboard_event(&mut self, event: KeyboardEvent) {
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

                if let Some(result) = self.handle_command_shortcut(code, extended, true) {
                    result
                } else if caps_repeat
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
                let shortcut_result = self.handle_command_shortcut(code, extended, false);
                let kb = self.keyboard.as_deref().expect("keyboard checked above");

                if let Some(result) = shortcut_result {
                    result
                } else if code == 0x3A && !extended && self.translation_profile().is_some() {
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
                self.command_shortcuts.clear();
                result
            }
        };

        self.refresh_debug_state(Some(event_label));

        if let Err(e) = result {
            tracing::warn!("Keyboard injection failed: {e}");
        }
    }
}
