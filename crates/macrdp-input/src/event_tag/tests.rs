use super::*;
use core_graphics::event::{CGEventFlags, CGEventType};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

fn event() -> CGEvent {
    let source = CGEventSource::new(CGEventSourceStateID::Private).unwrap();
    CGEvent::new(source).unwrap()
}

#[test]
fn replacing_source_metadata_preserves_unicode_and_pointer_payloads() {
    use core_graphics::event::CGMouseButton;
    use core_graphics::geometry::CGPoint;
    extern "C" {
        fn CGEventKeyboardGetUnicodeString(
            event: *mut core_graphics::sys::CGEvent,
            max_length: usize,
            actual_length: *mut usize,
            text: *mut u16,
        );
    }

    let source = CGEventSource::new(CGEventSourceStateID::Private).unwrap();
    let keyboard = CGEvent::new_keyboard_event(source.clone(), 0x00, true).unwrap();
    let text = [0xD83D, 0xDE03];
    keyboard.set_string_from_utf16_unchecked(&text);
    stamp(&keyboard, 1).unwrap();
    let mut actual = [0; 2];
    let mut length = 0;
    unsafe {
        CGEventKeyboardGetUnicodeString(
            keyboard.as_ptr(),
            actual.len(),
            &mut length,
            actual.as_mut_ptr(),
        );
    }
    assert_eq!(length, text.len());
    assert_eq!(actual, text);
    assert_eq!(
        keyboard.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
        0x00
    );

    let mouse = CGEvent::new_mouse_event(
        source,
        CGEventType::LeftMouseDragged,
        CGPoint::new(-100.5, 25.25),
        CGMouseButton::Left,
    )
    .unwrap();
    mouse.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 2);
    stamp(&mouse, -1).unwrap();
    let location = mouse.location();
    assert_eq!((location.x, location.y), (-100.5, 25.25));
    assert_eq!(
        mouse.get_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE),
        2
    );

    let scroll = event();
    scroll.set_type(CGEventType::ScrollWheel);
    scroll.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1, -3);
    scroll.set_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2, 2);
    stamp(&scroll, 1).unwrap();
    assert_eq!(
        scroll.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1),
        -3
    );
    assert_eq!(
        scroll.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2),
        2
    );
}

#[test]
fn stamps_marker_and_worker_pid_without_changing_event_type_or_flags() {
    // Only construct and inspect events; never post to the user's desktop.
    for event_type in [
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseDragged,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseDragged,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseDragged,
        CGEventType::OtherMouseUp,
        CGEventType::ScrollWheel,
    ] {
        for tag in [0, 1, i64::MAX, i64::MIN, -1] {
            let event = event();
            let flags = CGEventFlags::CGEventFlagShift | CGEventFlags::CGEventFlagCommand;
            event.set_type(event_type);
            event.set_flags(flags);
            event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, 99);
            event.set_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID, 1);
            stamp(&event, tag).unwrap();
            assert_eq!(
                event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
                tag
            );
            assert_eq!(
                event.get_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID),
                i64::from(std::process::id())
            );
            assert_eq!(event.get_type() as u32, event_type as u32);
            assert_eq!(event.get_flags(), flags);
        }
    }
}

#[test]
fn session_marker_is_replaced_and_cleared_at_the_posting_boundary() {
    struct ResetTag;
    impl Drop for ResetTag {
        fn drop(&mut self) {
            clear_remote_event_tag();
        }
    }
    let _reset = ResetTag;
    let prepared_before_session = event();
    for tag in [i64::MAX, i64::MIN] {
        set_remote_event_tag(tag);
        assert_eq!(private_modifier_flags(), Some(CGEventFlags::empty()));
        set_private_modifier_flags(CGEventFlags::CGEventFlagShift);
        assert_eq!(
            private_modifier_flags(),
            Some(CGEventFlags::CGEventFlagShift)
        );
        stamp_remote_event(&prepared_before_session).unwrap();
        assert_eq!(
            prepared_before_session.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
            tag
        );
    }
    clear_remote_event_tag();
    assert_eq!(private_modifier_flags(), None);
    stamp_remote_event(&prepared_before_session).unwrap();
    assert_eq!(
        prepared_before_session.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
        0
    );
}
