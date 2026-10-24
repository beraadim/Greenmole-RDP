//! Shortcut policy is independent of the text layout negotiated over RDP.
use super::{MacInputHandler, TextTranslation};

/// Versioned virtual keyboard subtypes (MS-RDPBCGR's OEM-dependent field).
/// These are preferences only, never authentication or capability grants.
const WINDOWS: u32 = 0x474D_0101;
const MAC: u32 = 0x474D_0102;
const PHYSICAL: u32 = 0x474D_0103;

impl MacInputHandler {
    pub(super) fn shortcut_policy(&self) -> &str {
        match self.keyboard_shortcuts.as_str() {
            "mac" => "mac",
            "physical" => "physical",
            "windows" => "windows",
            _ => match self.client_context.keyboard_subtype {
                MAC => "mac",
                PHYSICAL => "physical",
                WINDOWS => "windows",
                // Existing native RDP clients and older Greenmole retain the
                // established Windows-friendly behavior without migration.
                _ => "windows",
            },
        }
    }

    pub(super) fn translates_altgr(&self) -> bool {
        !self.native_mac_option() && self.translation_profile().is_some_and(|p| p.has_altgr())
    }

    // In browser sessions guacd synthesizes AltGr to encode the advertised
    // Windows layout, even when the originating keyboard is a Mac. A native
    // Mac client with an explicit Mac policy uses Option as a native modifier.
    pub(super) fn native_mac_option(&self) -> bool {
        self.shortcut_policy() == "mac"
            && !matches!(
                self.client_context.keyboard_subtype,
                WINDOWS | MAC | PHYSICAL
            )
    }

    pub(super) fn source_character(&self, code: u8, extended: bool) -> Option<char> {
        if extended {
            return None;
        }
        match self
            .translation_profile()?
            .translate(code, false, false, false)?
        {
            TextTranslation::Unicode(ch) => Some(ch),
            TextTranslation::Dead(_) => None,
        }
    }

    fn native_modifier_override(&self, code: u8, extended: bool) -> Option<u16> {
        let native_modifiers = self.shortcut_policy() != "windows";
        match (code, extended) {
            (0x38, false) if native_modifiers => Some(0x3A), // Alt/Option -> Option
            (0x38, true) if native_modifiers || self.map_right_alt_to_option() => Some(0x3D),
            (0x5B, true) if native_modifiers => Some(0x37), // Super/Command -> Command
            (0x5C, true) if native_modifiers => Some(0x36),
            _ => None,
        }
    }

    /// Unicode translation clears native modifiers without changing the keys
    /// held by the RDP client. Restore those keys before the next physical
    /// action (for example Shift+Arrow after typing an uppercase letter).
    fn restore_physical_modifiers(&mut self, current: (u8, bool)) -> anyhow::Result<()> {
        let m = self.modifier_state;
        for (key, held) in [
            ((0x2A, false), m.left_shift),
            ((0x36, false), m.right_shift),
            (
                (0x1D, false),
                m.left_ctrl && !self.suppressed_ctrl_for_altgr.left,
            ),
            (
                (0x1D, true),
                m.right_ctrl
                    && !self.suppressed_ctrl_for_altgr.right
                    && !self.treat_right_ctrl_as_altgr_companion(),
            ),
            ((0x38, false), m.left_alt),
            (
                (0x38, true),
                m.right_alt
                    && !self.translates_altgr()
                    && !(self.shortcut_policy() == "windows"
                        && self.translation_profile().is_some()),
            ),
            ((0x5B, true), m.left_win),
            ((0x5C, true), m.right_win),
        ] {
            if !held || key == current {
                continue;
            }
            let keycode = self
                .mapped_key_releases
                .get(&key)
                .copied()
                .or_else(|| self.native_modifier_override(key.0, key.1))
                .or_else(|| macrdp_input::scancode_to_keycode(key.0, key.1));
            if let Some(keycode) = keycode {
                self.keyboard
                    .as_deref()
                    .expect("keyboard checked above")
                    .ensure_keycode_pressed(keycode)?;
                self.mapped_key_releases.insert(key, keycode);
            }
        }
        Ok(())
    }

    /// Remember the selected native key through key-up and repeats. A layout
    /// refresh or modifier release must not change the key being released.
    pub(super) fn inject_physical_key(
        &mut self,
        code: u8,
        extended: bool,
        pressed: bool,
    ) -> anyhow::Result<()> {
        let key = (code, extended);
        if pressed {
            self.restore_physical_modifiers(key)?;
        }
        let kb = self.keyboard.as_deref().expect("keyboard checked above");
        if let Some(keycode) = self.mapped_key_releases.get(&key).copied() {
            kb.inject_keycode(keycode, pressed)?;
            if !pressed {
                self.mapped_key_releases.remove(&key);
            }
            return Ok(());
        }
        let override_code = self.native_modifier_override(code, extended);
        let m = self.modifier_state;
        let chord =
            m.left_ctrl || m.right_ctrl || m.left_alt || m.right_alt || m.left_win || m.right_win;
        let character_code = if chord {
            self.source_character(code, extended)
                .filter(|ch| ch.is_ascii_alphabetic())
                .and_then(|ch| kb.keycode_for_character(ch))
        } else {
            None
        };
        if let Some(keycode) = override_code
            .or(character_code)
            .or_else(|| macrdp_input::scancode_to_keycode(code, extended))
        {
            kb.inject_keycode(keycode, pressed)?;
            if pressed {
                self.mapped_key_releases.insert(key, keycode);
            }
        }
        Ok(())
    }
}
