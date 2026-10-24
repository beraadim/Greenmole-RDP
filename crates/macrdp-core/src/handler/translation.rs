use super::{MacInputHandler, TextTranslation};

impl MacInputHandler {
    /// Undefined text levels (including AltGr and French Shift+²) must not
    /// leak through as an unrelated physical key in the host's layout.
    pub(super) fn is_unmapped_text_chord(&self, code: u8, extended: bool) -> bool {
        if extended
            || self.modifier_state.left_alt
            || self.modifier_state.left_win
            || self.modifier_state.right_win
        {
            return false;
        }
        let altgr = self.modifier_state.right_alt && self.translates_altgr();
        if (self.modifier_state.right_alt
            || self.modifier_state.left_ctrl
            || self.modifier_state.right_ctrl)
            && !altgr
        {
            return false;
        }
        self.translation_profile().is_some_and(|profile| {
            profile.translate(code, false, false, false).is_some()
                && profile
                    .translate(
                        code,
                        self.modifier_state.shift(),
                        altgr,
                        self.modifier_state.caps_lock,
                    )
                    .is_none()
        })
    }

    pub(super) fn translate_text_key(&self, code: u8, extended: bool) -> Option<TextTranslation> {
        if extended {
            return None;
        }
        let profile = self.translation_profile()?;
        if self.modifier_state.right_alt
            && (self.native_mac_option()
                || (!profile.has_altgr() && matches!(self.shortcut_policy(), "mac" | "physical")))
        {
            // A native Mac Option chord must reach macOS as a chord. The US
            // bridge keeps its legacy text behavior in Windows compatibility.
            return None;
        }
        let altgr = self.modifier_state.right_alt && self.translates_altgr();
        let ctrl_blocks_text =
            (self.modifier_state.left_ctrl || self.modifier_state.right_ctrl) && !altgr;
        if ctrl_blocks_text
            || self.modifier_state.left_alt
            || self.modifier_state.left_win
            || self.modifier_state.right_win
        {
            return None;
        }
        profile.translate(
            code,
            self.modifier_state.shift(),
            altgr,
            self.modifier_state.caps_lock,
        )
    }

    pub(super) fn compose_dead_key(accent: char, base: char) -> Option<char> {
        match (accent, base) {
            ('¨', 'a') => Some('ä'),
            ('¨', 'e') => Some('ë'),
            ('¨', 'i') => Some('ï'),
            ('¨', 'o') => Some('ö'),
            ('¨', 'u') => Some('ü'),
            ('¨', 'y') => Some('ÿ'),
            ('¨', 'A') => Some('Ä'),
            ('¨', 'E') => Some('Ë'),
            ('¨', 'I') => Some('Ï'),
            ('¨', 'O') => Some('Ö'),
            ('¨', 'U') => Some('Ü'),
            ('¨', 'Y') => Some('Ÿ'),
            ('¨', ' ') => Some('¨'),
            ('^', 'a') => Some('â'),
            ('^', 'e') => Some('ê'),
            ('^', 'i') => Some('î'),
            ('^', 'o') => Some('ô'),
            ('^', 'u') => Some('û'),
            ('^', 'A') => Some('Â'),
            ('^', 'E') => Some('Ê'),
            ('^', 'I') => Some('Î'),
            ('^', 'O') => Some('Ô'),
            ('^', 'U') => Some('Û'),
            ('^', ' ') => Some('^'),
            ('~', 'a') => Some('ã'),
            ('~', 'o') => Some('õ'),
            ('~', 'n') => Some('ñ'),
            ('~', 'A') => Some('Ã'),
            ('~', 'O') => Some('Õ'),
            ('~', 'N') => Some('Ñ'),
            ('~', ' ') => Some('~'),
            ('´', 'a') => Some('á'),
            ('´', 'e') => Some('é'),
            ('´', 'i') => Some('í'),
            ('´', 'o') => Some('ó'),
            ('´', 'u') => Some('ú'),
            ('´', 'y') => Some('ý'),
            ('´', 'A') => Some('Á'),
            ('´', 'E') => Some('É'),
            ('´', 'I') => Some('Í'),
            ('´', 'O') => Some('Ó'),
            ('´', 'U') => Some('Ú'),
            ('´', 'Y') => Some('Ý'),
            ('´', ' ') => Some('´'),
            ('`', 'a') => Some('à'),
            ('`', 'e') => Some('è'),
            ('`', 'i') => Some('ì'),
            ('`', 'o') => Some('ò'),
            ('`', 'u') => Some('ù'),
            ('`', 'A') => Some('À'),
            ('`', 'E') => Some('È'),
            ('`', 'I') => Some('Ì'),
            ('`', 'O') => Some('Ò'),
            ('`', 'U') => Some('Ù'),
            ('`', ' ') => Some('`'),
            _ => None,
        }
    }

    pub(super) fn char_to_u16(ch: char) -> Option<u16> {
        u16::try_from(u32::from(ch)).ok()
    }
}
