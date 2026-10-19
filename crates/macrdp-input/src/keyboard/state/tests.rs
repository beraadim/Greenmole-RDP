use super::*;

#[test]
fn private_flags_follow_only_remote_owned_modifier_transitions() {
    let mut state = KeyboardState::default();
    assert_eq!(state.modifiers().flags, CGEventFlags::empty());
    for code in [0x38, 0x3C, 0x37, 0x36, 0x3B, 0x3E, 0x3A, 0x3D, 0x3F] {
        state
            .inject(InjectedKey::Physical(code), true, |_, _, snapshot| {
                assert!(!snapshot.flags.is_empty());
                Ok(())
            })
            .unwrap();
    }
    let flags = state.modifiers().flags;
    for aggregate in [
        CGEventFlags::CGEventFlagShift,
        CGEventFlags::CGEventFlagCommand,
        CGEventFlags::CGEventFlagControl,
        CGEventFlags::CGEventFlagAlternate,
        CGEventFlags::CGEventFlagSecondaryFn,
    ] {
        assert!(flags.contains(aggregate));
    }
    assert!(state.modifiers().left_command && state.modifiers().right_command);
    state
        .inject(InjectedKey::Physical(0x37), false, |_, _, snapshot| {
            assert!(!snapshot.left_command);
            assert!(snapshot.right_command);
            assert!(snapshot.flags.contains(CGEventFlags::CGEventFlagCommand));
            assert_eq!(snapshot.flags.bits() & 0x8, 0);
            Ok(())
        })
        .unwrap();
    state.release_matching(|_| true, |_, _, _| Ok(())).unwrap();
    assert_eq!(state.modifiers().flags, CGEventFlags::empty());
}

#[test]
fn private_caps_lock_commits_only_successful_first_presses_and_resets_each_session() {
    let mut state = KeyboardState::default();
    let caps = InjectedKey::Physical(0x39);
    state.synchronize_private_tag(1);
    assert!(state
        .inject(caps, true, |_, _, _| anyhow::bail!("allocation failed"))
        .is_err());
    assert_eq!(state.modifiers().flags, CGEventFlags::empty());
    for _ in 0..2 {
        state
            .inject(caps, true, |_, _, snapshot| {
                assert!(snapshot.flags.contains(CGEventFlags::CGEventFlagAlphaShift));
                Ok(())
            })
            .unwrap();
    }
    state.release_matching(|_| true, |_, _, _| Ok(())).unwrap();
    assert!(state
        .modifiers()
        .flags
        .contains(CGEventFlags::CGEventFlagAlphaShift));
    state.synchronize_private_tag(1);
    assert!(state
        .modifiers()
        .flags
        .contains(CGEventFlags::CGEventFlagAlphaShift));
    state.synchronize_private_tag(2);
    assert_eq!(state.modifiers().flags, CGEventFlags::empty());
    state.inject(caps, true, |_, _, _| Ok(())).unwrap();
    state.inject(caps, false, |_, _, _| Ok(())).unwrap();
    state
        .inject(caps, true, |_, _, snapshot| {
            assert_eq!(snapshot.flags, CGEventFlags::empty());
            Ok(())
        })
        .unwrap();
}

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
            .inject(key, true, |key, down, _| {
                events.push((key, down));
                Ok(())
            })
            .unwrap();
    }
    state
        .inject(InjectedKey::Physical(0x37), false, |_, _, _| {
            panic!("unowned local key released")
        })
        .unwrap();
    assert!(state
        .inject(InjectedKey::Physical(1), true, |_, _, _| anyhow::bail!(
            "post failed"
        ))
        .is_err());
    events.clear();
    state
        .release_matching(
            |_| true,
            |key, down, _| {
                events.push((key, down));
                Ok(())
            },
        )
        .unwrap();
    state
        .release_matching(|_| true, |_, _, _| panic!("duplicate cleanup"))
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
        state.inject(key, true, |_, _, _| Ok(())).unwrap();
    }
    assert!(state
        .release_matching(
            |_| true,
            |key, _, _| {
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
            |key, _, _| {
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
            .inject(InjectedKey::Physical(code), true, |_, _, _| Ok(()))
            .unwrap();
    }
    let mut released = Vec::new();
    state
        .release_matching(InjectedKey::is_modifier, |key, _, _| {
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
        state.inject(key, true, |_, _, _| Ok(())).unwrap();
    }
    let mut events = Vec::new();
    state
        .release_matching(
            |_| true,
            |key, pressed, _| {
                events.push((key, pressed));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(events, [(shortcut, false), (ctrl, false)]);
    state
        .release_matching(|_| true, |_, _, _| panic!("duplicate release"))
        .unwrap();
}
