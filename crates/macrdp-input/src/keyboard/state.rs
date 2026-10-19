use std::collections::BTreeSet;

use super::modifiers::RemoteModifiers;
use anyhow::Result;
use core_graphics::event::CGEventFlags;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum InjectedKey {
    Physical(u16),
    Unicode(u16),
    CommandShortcut { keycode: u16, shifted: bool },
}

impl InjectedKey {
    pub(super) fn is_modifier(self) -> bool {
        matches!(self, Self::Physical(0x36..=0x38 | 0x3A..=0x3F))
    }
}

/// The set changes only after a successful post. Repeated key-downs still reach
/// the application, while releases for keys this session never pressed do not.
#[derive(Default)]
pub(super) struct KeyboardState {
    held: BTreeSet<InjectedKey>,
    caps_lock: bool,
    private_tag: i64,
}

impl KeyboardState {
    pub(super) fn synchronize_private_tag(&mut self, tag: i64) {
        if tag != self.private_tag {
            // Each guarded session starts without inheriting local lock state.
            // RDP's existing synchronization then applies the client's Caps state.
            if tag != 0 {
                self.caps_lock = false;
            }
            self.private_tag = tag;
        }
    }

    pub(super) fn modifiers(&self) -> RemoteModifiers {
        RemoteModifiers::from_keys(
            |code| self.held.contains(&InjectedKey::Physical(code)),
            self.caps_lock,
        )
    }

    fn modifiers_after(&self, key: InjectedKey, pressed: bool) -> RemoteModifiers {
        let caps_lock = self.caps_lock
            ^ (key == InjectedKey::Physical(0x39) && pressed && !self.held.contains(&key));
        RemoteModifiers::from_keys(
            |code| {
                let modifier = InjectedKey::Physical(code);
                if modifier == key {
                    pressed
                } else {
                    self.held.contains(&modifier)
                }
            },
            caps_lock,
        )
    }

    pub(super) fn inject(
        &mut self,
        key: InjectedKey,
        pressed: bool,
        mut post: impl FnMut(InjectedKey, bool, RemoteModifiers) -> Result<()>,
    ) -> Result<()> {
        if !pressed && !self.held.contains(&key) {
            return Ok(());
        }
        let modifiers = self.modifiers_after(key, pressed);
        post(key, pressed, modifiers)?;
        self.caps_lock = modifiers
            .flags
            .contains(CGEventFlags::CGEventFlagAlphaShift);
        if pressed {
            self.held.insert(key);
        } else {
            self.held.remove(&key);
        }
        Ok(())
    }

    pub(super) fn release_matching(
        &mut self,
        include: impl Fn(InjectedKey) -> bool,
        mut post: impl FnMut(InjectedKey, bool, RemoteModifiers) -> Result<()>,
    ) -> Result<()> {
        let mut keys: Vec<_> = self
            .held
            .iter()
            .copied()
            .filter(|key| include(*key))
            .collect();
        // End ordinary keys before modifiers so cleanup cannot turn a held
        // shortcut into an unintended unmodified key action.
        keys.sort_by_key(|key| key.is_modifier());
        let mut first_error = None;
        for key in keys {
            if let Err(error) = self.inject(key, false, &mut post) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests;
