use anyhow::Result;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use crate::keymap::scancode_to_keycode;

unsafe extern "C" {
    fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> CGEventFlags;
}

pub struct KeyboardInjector;

impl KeyboardInjector {
    const TRANSIENT_MODIFIERS: &'static [(u8, bool)] = &[
        (0x2A, false), // Left Shift
        (0x36, false), // Right Shift
        (0x1D, false), // Left Ctrl
        (0x1D, true),  // Right Ctrl
        (0x38, false), // Left Alt / Command mapping
        (0x38, true),  // Right Alt / Command mapping
        (0x5B, true),  // Left Win / Option mapping
        (0x5C, true),  // Right Win / Option mapping
    ];

    pub fn new() -> Result<Self> {
        // Verify we can create an event source (permission check)
        let _ = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
            anyhow::anyhow!("Failed to create CGEventSource — check Accessibility permission")
        })?;
        Ok(Self)
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
        let source = Self::source()?;
        let event = CGEvent::new_keyboard_event(source, keycode as CGKeyCode, pressed)
            .map_err(|_| anyhow::anyhow!("Failed to create keyboard event"))?;

        event.post(CGEventTapLocation::HID);
        tracing::trace!(keycode, pressed, "Keyboard keycode injected");
        Ok(())
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

    pub fn release_transient_modifiers(&self) -> Result<()> {
        for (scancode, extended) in Self::TRANSIENT_MODIFIERS {
            self.inject_key(*scancode, *extended, false)?;
        }

        // Release both right-side macOS modifiers explicitly so session-specific
        // AltGr handling cannot leave a stuck key behind after sync.
        self.inject_keycode(0x36, false)?; // Right Command
        self.inject_keycode(0x3D, false)?; // Right Option

        Ok(())
    }

    pub fn hard_reset_state(&self) -> Result<()> {
        self.release_transient_modifiers()?;
        self.inject_keycode(0x37, false)?; // Left Command
        self.inject_keycode(0x3A, false)?; // Left Option
        self.inject_keycode(0x3B, false)?; // Left Control
        self.inject_keycode(0x38, false)?; // Left Shift
        self.inject_keycode(0x3C, false)?; // Right Shift
        self.inject_keycode(0x3E, false)?; // Right Control
        self.inject_keycode(0x3F, false)?; // Function
        Ok(())
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
