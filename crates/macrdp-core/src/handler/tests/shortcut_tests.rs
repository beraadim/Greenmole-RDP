use super::*;

fn recording_handler(name: &str, layout: u32) -> (MacInputHandler, Arc<Mutex<Vec<KeyAction>>>) {
    let mut handler = handler_for(name, layout);
    let events = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard(Arc::clone(&events))));
    (handler, events)
}

fn key(handler: &mut MacInputHandler, code: u8, extended: bool, pressed: bool) {
    handler.keyboard(if pressed {
        KeyboardEvent::Pressed { code, extended }
    } else {
        KeyboardEvent::Released { code, extended }
    });
}

#[test]
fn requested_shortcuts_use_command_for_both_ctrl_keys_and_clients() {
    for (name, layout) in [
        ("Windows Remote Desktop", 0x0414),
        ("Guacamole RDP", 0x0409),
        ("Guacamole RDP", 0x0414),
        ("Windows Remote Desktop", 0x0409),
    ] {
        for controls in [&[false][..], &[true][..], &[false, true][..]] {
            for shifted in [false, true] {
                for (code, extended, native) in [
                    (0x1E, false, 0x00),
                    (0x2D, false, 0x07),
                    (0x2E, false, 0x08),
                    (0x2F, false, 0x09),
                    (0x1C, false, 0x24),
                    (0x1C, true, 0x4C),
                ] {
                    let (mut handler, events) = recording_handler(name, layout);
                    if shifted {
                        key(&mut handler, 0x2A, false, true);
                    }
                    for extended in controls {
                        key(&mut handler, 0x1D, *extended, true);
                    }
                    events.lock().unwrap().clear();
                    key(&mut handler, code, extended, true);
                    key(&mut handler, code, extended, false);
                    assert_eq!(
                        *events.lock().unwrap(),
                        [
                            KeyAction::Command(native, true, shifted),
                            KeyAction::Command(native, false, shifted)
                        ],
                        "{name}, layout {layout:#x}, controls {controls:?}"
                    );
                    assert!(handler.command_shortcuts.is_empty());
                    assert!(handler.translated_key_releases.is_empty());
                }
            }
        }
    }
}

#[test]
fn shortcut_repeat_and_release_keep_the_original_mapping_after_ctrl_release() {
    for ctrl_extended in [false, true] {
        for (first, first_native, second, second_native) in
            [(0x2E, 0x08, 0x2F, 0x09), (0x2D, 0x07, 0x1E, 0x00)]
        {
            let (mut handler, events) = recording_handler("Windows Remote Desktop", 0x0414);
            key(&mut handler, 0x1D, ctrl_extended, true);
            key(&mut handler, first, false, true);
            key(&mut handler, first, false, true);
            key(&mut handler, second, false, true);
            key(&mut handler, 0x1D, ctrl_extended, false);
            events.lock().unwrap().clear();
            // Even a layout refresh cannot reinterpret an outstanding key release.
            handler.update_client_context(ClientInputContext::default());
            key(&mut handler, first, false, true);
            key(&mut handler, second, false, false);
            key(&mut handler, first, false, false);
            assert_eq!(
                *events.lock().unwrap(),
                [
                    KeyAction::Command(first_native, true, false),
                    KeyAction::Command(second_native, false, false),
                    KeyAction::Command(first_native, false, false),
                ]
            );
            assert!(handler.command_shortcuts.is_empty());
        }
    }
}

#[test]
fn plain_text_and_other_control_combinations_keep_existing_behavior() {
    let (mut handler, events) = recording_handler("Windows Remote Desktop", 0x0414);
    for code in [0x1E, 0x2D, 0x2E, 0x2F] {
        key(&mut handler, code, false, true);
        key(&mut handler, code, false, false);
    }
    key(&mut handler, 0x1C, false, true);
    key(&mut handler, 0x1C, false, false);
    assert!(events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(b'c' as u16, true)));
    assert!(events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(b'v' as u16, true)));
    assert!(events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(b'a' as u16, true)));
    assert!(events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(b'x' as u16, true)));
    assert!(events.lock().unwrap().contains(&KeyAction::Key(0x24, true)));
    key(&mut handler, 0x1D, false, true);
    for (code, native) in [(0x1F, 0x01), (0x2C, 0x06), (0x0F, 0x30), (0x01, 0x35)] {
        key(&mut handler, code, false, true);
        key(&mut handler, code, false, false);
        assert!(events
            .lock()
            .unwrap()
            .contains(&KeyAction::Key(native, true)));
    }
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|event| matches!(event, KeyAction::Command(..))));
}

#[test]
fn altgr_and_extra_modifiers_do_not_become_command_shortcuts() {
    for (name, layout) in [
        ("Windows Remote Desktop", 0x0414),
        ("Guacamole RDP", 0x0409),
        ("Guacamole RDP", 0x0414),
        ("Windows Remote Desktop", 0x0409),
    ] {
        for ctrl_extended in [false, true] {
            for modifier_first in [false, true] {
                for modifier in [(0x38, true), (0x38, false), (0x5B, true), (0x5C, true)] {
                    let (mut handler, events) = recording_handler(name, layout);
                    let ctrl = (0x1D, ctrl_extended);
                    let order = if modifier_first {
                        [modifier, ctrl]
                    } else {
                        [ctrl, modifier]
                    };
                    for (code, extended) in order {
                        key(&mut handler, code, extended, true);
                    }
                    for code in [0x1E, 0x2D, 0x2E, 0x2F, 0x1C] {
                        key(&mut handler, code, false, true);
                        key(&mut handler, code, false, false);
                    }
                    assert!(!events
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|event| matches!(event, KeyAction::Command(..))));
                    if name == "Windows Remote Desktop"
                        && layout == 0x0414
                        && modifier == (0x38, true)
                    {
                        key(&mut handler, 0x03, false, true);
                        assert!(events
                            .lock()
                            .unwrap()
                            .contains(&KeyAction::Unicode(b'@' as u16, true)));
                    }
                }
            }
        }
    }
}

#[test]
fn editing_shortcuts_while_holding_ctrl_do_not_leak_into_following_text() {
    let (mut handler, events) = recording_handler("Windows Remote Desktop", 0x0414);
    handler.pending_dead_key = Some('^');
    key(&mut handler, 0x1D, false, true);
    for code in [0x1E, 0x2D, 0x2E, 0x2F] {
        key(&mut handler, code, false, true);
        key(&mut handler, code, false, false);
        assert!(handler.modifier_state.left_ctrl);
    }
    key(&mut handler, 0x1D, false, false);
    key(&mut handler, 0x1E, false, true);
    key(&mut handler, 0x1E, false, false);
    assert!(handler.pending_dead_key.is_none());
    assert!(handler.command_shortcuts.is_empty());
    assert!(events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(b'a' as u16, true)));
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, KeyAction::Command(..)))
            .count(),
        8
    );
    assert_eq!(
        handler
            .debug_state
            .lock()
            .unwrap()
            .last_translation
            .as_deref(),
        Some("a")
    );
}

#[test]
fn synchronize_disconnect_and_reconnect_clear_held_shortcuts() {
    for (code, text) in [(0x2E, 'c'), (0x2D, 'x'), (0x1E, 'a')] {
        for reset in 0..3 {
            let (mut handler, events) = recording_handler("Windows Remote Desktop", 0x0414);
            key(&mut handler, 0x1D, false, true);
            key(&mut handler, code, false, true);
            assert_eq!(handler.command_shortcuts.len(), 1);
            match reset {
                0 => handler.keyboard(KeyboardEvent::Synchronize(
                    ironrdp_server::SynchronizeFlags::empty(),
                )),
                1 => handler.on_disconnect(),
                _ => handler.on_connect(
                    ClientInputContext {
                        client_name: "Guacamole RDP".into(),
                        keyboard_layout: 0x0409,
                    },
                    None,
                ),
            }
            assert_eq!(events.lock().unwrap().last(), Some(&KeyAction::ReleaseAll));
            assert!(handler.command_shortcuts.is_empty());
            events.lock().unwrap().clear();
            key(&mut handler, code, false, true);
            key(&mut handler, code, false, false);
            assert!(events
                .lock()
                .unwrap()
                .contains(&KeyAction::Unicode(text as u16, true)));
            assert!(!events
                .lock()
                .unwrap()
                .iter()
                .any(|event| matches!(event, KeyAction::Command(..))));
        }
    }
}

#[test]
fn pressing_ctrl_after_text_started_does_not_turn_a_repeat_into_cut_or_select_all() {
    for (code, text) in [(0x2D, 'x'), (0x1E, 'a')] {
        let (mut handler, events) = recording_handler("Windows Remote Desktop", 0x0414);
        key(&mut handler, code, false, true);
        key(&mut handler, 0x1D, false, true);
        events.lock().unwrap().clear();
        key(&mut handler, code, false, true);
        key(&mut handler, code, false, false);
        assert!(handler.command_shortcuts.is_empty());
        assert!(handler.translated_key_releases.is_empty());
        assert!(events
            .lock()
            .unwrap()
            .contains(&KeyAction::Unicode(text as u16, false)));
        assert!(!events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, KeyAction::Command(..))));
    }
}

struct FailingKeyboard {
    recording: RecordingKeyboard,
    fail_command: Arc<Mutex<bool>>,
}

impl KeyboardDevice for FailingKeyboard {
    fn inject_command_keycode(&self, key: u16, pressed: bool, shifted: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            !*self.fail_command.lock().unwrap(),
            "simulated event allocation failure"
        );
        self.recording.inject_command_keycode(key, pressed, shifted)
    }
    fn inject_keycode(&self, key: u16, pressed: bool) -> anyhow::Result<()> {
        self.recording.inject_keycode(key, pressed)
    }
    fn inject_unicode(&self, ch: u16, pressed: bool) -> anyhow::Result<()> {
        self.recording.inject_unicode(ch, pressed)
    }
    fn release_transient_modifiers(&self) -> anyhow::Result<()> {
        self.recording.release_transient_modifiers()
    }
    fn release_all(&self) -> anyhow::Result<()> {
        self.recording.release_all()
    }
    fn current_modifier_flags(&self) -> (Vec<String>, u64) {
        self.recording.current_modifier_flags()
    }
}

#[test]
fn failed_shortcut_press_never_falls_back_to_text_and_failed_release_can_retry() {
    let (mut handler, events) = recording_handler("Windows Remote Desktop", 0x0414);
    let fail_command = Arc::new(Mutex::new(true));
    handler.keyboard = Some(Box::new(FailingKeyboard {
        recording: RecordingKeyboard(Arc::clone(&events)),
        fail_command: Arc::clone(&fail_command),
    }));
    key(&mut handler, 0x1D, false, true);
    events.lock().unwrap().clear();
    key(&mut handler, 0x2E, false, true);
    assert!(handler.command_shortcuts.is_empty());
    assert!(events.lock().unwrap().is_empty());
    *fail_command.lock().unwrap() = false;
    key(&mut handler, 0x2E, false, true);
    *fail_command.lock().unwrap() = true;
    key(&mut handler, 0x2E, false, false);
    assert_eq!(handler.command_shortcuts.len(), 1);
    *fail_command.lock().unwrap() = false;
    key(&mut handler, 0x2E, false, false);
    assert!(handler.command_shortcuts.is_empty());
    assert_eq!(
        *events.lock().unwrap(),
        [
            KeyAction::Command(0x08, true, false),
            KeyAction::Command(0x08, false, false)
        ]
    );
}
