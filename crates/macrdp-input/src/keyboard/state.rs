use std::collections::BTreeSet;

use anyhow::Result;

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
}

impl KeyboardState {
    pub(super) fn inject(
        &mut self,
        key: InjectedKey,
        pressed: bool,
        mut post: impl FnMut(InjectedKey, bool) -> Result<()>,
    ) -> Result<()> {
        if !pressed && !self.held.contains(&key) {
            return Ok(());
        }
        post(key, pressed)?;
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
        mut post: impl FnMut(InjectedKey, bool) -> Result<()>,
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
mod tests {
    use super::*;

    #[test]
    fn cleanup_releases_only_successful_remote_presses_including_unicode_and_repeats() {
        let mut state = KeyboardState::default();
        let keys = [
            InjectedKey::Physical(0x38),
            InjectedKey::Physical(0),
            InjectedKey::Unicode(0xE9),
        ];
        let mut events = Vec::new();
        for key in keys.into_iter().chain([keys[1]]) {
            state
                .inject(key, true, |key, down| {
                    events.push((key, down));
                    Ok(())
                })
                .unwrap();
        }
        state
            .inject(InjectedKey::Physical(0x37), false, |_, _| {
                panic!("unowned local key released")
            })
            .unwrap();
        assert!(state
            .inject(InjectedKey::Physical(1), true, |_, _| anyhow::bail!(
                "post failed"
            ))
            .is_err());
        events.clear();
        state
            .release_matching(
                |_| true,
                |key, down| {
                    events.push((key, down));
                    Ok(())
                },
            )
            .unwrap();
        state
            .release_matching(|_| true, |_, _| panic!("duplicate cleanup"))
            .unwrap();
        assert_eq!(
            events,
            [(keys[1], false), (keys[2], false), (keys[0], false)]
        );
    }

    #[test]
    fn failed_release_keeps_ownership_and_does_not_prevent_other_cleanup() {
        let mut state = KeyboardState::default();
        let a = InjectedKey::Physical(0);
        let shift = InjectedKey::Physical(0x38);
        for key in [a, shift] {
            state.inject(key, true, |_, _| Ok(())).unwrap();
        }
        assert!(state
            .release_matching(
                |_| true,
                |key, _| {
                    if key == a {
                        anyhow::bail!("simulated allocation failure");
                    }
                    Ok(())
                }
            )
            .is_err());
        let mut retried = Vec::new();
        state
            .release_matching(
                |_| true,
                |key, _| {
                    retried.push(key);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(retried, [a]);
    }

    #[test]
    fn modifier_cleanup_preserves_regular_keys_and_caps_lock() {
        let mut state = KeyboardState::default();
        for code in [0, 0x39, 0x36, 0x3D] {
            state
                .inject(InjectedKey::Physical(code), true, |_, _| Ok(()))
                .unwrap();
        }
        let mut released = Vec::new();
        state
            .release_matching(InjectedKey::is_modifier, |key, _| {
                released.push(key);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            released,
            [InjectedKey::Physical(0x36), InjectedKey::Physical(0x3D)]
        );
        assert_eq!(
            state.held,
            BTreeSet::from([InjectedKey::Physical(0), InjectedKey::Physical(0x39)])
        );
    }

    #[test]
    fn shortcut_cleanup_retains_event_flags_and_precedes_ctrl_release() {
        let mut state = KeyboardState::default();
        let ctrl = InjectedKey::Physical(0x3B);
        let shortcut = InjectedKey::CommandShortcut {
            keycode: 0x08,
            shifted: true,
        };
        for key in [ctrl, shortcut, shortcut] {
            state.inject(key, true, |_, _| Ok(())).unwrap();
        }
        let mut events = Vec::new();
        state
            .release_matching(
                |_| true,
                |key, pressed| {
                    events.push((key, pressed));
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(events, [(shortcut, false), (ctrl, false)]);
        state
            .release_matching(|_| true, |_, _| panic!("duplicate release"))
            .unwrap();
    }
}
