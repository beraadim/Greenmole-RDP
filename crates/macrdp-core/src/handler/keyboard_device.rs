use macrdp_input::KeyboardInjector;

/// A recording implementation lets lifecycle and translation tests exercise the
/// real handler without creating a CGEvent or changing the host keyboard state.
pub(super) trait KeyboardDevice: Send {
    /// Recording devices default to an ANSI host without accessing local APIs.
    fn keycode_for_character(&self, ch: char) -> Option<u16> {
        let letters = "abcdefghijklmnopqrstuvwxyz";
        let codes = [
            0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D,
            0x1F, 0x23, 0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
        ];
        letters.find(ch.to_ascii_lowercase()).map(|i| codes[i])
    }
    fn inject_keycode(&self, keycode: u16, pressed: bool) -> anyhow::Result<()>;
    /// Restore a protocol-held modifier after translated text temporarily
    /// released it. Stateful devices avoid sending a duplicate key-down.
    fn ensure_keycode_pressed(&self, keycode: u16) -> anyhow::Result<()> {
        self.inject_keycode(keycode, true)
    }
    fn inject_command_keycode(
        &self,
        keycode: u16,
        pressed: bool,
        shifted: bool,
    ) -> anyhow::Result<()>;
    fn inject_unicode(&self, ch: u16, pressed: bool) -> anyhow::Result<()>;
    fn release_transient_modifiers(&self) -> anyhow::Result<()>;
    fn release_all(&self) -> anyhow::Result<()>;
    fn current_modifier_flags(&self) -> (Vec<String>, u64);

    fn inject_key(&self, code: u8, extended: bool, pressed: bool) -> anyhow::Result<()> {
        if let Some(keycode) = macrdp_input::scancode_to_keycode(code, extended) {
            self.inject_keycode(keycode, pressed)?;
        }
        Ok(())
    }

    fn tap_key(&self, code: u8, extended: bool) -> anyhow::Result<()> {
        self.inject_key(code, extended, true)?;
        self.inject_key(code, extended, false)
    }
}

impl KeyboardDevice for KeyboardInjector {
    fn keycode_for_character(&self, ch: char) -> Option<u16> {
        macrdp_input::host_layout::keycode_for_character(ch)
    }
    fn inject_command_keycode(
        &self,
        keycode: u16,
        pressed: bool,
        shifted: bool,
    ) -> anyhow::Result<()> {
        self.inject_command_keycode(keycode, pressed, shifted)
    }
    fn inject_keycode(&self, keycode: u16, pressed: bool) -> anyhow::Result<()> {
        self.inject_keycode(keycode, pressed)
    }
    fn ensure_keycode_pressed(&self, keycode: u16) -> anyhow::Result<()> {
        self.ensure_keycode_pressed(keycode)
    }
    fn inject_unicode(&self, ch: u16, pressed: bool) -> anyhow::Result<()> {
        self.inject_unicode(ch, pressed)
    }
    fn release_transient_modifiers(&self) -> anyhow::Result<()> {
        self.release_transient_modifiers()
    }
    fn release_all(&self) -> anyhow::Result<()> {
        self.release_all()
    }
    fn current_modifier_flags(&self) -> (Vec<String>, u64) {
        self.current_modifier_flags()
    }
}
