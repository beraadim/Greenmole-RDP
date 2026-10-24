use super::*;

fn recorded(layout: u32, policy: &str) -> (MacInputHandler, Arc<Mutex<Vec<KeyAction>>>) {
    let mut handler = handler_for("Guacamole RDP", layout);
    handler.keyboard_shortcuts = policy.into();
    let events = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard::new(events.clone())));
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
fn versioned_client_preference_is_narrow_and_explicit_server_policy_wins() {
    let mut handler = handler_for("Arbitrary client", 0x0409);
    for (subtype, expected) in [
        (0, "windows"),
        (0x474D0101, "windows"),
        (0x474D0102, "mac"),
        (0x474D0103, "physical"),
        (0x474D0202, "windows"),
        (0x474D0104, "windows"),
        (u32::MAX, "windows"),
    ] {
        handler.client_context.keyboard_subtype = subtype;
        handler.keyboard_shortcuts = "auto".into();
        assert_eq!(handler.shortcut_policy(), expected);
        for explicit in ["windows", "mac", "physical"] {
            handler.keyboard_shortcuts = explicit.into();
            assert_eq!(handler.shortcut_policy(), explicit);
        }
    }
}

#[test]
fn mac_and_physical_policies_keep_terminal_control_chords() {
    for policy in ["mac", "physical"] {
        for layout in [0x0409, 0x0414, 0x040C] {
            for extended in [false, true] {
                let (mut handler, events) = recorded(layout, policy);
                key(&mut handler, 0x1D, extended, true);
                key(&mut handler, 0x2E, false, true);
                key(&mut handler, 0x2E, false, false);
                key(&mut handler, 0x1D, extended, false);
                assert_eq!(
                    *events.lock().unwrap(),
                    vec![
                        KeyAction::Key(if extended { 0x3E } else { 0x3B }, true),
                        KeyAction::Key(0x08, true),
                        KeyAction::Key(0x08, false),
                        KeyAction::Key(if extended { 0x3E } else { 0x3B }, false),
                    ],
                    "policy={policy} layout={layout:#x} rightCtrl={extended}"
                );
            }
        }
    }
}

#[test]
fn mac_command_and_option_have_native_meanings() {
    for (scan, extended, expected) in [
        (0x5B, true, 0x37),
        (0x5C, true, 0x36),
        (0x38, false, 0x3A),
        (0x38, true, 0x3D),
    ] {
        let (mut handler, events) = recorded(0x0409, "mac");
        key(&mut handler, scan, extended, true);
        key(&mut handler, 0x2E, false, true);
        key(&mut handler, 0x2E, false, false);
        key(&mut handler, scan, extended, false);
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                KeyAction::Key(expected, true),
                KeyAction::Key(0x08, true),
                KeyAction::Key(0x08, false),
                KeyAction::Key(expected, false),
            ],
            "scan={scan:#x} extended={extended}"
        );
    }
}

#[test]
fn french_ctrl_a_is_an_editing_shortcut_but_french_ctrl_q_is_not() {
    let (mut handler, events) = recorded(0x040C, "windows");
    key(&mut handler, 0x1D, false, true);
    events.lock().unwrap().clear();
    key(&mut handler, 0x10, false, true); // AZERTY A
    key(&mut handler, 0x10, false, false);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            KeyAction::Command(0x00, true, false),
            KeyAction::Command(0x00, false, false)
        ]
    );
    events.lock().unwrap().clear();
    key(&mut handler, 0x1E, false, true); // AZERTY Q
    key(&mut handler, 0x1E, false, false);
    assert_eq!(
        *events.lock().unwrap(),
        vec![KeyAction::Key(0x0C, true), KeyAction::Key(0x0C, false)]
    );
}

struct AzertyHost(RecordingKeyboard);
impl KeyboardDevice for AzertyHost {
    fn keycode_for_character(&self, ch: char) -> Option<u16> {
        match ch {
            'a' => Some(0x0C),
            'q' => Some(0x00),
            _ => self.0.keycode_for_character(ch),
        }
    }
    fn inject_keycode(&self, keycode: u16, pressed: bool) -> anyhow::Result<()> {
        self.0.inject_keycode(keycode, pressed)
    }
    fn ensure_keycode_pressed(&self, keycode: u16) -> anyhow::Result<()> {
        self.0.ensure_keycode_pressed(keycode)
    }
    fn inject_command_keycode(
        &self,
        keycode: u16,
        pressed: bool,
        shifted: bool,
    ) -> anyhow::Result<()> {
        self.0.inject_command_keycode(keycode, pressed, shifted)
    }
    fn inject_unicode(&self, ch: u16, pressed: bool) -> anyhow::Result<()> {
        self.0.inject_unicode(ch, pressed)
    }
    fn release_transient_modifiers(&self) -> anyhow::Result<()> {
        self.0.release_transient_modifiers()
    }
    fn release_all(&self) -> anyhow::Result<()> {
        self.0.release_all()
    }
    fn current_modifier_flags(&self) -> (Vec<String>, u64) {
        self.0.current_modifier_flags()
    }
}

#[test]
fn editing_shortcut_resolves_the_host_layout_and_keeps_its_release() {
    let (mut handler, events) = recorded(0x0409, "windows");
    handler.keyboard = Some(Box::new(AzertyHost(RecordingKeyboard::new(events.clone()))));
    key(&mut handler, 0x1D, false, true);
    events.lock().unwrap().clear();
    key(&mut handler, 0x1E, false, true); // US A -> French host's A position
    handler.keyboard_shortcuts = "mac".into();
    key(&mut handler, 0x1E, false, false);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            KeyAction::Command(0x0C, true, false),
            KeyAction::Command(0x0C, false, false)
        ]
    );
}

#[test]
fn native_chord_repeats_and_key_up_retain_original_mapping_after_context_change() {
    let (mut handler, events) = recorded(0x040C, "auto");
    handler.client_context.keyboard_subtype = 0x474D0102;
    key(&mut handler, 0x5B, true, true);
    key(&mut handler, 0x10, false, true); // French Command+A
    handler.update_client_context(ClientInputContext {
        client_name: "new context".into(),
        keyboard_layout: 0x0409,
        keyboard_subtype: 0,
    });
    key(&mut handler, 0x5B, true, false);
    key(&mut handler, 0x10, false, true); // repeat with Command already released
    key(&mut handler, 0x10, false, false);
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            KeyAction::Key(0x37, true),
            KeyAction::Key(0x00, true),
            KeyAction::Key(0x37, false),
            KeyAction::Key(0x00, true),
            KeyAction::Key(0x00, false),
        ]
    );
    assert!(handler.mapped_key_releases.is_empty());
}

#[test]
fn native_option_release_survives_a_context_change_to_windows_translation() {
    let (mut handler, events) = recorded(0x0409, "auto");
    handler.client_context.keyboard_subtype = 0x474D0102;
    key(&mut handler, 0x38, true, true);
    handler.update_client_context(ClientInputContext {
        client_name: "older client".into(),
        keyboard_layout: 0x0409,
        keyboard_subtype: 0,
    });
    key(&mut handler, 0x38, true, false);
    assert_eq!(
        *events.lock().unwrap(),
        vec![KeyAction::Key(0x3D, true), KeyAction::Key(0x3D, false)]
    );
    assert!(handler.mapped_key_releases.is_empty());
}

#[test]
fn held_control_q_repeat_never_changes_to_select_all_after_layout_refresh() {
    let (mut handler, events) = recorded(0x0409, "windows");
    key(&mut handler, 0x1D, false, true);
    key(&mut handler, 0x10, false, true); // US Q
    events.lock().unwrap().clear();
    handler.update_client_context(ClientInputContext {
        client_name: "layout refresh".into(),
        keyboard_layout: 0x040C,
        keyboard_subtype: 0,
    });
    key(&mut handler, 0x10, false, true); // French A position, still held US Q
    key(&mut handler, 0x10, false, false);
    assert_eq!(
        *events.lock().unwrap(),
        vec![KeyAction::Key(0x0C, true), KeyAction::Key(0x0C, false)]
    );
    assert!(handler.command_shortcuts.is_empty());
}

#[test]
fn norwegian_native_mac_option_and_browser_altgr_are_distinct() {
    let (mut native, native_events) = recorded(0x0414, "mac");
    key(&mut native, 0x38, true, true);
    key(&mut native, 0x03, false, true);
    key(&mut native, 0x03, false, false);
    key(&mut native, 0x38, true, false);
    assert_eq!(
        *native_events.lock().unwrap(),
        vec![
            KeyAction::Key(0x3D, true),
            KeyAction::Key(0x13, true),
            KeyAction::Key(0x13, false),
            KeyAction::Key(0x3D, false)
        ]
    );

    let (mut browser, browser_events) = recorded(0x0414, "auto");
    browser.client_context.keyboard_subtype = 0x474D0102;
    key(&mut browser, 0x38, true, true);
    key(&mut browser, 0x03, false, true);
    key(&mut browser, 0x03, false, false);
    key(&mut browser, 0x38, true, false);
    let unicode: Vec<_> = browser_events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| {
            if let KeyAction::Unicode(ch, pressed) = event {
                Some((*ch, *pressed))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(unicode, vec![('@' as u16, true), ('@' as u16, false)]);
    assert!(!browser_events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, KeyAction::Key(0x3D, _))));
}

#[test]
fn reconnect_and_synchronize_clear_session_key_mappings() {
    for reconnect in [false, true] {
        let (mut handler, events) = recorded(0x0409, "mac");
        key(&mut handler, 0x5B, true, true);
        key(&mut handler, 0x2E, false, true);
        assert_eq!(handler.mapped_key_releases.len(), 2);
        events.lock().unwrap().clear();
        if reconnect {
            handler.on_connect(ClientInputContext::default(), None);
        } else {
            handler.keyboard(KeyboardEvent::Synchronize(
                ironrdp_server::SynchronizeFlags::empty(),
            ));
        }
        assert!(handler.mapped_key_releases.is_empty());
        assert!(events.lock().unwrap().contains(&KeyAction::ReleaseAll));
    }
}

#[test]
fn undefined_altgr_printable_chords_and_dead_key_release_never_inject_physical_text() {
    for (layout, code, altgr) in [
        (0x0414, 0x1E, true),
        (0x0407, 0x20, true),
        (0x041D, 0x0D, false),
    ] {
        let (mut handler, events) = recorded(layout, "windows");
        if altgr {
            key(&mut handler, 0x38, true, true);
        }
        events.lock().unwrap().clear();
        key(&mut handler, code, false, true);
        key(&mut handler, code, false, false);
        assert!(
            !events.lock().unwrap().iter().any(|event| matches!(
                event,
                KeyAction::Key(_, _) | KeyAction::Unicode(_, _) | KeyAction::Command(_, _, _)
            )),
            "layout={layout:#x}"
        );
        assert!(handler.translated_key_releases.is_empty());
    }
}

#[test]
fn french_altgr_composes_but_german_tilde_is_immediate_text() {
    for (layout, accent_scan, base_scan, expected) in [
        (0x040C, 0x03, 0x31, "ñ"),
        (0x040C, 0x08, 0x12, "è"),
        (0x040C, 0x03, 0x39, "~"),
        (0x040C, 0x08, 0x39, "`"),
        (0x0407, 0x1B, 0x31, "~n"),
    ] {
        let (mut handler, events) = recorded(layout, "windows");
        key(&mut handler, 0x38, true, true);
        key(&mut handler, accent_scan, false, true);
        key(&mut handler, accent_scan, false, false);
        key(&mut handler, 0x38, true, false);
        key(&mut handler, base_scan, false, true);
        key(&mut handler, base_scan, false, false);
        let actual: String = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                KeyAction::Unicode(ch, true) => char::from_u32(u32::from(*ch)),
                _ => None,
            })
            .collect();
        assert_eq!(
            actual, expected,
            "layout={layout:#x} accent={accent_scan:#x}"
        );
        assert!(handler.translated_key_releases.is_empty());
        assert!(handler.mapped_key_releases.is_empty());
    }
}

#[test]
fn french_shift_superscript_two_has_no_text_or_host_key_fallback() {
    let (mut handler, events) = recorded(0x040C, "windows");
    key(&mut handler, 0x2A, false, true);
    events.lock().unwrap().clear();
    key(&mut handler, 0x29, false, true);
    key(&mut handler, 0x29, false, false);
    assert!(!events.lock().unwrap().iter().any(|event| matches!(
        event,
        KeyAction::Key(_, _) | KeyAction::Unicode(_, _) | KeyAction::Command(_, _, _)
    )));
    assert!(handler.translated_key_releases.is_empty());
}
