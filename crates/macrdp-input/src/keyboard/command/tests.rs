use super::*;
use core_graphics::event::EventField;

fn snapshot(flags: CGEventFlags) -> ModifierSnapshot {
    ModifierSnapshot {
        flags,
        left_command: false,
        right_command: false,
    }
}

#[test]
fn every_shortcut_press_repeat_and_release_ends_with_explicit_command_release() {
    // Inspect native events without posting shortcuts to the user's desktop.
    for keycode in [0x00, 0x07, 0x08, 0x09, 0x24, 0x4C] {
        for shifted in [false, true] {
            for pressed in [true, true, false] {
                let native = CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagAlphaShift;
                let [key, restore] = events(keycode, pressed, shifted, snapshot(native)).unwrap();
                let mut expected = CGEventFlags::CGEventFlagCommand;
                expected.set(CGEventFlags::CGEventFlagShift, shifted);
                expected.set(CGEventFlags::CGEventFlagNumericPad, keycode == 0x4C);
                assert_eq!(key.get_flags(), expected);
                assert_eq!(
                    key.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
                    i64::from(keycode)
                );
                assert_eq!(
                    key.get_type() as u32,
                    if pressed {
                        CGEventType::KeyDown
                    } else {
                        CGEventType::KeyUp
                    } as u32
                );
                assert_eq!(restore.get_type() as u32, CGEventType::FlagsChanged as u32);
                assert_eq!(
                    restore.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
                    i64::from(LEFT_COMMAND)
                );
                assert_eq!(restore.get_flags(), native);
            }
        }
    }
}

#[test]
fn stale_aggregate_and_both_device_command_flags_are_cleared() {
    let unrelated = CGEventFlags::CGEventFlagControl
        | CGEventFlags::CGEventFlagShift
        | CGEventFlags::CGEventFlagAlternate
        | CGEventFlags::CGEventFlagAlphaShift
        | CGEventFlags::CGEventFlagSecondaryFn;
    let stale = CGEventFlags::from_bits_retain(
        unrelated.bits()
            | CGEventFlags::CGEventFlagCommand.bits()
            | LEFT_COMMAND_MASK
            | RIGHT_COMMAND_MASK,
    );
    let [_, restore] = events(0x09, false, true, snapshot(stale)).unwrap();
    assert_eq!(restore.get_flags(), unrelated);
}

#[test]
fn genuine_left_right_and_both_command_keys_are_preserved() {
    for (left, right, keycode) in [
        (true, false, LEFT_COMMAND),
        (false, true, RIGHT_COMMAND),
        (true, true, LEFT_COMMAND),
    ] {
        let native = ModifierSnapshot {
            flags: CGEventFlags::CGEventFlagShift,
            left_command: left,
            right_command: right,
        };
        let [_, restore] = events(0x08, false, false, native).unwrap();
        assert_eq!(
            restore.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
            i64::from(keycode)
        );
        let mut expected =
            (CGEventFlags::CGEventFlagCommand | CGEventFlags::CGEventFlagShift).bits();
        if left {
            expected |= LEFT_COMMAND_MASK;
        }
        if right {
            expected |= RIGHT_COMMAND_MASK;
        }
        assert_eq!(restore.get_flags().bits(), expected);
    }
}

#[test]
fn ctrl_first_release_does_not_restore_an_old_ctrl_snapshot() {
    let [_, after_press] = events(
        0x08,
        true,
        false,
        snapshot(CGEventFlags::CGEventFlagControl),
    )
    .unwrap();
    assert_eq!(after_press.get_flags(), CGEventFlags::CGEventFlagControl);
    // The current HID snapshot is captured for each post, not saved from the
    // initial press. Ctrl was released before C, so neither modifier remains.
    let [_, after_release] = events(0x08, false, false, snapshot(CGEventFlags::empty())).unwrap();
    assert_eq!(after_release.get_flags(), CGEventFlags::empty());
}
