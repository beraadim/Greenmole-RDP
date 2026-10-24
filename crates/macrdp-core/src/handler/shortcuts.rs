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
        if self.shortcut_policy() != "windows"
            || !pressed
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
        let (keycode, label) = if code == 0x1C {
            (
                if extended { 0x4C } else { 0x24 },
                "Ctrl+Enter -> Cmd+Enter",
            )
        } else {
            // Resolve the logical letter in the negotiated source layout, then
            // the host layout. French Ctrl+A is not at the US A position.
            let character = self.source_character(code, extended).or_else(|| {
                // Explicit physical/unknown layouts retain legacy bindings.
                if self.translation_profile().is_none() && !extended {
                    match code {
                        0x1E => Some('a'),
                        0x2D => Some('x'),
                        0x2E => Some('c'),
                        0x2F => Some('v'),
                        _ => None,
                    }
                } else {
                    None
                }
            })?;
            let label = match character {
                'a' => "Ctrl+A -> Cmd+A",
                'x' => "Ctrl+X -> Cmd+X",
                'c' => "Ctrl+C -> Cmd+C",
                'v' => "Ctrl+V -> Cmd+V",
                _ => return None,
            };
            let Some(keycode) = self.keyboard.as_deref()?.keycode_for_character(character) else {
                return Some(Err(anyhow::anyhow!(
                    "Host keyboard layout cannot resolve editing shortcut"
                )));
            };
            (keycode, label)
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
