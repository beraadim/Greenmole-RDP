use super::keyboard_device::KeyboardDevice;
use super::{
    ClientInputContext, InputDebugState, KeyboardTranslationProfile, MacInputHandler,
    ModifierState, MouseCoordMapper, SuppressedCtrlState, TextTranslation,
};
use ironrdp_server::{KeyboardEvent, RdpServerInputHandler};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod shortcut_tests;

#[test]
fn mouse_mapper_scales_linearly_across_resized_desktop() {
    let mapper = MouseCoordMapper::new(1920, 1200, 2560, 1305);
    let center = mapper.map(1280, 652);
    assert!((center.0 - 960.0).abs() < 1e-8);
    assert!((center.1 - 599.5402298850574).abs() < 1e-8);
    let corner = mapper.map(2559, 1304);
    assert!((corner.0 - 1919.25).abs() < 1e-8);
    assert!((corner.1 - 1199.080459770115).abs() < 1e-8);
}

#[test]
fn mouse_mapper_uses_reported_content_rect_for_aspect_fitted_frames() {
    let mapper = MouseCoordMapper::new(1920, 1200, 2560, 1305);
    let content = macrdp_capture::ContentRect {
        x: 236.0,
        y: 0.0,
        width: 2088.0,
        height: 1305.0,
    };
    mapper.update_frame_content_rect(2560, 1305, Some(&content));

    assert_eq!(mapper.map(236, 0), (0.0, 0.0));
    let center = mapper.map(1280, 652);
    assert!((center.0 - 960.0).abs() < 1e-8);
    assert!((center.1 - 599.5402298850574).abs() < 1e-8);
    let corner = mapper.map(2323, 1304);
    assert!((corner.0 - 1919.080459770115).abs() < 1e-8);
    assert!((corner.1 - 1199.080459770115).abs() < 1e-8);
}

fn handler_for(client_name: &str, keyboard_layout: u32) -> MacInputHandler {
    // Pure translation tests must never construct native injectors: their
    // disconnect/Drop cleanup posts modifier releases to the user's Mac.
    MacInputHandler {
        keyboard: None,
        mouse: None,
        wake: None,
        last_mouse_x: 0.0,
        last_mouse_y: 0.0,
        last_sync_lock_bits: None,
        coord_mapper: MouseCoordMapper::new(1920, 1200, 1920, 1200),
        client_context: ClientInputContext {
            client_name: client_name.to_string(),
            keyboard_layout,
        },
        modifier_state: ModifierState::default(),
        suppressed_ctrl_for_altgr: SuppressedCtrlState::default(),
        pending_dead_key: None,
        translated_key_releases: HashMap::new(),
        command_shortcuts: HashMap::new(),
        debug_state: Arc::new(Mutex::new(InputDebugState::default())),
        active_connection: Arc::new(Mutex::new(None)),
        display_controller: None,
        session_events: None,
    }
}

#[test]
fn guacamole_uses_norwegian_text_translation_for_comma_key() {
    let mut handler = handler_for("Guacamole RDP", 0x0409);
    assert_eq!(
        handler.translation_profile(),
        Some(KeyboardTranslationProfile::GuacamoleUs)
    );
    assert_eq!(
        handler.translate_text_key(0x33, false),
        Some(TextTranslation::Unicode(','))
    );
    handler.modifier_state.left_shift = true;
    assert_eq!(
        handler.translate_text_key(0x33, false),
        Some(TextTranslation::Unicode('<'))
    );
}

#[test]
fn norwegian_star_key_translates_to_expected_characters() {
    let mut handler = handler_for("Windows App", 0x0414);
    assert_eq!(
        handler.translation_profile(),
        Some(KeyboardTranslationProfile::NorwegianPhysical)
    );
    assert_eq!(
        handler.translate_text_key(0x2B, false),
        Some(TextTranslation::Unicode('\''))
    );
    handler.modifier_state.right_shift = true;
    assert_eq!(
        handler.translate_text_key(0x2B, false),
        Some(TextTranslation::Unicode('*'))
    );
}

#[test]
fn translation_is_disabled_for_shortcuts() {
    let mut handler = handler_for("Guacamole RDP", 0x0409);
    handler.modifier_state = ModifierState {
        left_ctrl: true,
        ..ModifierState::default()
    };
    assert_eq!(handler.translate_text_key(0x33, false), None);
}

#[test]
fn native_norwegian_altgr_keys_translate_to_symbols() {
    let mut handler = handler_for("Windows App", 0x0414);
    handler.modifier_state.right_alt = true;

    assert_eq!(
        handler.translate_text_key(0x03, false),
        Some(TextTranslation::Unicode('@'))
    );
    assert_eq!(
        handler.translate_text_key(0x08, false),
        Some(TextTranslation::Unicode('{'))
    );
    assert_eq!(
        handler.translate_text_key(0x09, false),
        Some(TextTranslation::Unicode('['))
    );
    assert_eq!(
        handler.translate_text_key(0x0A, false),
        Some(TextTranslation::Unicode(']'))
    );
    assert_eq!(
        handler.translate_text_key(0x0B, false),
        Some(TextTranslation::Unicode('}'))
    );
    assert_eq!(
        handler.translate_text_key(0x12, false),
        Some(TextTranslation::Unicode('€'))
    );
    assert_eq!(
        handler.translate_text_key(0x32, false),
        Some(TextTranslation::Unicode('µ'))
    );
}

#[test]
fn norwegian_letters_map_to_expected_glyphs() {
    let mut handler = handler_for("Windows App", 0x0414);
    assert_eq!(
        handler.translate_text_key(0x1A, false),
        Some(TextTranslation::Unicode('å'))
    );
    assert_eq!(
        handler.translate_text_key(0x27, false),
        Some(TextTranslation::Unicode('ø'))
    );
    assert_eq!(
        handler.translate_text_key(0x28, false),
        Some(TextTranslation::Unicode('æ'))
    );
    handler.modifier_state.left_shift = true;
    assert_eq!(
        handler.translate_text_key(0x27, false),
        Some(TextTranslation::Unicode('Ø'))
    );
}

#[test]
fn guacamole_us_profile_matches_reported_symbol_positions() {
    let mut handler = handler_for("Guacamole RDP", 0x0409);

    handler.modifier_state.left_shift = true;
    assert_eq!(
        handler.translate_text_key(0x0A, false),
        Some(TextTranslation::Unicode('('))
    );
    assert_eq!(
        handler.translate_text_key(0x0B, false),
        Some(TextTranslation::Unicode(')'))
    );
    assert_eq!(
        handler.translate_text_key(0x0D, false),
        Some(TextTranslation::Unicode('+'))
    );
    assert_eq!(
        handler.translate_text_key(0x35, false),
        Some(TextTranslation::Unicode('?'))
    );
    assert_eq!(
        handler.translate_text_key(0x28, false),
        Some(TextTranslation::Unicode('"'))
    );
    assert_eq!(
        handler.translate_text_key(0x2B, false),
        Some(TextTranslation::Unicode('|'))
    );
    assert_eq!(
        handler.translate_text_key(0x33, false),
        Some(TextTranslation::Unicode('<'))
    );

    handler.modifier_state = ModifierState::default();
    assert_eq!(
        handler.translate_text_key(0x1A, false),
        Some(TextTranslation::Unicode('['))
    );
    assert_eq!(
        handler.translate_text_key(0x1B, false),
        Some(TextTranslation::Unicode(']'))
    );
    assert_eq!(
        handler.translate_text_key(0x0C, false),
        Some(TextTranslation::Unicode('-'))
    );
    assert_eq!(
        handler.translate_text_key(0x2B, false),
        Some(TextTranslation::Unicode('\\'))
    );
    assert_eq!(
        handler.translate_text_key(0x35, false),
        Some(TextTranslation::Unicode('/'))
    );
}

#[test]
fn native_norwegian_allows_altgr_even_if_right_ctrl_is_also_set() {
    let mut handler = handler_for("Windows App", 0x0414);
    handler.modifier_state.right_alt = true;
    handler.modifier_state.right_ctrl = true;

    assert_eq!(
        handler.translate_text_key(0x09, false),
        Some(TextTranslation::Unicode('['))
    );
}

#[test]
fn native_norwegian_treats_right_ctrl_as_altgr_companion() {
    let handler = handler_for("Windows App", 0x0414);
    assert!(handler.treat_right_ctrl_as_altgr_companion());

    let handler = handler_for("Guacamole RDP", 0x0409);
    assert!(!handler.treat_right_ctrl_as_altgr_companion());
}

#[test]
fn norwegian_altgr_press_clears_companion_ctrl_state() {
    let mut handler = handler_for("Windows App", 0x0414);
    handler.modifier_state.left_ctrl = true;
    handler.modifier_state.right_ctrl = true;
    handler.modifier_state.right_alt = true;

    handler.handle_norwegian_altgr_press().unwrap();

    assert!(!handler.modifier_state.left_ctrl);
    assert!(!handler.modifier_state.right_ctrl);
    assert!(handler.suppressed_ctrl_for_altgr.left);
    assert!(handler.suppressed_ctrl_for_altgr.right);
}

#[test]
fn dead_key_composition_matches_expected_output() {
    assert_eq!(MacInputHandler::compose_dead_key('¨', 'o'), Some('ö'));
    assert_eq!(MacInputHandler::compose_dead_key('´', 'e'), Some('é'));
    assert_eq!(MacInputHandler::compose_dead_key('~', 'n'), Some('ñ'));
    assert_eq!(MacInputHandler::compose_dead_key('`', ' '), Some('`'));
}

#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    Key(u16, bool),
    Command(u16, bool, bool),
    Unicode(u16, bool),
    ReleaseModifiers,
    ReleaseAll,
}

struct RecordingKeyboard(Arc<Mutex<Vec<KeyAction>>>);
impl KeyboardDevice for RecordingKeyboard {
    fn inject_command_keycode(&self, key: u16, pressed: bool, shifted: bool) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(KeyAction::Command(key, pressed, shifted));
        Ok(())
    }
    fn inject_keycode(&self, key: u16, pressed: bool) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(KeyAction::Key(key, pressed));
        Ok(())
    }
    fn inject_unicode(&self, ch: u16, pressed: bool) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(KeyAction::Unicode(ch, pressed));
        Ok(())
    }
    fn release_transient_modifiers(&self) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(KeyAction::ReleaseModifiers);
        Ok(())
    }
    fn release_all(&self) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(KeyAction::ReleaseAll);
        Ok(())
    }
    fn current_modifier_flags(&self) -> (Vec<String>, u64) {
        (Vec::new(), 0)
    }
}

#[test]
fn caps_lock_sync_repeat_and_reconnect_follow_remote_state_without_host_toggles() {
    let mut handler = handler_for("Guacamole RDP", 0x0409);
    let actions = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard(actions.clone())));
    handler.keyboard(KeyboardEvent::Synchronize(
        ironrdp_server::SynchronizeFlags::from_bits_truncate(4),
    ));
    assert_eq!(
        handler.translate_text_key(0x1E, false),
        Some(TextTranslation::Unicode('A'))
    );
    // A press toggles once, even if the client repeats the key-down.
    for _ in 0..3 {
        handler.keyboard(KeyboardEvent::Pressed {
            code: 0x3A,
            extended: false,
        });
    }
    assert_eq!(
        handler.translate_text_key(0x1E, false),
        Some(TextTranslation::Unicode('a'))
    );
    handler.keyboard(KeyboardEvent::Released {
        code: 0x3A,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x3A,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Released {
        code: 0x3A,
        extended: false,
    });
    assert_eq!(handler.last_sync_lock_bits, Some(4));
    handler.keyboard(KeyboardEvent::Synchronize(
        ironrdp_server::SynchronizeFlags::from_bits_truncate(4),
    ));
    assert_eq!(
        handler.translate_text_key(0x1E, false),
        Some(TextTranslation::Unicode('A'))
    );
    assert!(!actions
        .lock()
        .unwrap()
        .iter()
        .any(|action| matches!(action, KeyAction::Key(0x39, _))));
    handler.on_disconnect();
    handler.on_connect(
        ClientInputContext {
            client_name: "Guacamole RDP".into(),
            keyboard_layout: 0x0409,
        },
        None,
    );
    assert_eq!(
        handler.translate_text_key(0x1E, false),
        Some(TextTranslation::Unicode('a'))
    );
    handler.keyboard(KeyboardEvent::Synchronize(
        ironrdp_server::SynchronizeFlags::from_bits_truncate(4),
    ));
    assert_eq!(
        handler.translate_text_key(0x1E, false),
        Some(TextTranslation::Unicode('A'))
    );
}

#[test]
fn caps_lock_xor_shift_applies_to_letters_not_symbols_or_altgr() {
    for (name, layout, letters) in [
        ("Guacamole RDP", 0x0409, vec![(0x1E, 'a', 'A')]),
        (
            "Windows App",
            0x0414,
            vec![
                (0x1E, 'a', 'A'),
                (0x1A, 'å', 'Å'),
                (0x27, 'ø', 'Ø'),
                (0x28, 'æ', 'Æ'),
            ],
        ),
    ] {
        let mut handler = handler_for(name, layout);
        handler.modifier_state.caps_lock = true;
        for (code, lower, upper) in letters {
            handler.modifier_state.left_shift = false;
            assert_eq!(
                handler.translate_text_key(code, false),
                Some(TextTranslation::Unicode(upper))
            );
            handler.modifier_state.left_shift = true;
            assert_eq!(
                handler.translate_text_key(code, false),
                Some(TextTranslation::Unicode(lower))
            );
        }
        handler.modifier_state.left_shift = false;
        assert_eq!(
            handler.translate_text_key(0x02, false),
            Some(TextTranslation::Unicode('1'))
        );
        handler.modifier_state.left_shift = true;
        assert_eq!(
            handler.translate_text_key(0x02, false),
            Some(TextTranslation::Unicode('!'))
        );
    }
    let mut handler = handler_for("Windows App", 0x0414);
    handler.modifier_state.caps_lock = true;
    handler.modifier_state.right_alt = true;
    assert_eq!(
        handler.translate_text_key(0x03, false),
        Some(TextTranslation::Unicode('@'))
    );
    assert_eq!(
        handler.translate_text_key(0x32, false),
        Some(TextTranslation::Unicode('µ'))
    );
    assert_eq!(
        handler.translate_text_key(0x0D, false),
        Some(TextTranslation::Dead('´'))
    );
}

#[test]
fn caps_lock_composes_uppercase_dead_keys_and_releases_the_original_character() {
    let mut handler = handler_for("Windows App", 0x0414);
    let actions = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard(actions.clone())));
    handler.keyboard(KeyboardEvent::Synchronize(
        ironrdp_server::SynchronizeFlags::from_bits_truncate(4),
    ));
    // Norwegian diaeresis, then O, must inject Ö under Caps Lock.
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x1B,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Released {
        code: 0x1B,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x18,
        extended: false,
    });
    // Changing Caps before key-up must not change which Unicode key is released.
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x3A,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Released {
        code: 0x3A,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Released {
        code: 0x18,
        extended: false,
    });
    let unicode: Vec<_> = actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            KeyAction::Unicode(ch, pressed) => Some((*ch, *pressed)),
            _ => None,
        })
        .collect();
    assert_eq!(unicode, vec![('Ö' as u16, true), ('Ö' as u16, false)]);
    assert_eq!(MacInputHandler::compose_dead_key('¨', 'Y'), Some('Ÿ'));
}

#[test]
fn initial_physical_caps_sync_reconciles_host_lock_once() {
    let mut handler = handler_for("Windows App", 0x0409);
    let actions = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard(actions.clone())));
    let caps = ironrdp_server::SynchronizeFlags::from_bits_truncate(4);
    handler.keyboard(KeyboardEvent::Synchronize(caps));
    handler.keyboard(KeyboardEvent::Synchronize(caps));
    let caps_events: Vec<_> = actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            KeyAction::Key(0x39, pressed) => Some(*pressed),
            _ => None,
        })
        .collect();
    assert_eq!(caps_events, vec![true, false]);
    assert!(handler.modifier_state.caps_lock);
}

#[test]
fn disconnect_releases_keyboard_before_reconnect_and_clears_translation_state() {
    let mut handler = handler_for("Windows App", 0x0414);
    let events = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard(Arc::clone(&events))));
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x2A,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x3B,
        extended: false,
    }); // F1
    handler.keyboard(KeyboardEvent::UnicodePressed(0xE5));
    handler.pending_dead_key = Some('^');
    handler
        .translated_key_releases
        .insert((0x1E, false), vec![0xE2]);
    handler.suppressed_ctrl_for_altgr.left = true;
    handler.last_sync_lock_bits = Some(4);
    handler.on_disconnect();
    assert_eq!(events.lock().unwrap().last(), Some(&KeyAction::ReleaseAll));
    assert!(!handler.modifier_state.shift());
    assert!(!handler.suppressed_ctrl_for_altgr.left);
    assert!(handler.pending_dead_key.is_none());
    assert!(handler.translated_key_releases.is_empty());
    assert!(handler.last_sync_lock_bits.is_none());
    handler.on_connect(
        ClientInputContext {
            client_name: "Guacamole RDP".into(),
            keyboard_layout: 0x0409,
        },
        None,
    );
    events.lock().unwrap().clear();
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x1E,
        extended: false,
    });
    assert!(events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(u16::from(b'a'), true)));
    assert!(!events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(0xE2, true)));
    handler.keyboard(KeyboardEvent::Released {
        code: 0x1E,
        extended: false,
    });
    assert!(events
        .lock()
        .unwrap()
        .contains(&KeyAction::Unicode(u16::from(b'a'), false)));
}

#[test]
fn reconnect_without_disconnect_and_synchronize_release_all_remote_keys() {
    let mut handler = handler_for("Windows App", 0x0409);
    let events = Arc::new(Mutex::new(Vec::new()));
    handler.keyboard = Some(Box::new(RecordingKeyboard(Arc::clone(&events))));
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x38,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x1E,
        extended: false,
    });
    handler.on_connect(ClientInputContext::default(), None);
    assert_eq!(events.lock().unwrap().last(), Some(&KeyAction::ReleaseAll));
    handler.keyboard(KeyboardEvent::Pressed {
        code: 0x3B,
        extended: false,
    });
    handler.keyboard(KeyboardEvent::Synchronize(
        ironrdp_server::SynchronizeFlags::empty(),
    ));
    assert_eq!(events.lock().unwrap().last(), Some(&KeyAction::ReleaseAll));
    assert!(handler.translated_key_releases.is_empty());
}
