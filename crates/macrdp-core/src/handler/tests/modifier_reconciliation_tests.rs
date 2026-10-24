use super::*;

fn press(handler: &mut MacInputHandler, code: u8, extended: bool) {
    handler.keyboard(KeyboardEvent::Pressed { code, extended });
}

fn release(handler: &mut MacInputHandler, code: u8, extended: bool) {
    handler.keyboard(KeyboardEvent::Released { code, extended });
}

fn recorded(policy: &str) -> (MacInputHandler, Arc<Mutex<Vec<KeyAction>>>) {
    let mut handler = handler_for("Guacamole RDP", 0x0409);
    handler.keyboard_shortcuts = policy.into();
    let events = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard::new(events.clone())));
    (handler, events)
}

#[test]
fn held_shift_is_restored_once_before_navigation_after_translated_text() {
    for policy in ["windows", "mac", "physical"] {
        for (scan, native) in [(0x2A, 0x38), (0x36, 0x3C)] {
            let (mut handler, events) = recorded(policy);
            press(&mut handler, scan, false);
            press(&mut handler, 0x1E, false);
            release(&mut handler, 0x1E, false);
            assert!(events
                .lock()
                .unwrap()
                .contains(&KeyAction::Unicode('A' as u16, true)));
            events.lock().unwrap().clear();
            press(&mut handler, 0x4D, true);
            press(&mut handler, 0x4D, true); // Normal arrow repeat is preserved.
            release(&mut handler, 0x4D, true);
            release(&mut handler, scan, false);
            assert_eq!(
                *events.lock().unwrap(),
                vec![
                    KeyAction::Key(native, true),
                    KeyAction::Key(0x7C, true),
                    KeyAction::Key(0x7C, true),
                    KeyAction::Key(0x7C, false),
                    KeyAction::Key(native, false),
                ],
                "policy={policy}"
            );
            assert!(handler.mapped_key_releases.is_empty());
        }
    }
}

#[test]
fn held_shift_survives_transition_from_text_to_mac_command_chord() {
    let (mut handler, events) = recorded("mac");
    press(&mut handler, 0x2A, false);
    press(&mut handler, 0x1E, false);
    release(&mut handler, 0x1E, false);
    events.lock().unwrap().clear();
    press(&mut handler, 0x5B, true);
    press(&mut handler, 0x2E, false);
    release(&mut handler, 0x2E, false);
    release(&mut handler, 0x5B, true);
    release(&mut handler, 0x2A, false);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            KeyAction::Key(0x38, true),
            KeyAction::Key(0x37, true),
            KeyAction::Key(0x08, true),
            KeyAction::Key(0x08, false),
            KeyAction::Key(0x37, false),
            KeyAction::Key(0x38, false),
        ]
    );
}

#[test]
fn consumed_altgr_and_its_control_companion_are_not_restored_as_native_modifiers() {
    for (policy, subtype) in [
        ("windows", 0),
        ("mac", 0x474D0102),
        ("physical", 0x474D0103),
    ] {
        let (mut handler, events) = recorded(policy);
        handler.client_context.keyboard_layout = 0x0414;
        handler.client_context.keyboard_subtype = subtype;
        press(&mut handler, 0x1D, false);
        press(&mut handler, 0x38, true);
        press(&mut handler, 0x03, false);
        release(&mut handler, 0x03, false);
        events.lock().unwrap().clear();
        press(&mut handler, 0x4D, true);
        release(&mut handler, 0x4D, true);
        assert_eq!(
            *events.lock().unwrap(),
            vec![KeyAction::Key(0x7C, true), KeyAction::Key(0x7C, false)],
            "policy={policy}"
        );
    }
}
