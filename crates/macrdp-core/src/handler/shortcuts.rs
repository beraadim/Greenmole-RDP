use super::MacInputHandler;

#[derive(Clone, Copy)]
pub(super) struct CommandShortcut {
    keycode: u16,
    shifted: bool,
}

impl MacInputHandler {
    pub(super) fn handle_command_shortcut(
        &mut self,
        code: u8,
        extended: bool,
        pressed: bool,
    ) -> Option<anyhow::Result<()>> {
        let key = (code, extended);
        // Remember the original mapping through repeats and key-up, including
        // when Ctrl is released first or the client context changes mid-chord.
        if let Some(shortcut) = self.command_shortcuts.get(&key).copied() {
            let result = self.keyboard.as_deref()?.inject_command_keycode(
                shortcut.keycode,
                pressed,
                shortcut.shifted,
            );
            if !pressed && result.is_ok() {
                self.command_shortcuts.remove(&key);
            }
            return Some(result);
        }

        let modifiers = self.modifier_state;
        if !pressed
            || !(modifiers.left_ctrl || modifiers.right_ctrl)
            || modifiers.left_alt
            || modifiers.right_alt
            || modifiers.left_win
            || modifiers.right_win
            || self.translated_key_releases.contains_key(&key)
        {
            // AltGr can arrive as Ctrl+Right Alt. Never turn those text chords
            // into shortcuts, and leave Ctrl+Alt/Win combinations unchanged.
            return None;
        }
        let (keycode, label) = match key {
            (0x1E, false) => (0x00, "Ctrl+A -> Cmd+A"),
            (0x2D, false) => (0x07, "Ctrl+X -> Cmd+X"),
            (0x2E, false) => (0x08, "Ctrl+C -> Cmd+C"),
            (0x2F, false) => (0x09, "Ctrl+V -> Cmd+V"),
            (0x1C, false) => (0x24, "Ctrl+Enter -> Cmd+Enter"),
            (0x1C, true) => (0x4C, "Ctrl+Numpad Enter -> Cmd+Numpad Enter"),
            _ => return None,
        };
        let shortcut = CommandShortcut {
            keycode,
            shifted: modifiers.shift(),
        };
        let result = self.keyboard.as_deref()?.inject_command_keycode(
            shortcut.keycode,
            true,
            shortcut.shifted,
        );
        if result.is_ok() {
            self.command_shortcuts.insert(key, shortcut);
            self.pending_dead_key = None;
            self.set_last_translation(Some(label.to_string()));
            tracing::debug!(
                shortcut = label,
                shifted = shortcut.shifted,
                "Translated Control shortcut"
            );
        }
        Some(result)
    }
}
