use anyhow::Result;
use std::cell::RefCell;

mod state;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use state::{InjectedKey, KeyboardState};

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

    fn source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))
    }

    fn private_source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::Private)
            .map_err(|_| anyhow::anyhow!("Failed to create private CGEventSource"))
    }

    pub fn inject_keycode(&self, keycode: u16, pressed: bool) -> Result<()> {
        self.state
            .borrow_mut()
            .inject(InjectedKey::Physical(keycode), pressed, Self::post)
    }

    /// Override Control for this shortcut event without pressing a synthetic
    /// Command modifier that could affect other held keys or become stuck.
    pub fn inject_command_keycode(&self, keycode: u16, pressed: bool, shifted: bool) -> Result<()> {
        self.state.borrow_mut().inject(
            InjectedKey::CommandShortcut { keycode, shifted },
            pressed,
            Self::post,
        )
    }

    fn post(key: InjectedKey, pressed: bool) -> Result<()> {
        let keycode = match key {
            InjectedKey::Physical(keycode) => keycode,
            InjectedKey::Unicode(ch) => return Self::post_unicode(ch, pressed),
            InjectedKey::CommandShortcut { keycode, shifted } => {
                let event = Self::command_event(keycode, pressed, shifted)?;
                event.post(CGEventTapLocation::HID);
                return Ok(());
            }
        };
        let source = Self::source()?;
        let event = CGEvent::new_keyboard_event(source, keycode as CGKeyCode, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create keyboard event"))?;

        event.post(CGEventTapLocation::HID);
        tracing::trace!(keycode, pressed, "Keyboard keycode injected");
        Ok(())
    }

    fn command_event(keycode: u16, pressed: bool, shifted: bool) -> Result<CGEvent> {
        // Explicit flags from an isolated source avoid inheriting either Ctrl
        // key, stale native modifiers, or the host's keyboard layout Caps state.
        let event = CGEvent::new_keyboard_event(Self::private_source()?, keycode, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create Command shortcut event"))?;
        let mut flags = CGEventFlags::CGEventFlagCommand;
        flags.set(CGEventFlags::CGEventFlagShift, shifted);
        flags.set(CGEventFlags::CGEventFlagNumericPad, keycode == 0x4C);
        event.set_flags(flags);
        Ok(event)
    }

    /// Inject a key press or release event
    pub fn inject_key(&self, scancode: u8, extended: bool, pressed: bool) -> Result<()> {
        let keycode = match scancode_to_keycode(scancode, extended) {
            Some(kc) => kc,
            None => {
                tracing::warn!(scancode, extended, "Unknown scancode, ignoring");
                return Ok(());
            }
        };

        self.inject_keycode(keycode, pressed)?;
        tracing::trace!(scancode, keycode, pressed, "Keyboard event injected");
        Ok(())
    }

    pub fn tap_key(&self, scancode: u8, extended: bool) -> Result<()> {
        self.inject_key(scancode, extended, true)?;
        self.inject_key(scancode, extended, false)
    }

    /// Release only modifiers successfully pressed by this injector. Never emit
    /// blanket key-ups for local modifiers when a remote session synchronizes.
    pub fn release_transient_modifiers(&self) -> Result<()> {
        self.state
            .borrow_mut()
            .release_matching(InjectedKey::is_modifier, Self::post)
    }

    /// Release physical keys and Unicode events, including failed earlier key-ups.
    /// Successful releases are removed; a later cleanup retries only failures.
    pub fn release_all(&self) -> Result<()> {
        self.state
            .borrow_mut()
            .release_matching(|_| true, Self::post)
    }

    pub fn current_modifier_flags(&self) -> (Vec<String>, u64) {
        let flags = unsafe { CGEventSourceFlagsState(CGEventSourceStateID::HIDSystemState) };
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
        self.state
            .borrow_mut()
            .inject(InjectedKey::Unicode(ch), pressed, Self::post)
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

        event.post(CGEventTapLocation::HID);
        Ok(())
    }
}

impl Drop for KeyboardInjector {
    fn drop(&mut self) {
        if let Err(error) = self.release_all() {
            tracing::warn!(%error, "Failed to release held remote keys on injector drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graphics::event::{CGEventType, EventField};

    #[test]
    fn command_events_have_exact_modifiers_on_press_and_release_without_posting() {
        // Construct but never post events: running tests must not send shortcuts
        // to the developer's foreground application or change native modifiers.
        for keycode in [0x08, 0x09, 0x24, 0x4C] {
            for shifted in [false, true] {
                for pressed in [false, true] {
                    let event = KeyboardInjector::command_event(keycode, pressed, shifted).unwrap();
                    let mut expected = CGEventFlags::CGEventFlagCommand;
                    expected.set(CGEventFlags::CGEventFlagShift, shifted);
                    expected.set(CGEventFlags::CGEventFlagNumericPad, keycode == 0x4C);
                    assert_eq!(event.get_flags(), expected);
                    assert_eq!(
                        event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
                        i64::from(keycode)
                    );
                    assert_eq!(
                        event.get_type() as u32,
                        if pressed {
                            CGEventType::KeyDown
                        } else {
                            CGEventType::KeyUp
                        } as u32
                    );
                }
            }
        }
    }
}
