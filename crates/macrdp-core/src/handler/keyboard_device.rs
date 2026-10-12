use macrdp_input::KeyboardInjector;

/// A recording implementation lets lifecycle and translation tests exercise the
/// real handler without creating a CGEvent or changing the host keyboard state.
pub(super) trait KeyboardDevice: Send {
    fn inject_keycode(&self, keycode: u16, pressed: bool) -> anyhow::Result<()>;
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
    fn inject_keycode(&self, keycode: u16, pressed: bool) -> anyhow::Result<()> {
        self.inject_keycode(keycode, pressed)
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
