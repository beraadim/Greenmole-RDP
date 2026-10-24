use anyhow::Result;
use std::cell::RefCell;

mod command;
mod modifiers;
mod state;
use core_graphics::event::{CGEvent, CGEventFlags, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use modifiers::RemoteModifiers;
use state::{InjectedKey, KeyboardState};

use crate::event_tag::{post_remote_event, remote_event_tag, set_private_modifier_flags};
use crate::keymap::scancode_to_keycode;

unsafe extern "C" {
    fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> CGEventFlags;
}

/// Tracks successfully posted keys independently of local hardware state.
pub struct KeyboardInjector {
    state: RefCell<KeyboardState>,
}

impl KeyboardInjector {
    pub fn new() -> Result<Self> {
        // Verify we can create an event source (permission check)
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self {
            state: RefCell::new(KeyboardState::default()),
        })
    }

    fn source(private: bool) -> Result<CGEventSource> {
        let state = if private {
            CGEventSourceStateID::Private
        } else {
            CGEventSourceStateID::HIDSystemState
        };
        CGEventSource::new(state).map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))
    }

    fn private_source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::Private)
            .map_err(|_| anyhow::anyhow!("Failed to create private CGEventSource"))
    }

    pub fn inject_keycode(&self, keycode: u16, pressed: bool) -> Result<()> {
        self.with_state(|state| state.inject(InjectedKey::Physical(keycode), pressed, Self::post))
    }

    /// Reassert a remotely held modifier only when text injection has released
    /// it. Normal key repeats must continue using `inject_keycode` instead.
    pub fn ensure_keycode_pressed(&self, keycode: u16) -> Result<()> {
        self.with_state(|state| state.ensure_pressed(InjectedKey::Physical(keycode), Self::post))
    }

    /// Apply Command only to this shortcut, then explicitly restore modifiers.
    pub fn inject_command_keycode(&self, keycode: u16, pressed: bool, shifted: bool) -> Result<()> {
        self.with_state(|state| {
            state.inject(
                InjectedKey::CommandShortcut { keycode, shifted },
                pressed,
                Self::post,
            )
        })
    }

    fn with_state<T>(&self, update: impl FnOnce(&mut KeyboardState) -> T) -> T {
        let mut state = self.state.borrow_mut();
        state.synchronize_private_tag(remote_event_tag());
        update(&mut state)
    }

    fn post(key: InjectedKey, pressed: bool, modifiers: RemoteModifiers) -> Result<()> {
        let keycode = match key {
            InjectedKey::Physical(keycode) => keycode,
            InjectedKey::Unicode(ch) => return Self::post_unicode(ch, pressed),
            InjectedKey::CommandShortcut { keycode, shifted } => {
                return command::post(keycode, pressed, shifted, modifiers);
            }
        };
        let private = remote_event_tag() != 0;
        let event = Self::keycode_event(keycode, pressed, private.then_some(modifiers))?;
        post_remote_event(&event)?;
        if private {
            set_private_modifier_flags(modifiers.flags);
        }
        if remote_event_tag() == 0 {
            tracing::trace!(keycode, pressed, "Keyboard keycode injected");
        }
        Ok(())
    }

    fn keycode_event(
        keycode: u16,
        pressed: bool,
        modifiers: Option<RemoteModifiers>,
    ) -> Result<CGEvent> {
        let source = Self::source(modifiers.is_some())?;
        let event = CGEvent::new_keyboard_event(source, keycode as CGKeyCode, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create keyboard event"))?;
        if let Some(modifiers) = modifiers {
            // A fresh private source has no hardware-derived modifier state.
            // Set the complete remote state on every event, so source-internal
            // key-state persistence is unnecessary even if worker threads move.
            let keypad = event.get_flags() & CGEventFlags::CGEventFlagNumericPad;
            event.set_flags(modifiers.flags | keypad);
        }

        Ok(event)
    }

    /// Inject a key press or release event
    pub fn inject_key(&self, scancode: u8, extended: bool, pressed: bool) -> Result<()> {
        let keycode = match scancode_to_keycode(scancode, extended) {
            Some(kc) => kc,
            None => {
                if remote_event_tag() == 0 {
                    tracing::warn!(scancode, extended, "Unknown scancode, ignoring");
                } else {
                    tracing::warn!("Unknown scancode ignored during private session");
                }
                return Ok(());
            }
        };

        self.inject_keycode(keycode, pressed)?;
        if remote_event_tag() == 0 {
            tracing::trace!(scancode, keycode, pressed, "Keyboard event injected");
        }
        Ok(())
    }

    pub fn tap_key(&self, scancode: u8, extended: bool) -> Result<()> {
        self.inject_key(scancode, extended, true)?;
        self.inject_key(scancode, extended, false)
    }

    /// Release only modifiers successfully pressed by this injector. Never emit
    /// blanket key-ups for local modifiers when a remote session synchronizes.
    pub fn release_transient_modifiers(&self) -> Result<()> {
        self.with_state(|state| state.release_matching(InjectedKey::is_modifier, Self::post))
    }

    /// Release physical keys and Unicode events, including failed earlier key-ups.
    /// Successful releases are removed; a later cleanup retries only failures.
    pub fn release_all(&self) -> Result<()> {
        self.with_state(|state| state.release_matching(|_| true, Self::post))
    }

    pub fn current_modifier_flags(&self) -> (Vec<String>, u64) {
        let flags = if remote_event_tag() == 0 {
            unsafe { CGEventSourceFlagsState(CGEventSourceStateID::HIDSystemState) }
        } else {
            self.with_state(|state| state.modifiers().flags)
        };
        let mut names = Vec::new();

        if flags.contains(CGEventFlags::CGEventFlagShift) {
            names.push("shift".to_string());
        }
        if flags.contains(CGEventFlags::CGEventFlagControl) {
            names.push("control".to_string());
        }
        if flags.contains(CGEventFlags::CGEventFlagAlternate) {
            names.push("option".to_string());
        }
        if flags.contains(CGEventFlags::CGEventFlagCommand) {
            names.push("command".to_string());
        }
        if flags.contains(CGEventFlags::CGEventFlagAlphaShift) {
            names.push("caps_lock".to_string());
        }
        if flags.contains(CGEventFlags::CGEventFlagSecondaryFn) {
            names.push("fn".to_string());
        }
        if flags.contains(CGEventFlags::CGEventFlagNumericPad) {
            names.push("numeric_pad".to_string());
        }

        (names, flags.bits())
    }

    /// Inject a unicode character press/release
    pub fn inject_unicode(&self, ch: u16, pressed: bool) -> Result<()> {
        self.with_state(|state| state.inject(InjectedKey::Unicode(ch), pressed, Self::post))
    }

    fn post_unicode(ch: u16, pressed: bool) -> Result<()> {
        // Use an isolated event source here so translated text injection does
        // not inherit or feed back into the live HID modifier state.
        let source = Self::private_source()?;
        let event = CGEvent::new_keyboard_event(source, 0, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create unicode event"))?;
        event.set_flags(CGEventFlags::empty());

        if pressed {
            event.set_string_from_utf16_unchecked(&[ch]);
        }

        post_remote_event(&event)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graphics::event::EventField;

    #[test]
    fn guarded_key_events_use_private_sources_and_complete_remote_flags_without_posting() {
        for (codes, caps) in [(vec![], false), (vec![0x38, 0x36, 0x3D], true)] {
            let modifiers = RemoteModifiers::from_keys(|code| codes.contains(&code), caps);
            for pressed in [true, false] {
                let event =
                    KeyboardInjector::keycode_event(0x00, pressed, Some(modifiers)).unwrap();
                assert_eq!(event.get_flags(), modifiers.flags);
                // Private is the creation request (-1); CoreGraphics assigns
                // the resulting private state its own identifier.
                let state = event.get_integer_value_field(EventField::EVENT_SOURCE_STATE_ID);
                assert_ne!(state, CGEventSourceStateID::HIDSystemState as i64);
                assert_ne!(state, CGEventSourceStateID::CombinedSessionState as i64);
                assert_eq!(
                    event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
                    0x00
                );
            }
        }
        let ordinary = KeyboardInjector::keycode_event(0x00, true, None).unwrap();
        assert_eq!(
            ordinary.get_integer_value_field(EventField::EVENT_SOURCE_STATE_ID),
            CGEventSourceStateID::HIDSystemState as i64
        );
    }
}

impl Drop for KeyboardInjector {
    fn drop(&mut self) {
        if let Err(error) = self.release_all() {
            tracing::warn!(%error, "Failed to release held remote keys on injector drop");
        }
    }
}
