use super::{normalize_scroll_lines, MouseButton, MouseState};
use anyhow::Result;
use core_graphics::event::{CGEventType, CGMouseButton};
use core_graphics::geometry::CGPoint;

#[derive(Debug)]
struct RecordedEvent(CGEventType, CGMouseButton, f64, f64);

impl PartialEq for RecordedEvent {
    fn eq(&self, other: &Self) -> bool {
        // core-graphics enums expose their C discriminants but do not implement PartialEq.
        self.0 as u32 == other.0 as u32
            && self.1 as u32 == other.1 as u32
            && self.2 == other.2
            && self.3 == other.3
    }
}

fn record(
    events: &mut Vec<RecordedEvent>,
) -> impl FnMut(CGEventType, CGMouseButton, CGPoint) -> Result<()> + '_ {
    move |event_type, button, point| {
        events.push(RecordedEvent(event_type, button, point.x, point.y));
        Ok(())
    }
}

#[test]
fn posts_drag_events_between_each_button_down_and_up() {
    for (button, cg_button, down, dragged, up) in [
        (
            MouseButton::Left,
            CGMouseButton::Left,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseDragged,
            CGEventType::LeftMouseUp,
        ),
        (
            MouseButton::Right,
            CGMouseButton::Right,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseDragged,
            CGEventType::RightMouseUp,
        ),
        (
            MouseButton::Middle,
            CGMouseButton::Center,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseDragged,
            CGEventType::OtherMouseUp,
        ),
    ] {
        let mut mouse = MouseState::default();
        let mut events = Vec::new();
        mouse.move_to(-100.5, 20.25, record(&mut events)).unwrap();
        mouse
            .button_event(button, true, -100.5, 20.25, record(&mut events))
            .unwrap();
        mouse.move_to(-80.0, 25.0, record(&mut events)).unwrap();
        // The RDP decoder forwards the release packet's coordinates before its up event.
        mouse.move_to(-30.0, 50.0, record(&mut events)).unwrap();
        mouse
            .button_event(button, false, -30.0, 50.0, record(&mut events))
            .unwrap();
        mouse.move_to(10.0, 60.0, record(&mut events)).unwrap();

        assert_eq!(
            events,
            vec![
                RecordedEvent(CGEventType::MouseMoved, CGMouseButton::Left, -100.5, 20.25),
                RecordedEvent(down, cg_button, -100.5, 20.25),
                RecordedEvent(dragged, cg_button, -80.0, 25.0),
                RecordedEvent(dragged, cg_button, -30.0, 50.0),
                RecordedEvent(up, cg_button, -30.0, 50.0),
                RecordedEvent(CGEventType::MouseMoved, CGMouseButton::Left, 10.0, 60.0),
            ],
            "incorrect drag sequence for {button:?}"
        );
    }
}

#[test]
fn stationary_clicks_do_not_emit_drag_events() {
    for (button, cg_button, down, up) in [
        (
            MouseButton::Left,
            CGMouseButton::Left,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
        ),
        (
            MouseButton::Right,
            CGMouseButton::Right,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
        ),
        (
            MouseButton::Middle,
            CGMouseButton::Center,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
        ),
    ] {
        let mut mouse = MouseState::default();
        let mut events = Vec::new();
        let mut expected = Vec::new();
        for (x, y) in [(0.0, 0.0), (-100.5, 20.25)] {
            for _ in 0..2 {
                // RDP supplies a position before both transitions, even without movement.
                mouse.move_to(x, y, record(&mut events)).unwrap();
                mouse
                    .button_event(button, true, x, y, record(&mut events))
                    .unwrap();
                mouse.move_to(x, y, record(&mut events)).unwrap();
                mouse
                    .button_event(button, false, x, y, record(&mut events))
                    .unwrap();
                expected.extend([
                    RecordedEvent(CGEventType::MouseMoved, CGMouseButton::Left, x, y),
                    RecordedEvent(down, cg_button, x, y),
                    RecordedEvent(up, cg_button, x, y),
                ]);
            }
        }
        // Even the initial move to default (0, 0) must position the physical cursor.
        assert_eq!(
            events, expected,
            "incorrect stationary clicks for {button:?}"
        );
    }
}

#[test]
fn releasing_one_button_preserves_other_buttons_and_duplicate_events_are_idempotent() {
    let mut mouse = MouseState::default();
    let mut moves = Vec::new();
    let mut discard = |_, _, _| Ok(());
    for button in [MouseButton::Middle, MouseButton::Right, MouseButton::Left] {
        mouse
            .button_event(button, true, 10.0, 20.0, &mut discard)
            .unwrap();
    }
    mouse
        .button_event(MouseButton::Left, true, 10.0, 20.0, &mut discard)
        .unwrap();
    mouse.move_to(11.0, 20.0, record(&mut moves)).unwrap();
    let mut x = 11.0;
    for button in [
        MouseButton::Left,
        MouseButton::Left,
        MouseButton::Right,
        MouseButton::Middle,
    ] {
        mouse
            .button_event(button, false, x, 20.0, &mut discard)
            .unwrap();
        x += 1.0;
        mouse.move_to(x, 20.0, record(&mut moves)).unwrap();
    }

    assert_eq!(
        moves,
        vec![
            RecordedEvent(
                CGEventType::LeftMouseDragged,
                CGMouseButton::Left,
                11.0,
                20.0
            ),
            RecordedEvent(
                CGEventType::RightMouseDragged,
                CGMouseButton::Right,
                12.0,
                20.0
            ),
            RecordedEvent(
                CGEventType::RightMouseDragged,
                CGMouseButton::Right,
                13.0,
                20.0
            ),
            RecordedEvent(
                CGEventType::OtherMouseDragged,
                CGMouseButton::Center,
                14.0,
                20.0
            ),
            RecordedEvent(CGEventType::MouseMoved, CGMouseButton::Left, 15.0, 20.0),
        ]
    );
}

#[test]
fn rejects_nonfinite_positions_before_posting_or_changing_state() {
    let mut mouse = MouseState::default();
    mouse
        .button_event(MouseButton::Left, true, -42.5, 80.25, |_, _, _| Ok(()))
        .unwrap();
    let mut must_not_post = |_, _, _| -> Result<()> { panic!("invalid position was posted") };
    for (x, y) in [
        (f64::NAN, 0.0),
        (0.0, f64::NAN),
        (f64::INFINITY, 0.0),
        (0.0, f64::INFINITY),
        (f64::NEG_INFINITY, -1200.0),
        (-1200.0, f64::NEG_INFINITY),
    ] {
        assert!(mouse.move_to(x, y, &mut must_not_post).is_err());
        assert!(mouse
            .button_event(MouseButton::Right, true, x, y, &mut must_not_post)
            .is_err());
        assert!(mouse
            .button_event(MouseButton::Left, false, x, y, &mut must_not_post)
            .is_err());
    }

    let mut events = Vec::new();
    mouse.release_all(record(&mut events)).unwrap();
    assert_eq!(
        events,
        vec![RecordedEvent(
            CGEventType::LeftMouseUp,
            CGMouseButton::Left,
            -42.5,
            80.25
        )]
    );
}

#[test]
fn failed_posts_do_not_start_or_end_drags_or_change_cleanup_position() {
    let mut mouse = MouseState::default();
    let mut events = Vec::new();
    let mut fail = |_, _, _| anyhow::bail!("simulated event creation failure");
    assert!(mouse
        .button_event(MouseButton::Left, true, 100.0, 200.0, &mut fail)
        .is_err());
    mouse.move_to(1.0, 2.0, record(&mut events)).unwrap();
    assert!(matches!(events[0].0, CGEventType::MouseMoved));
    mouse
        .button_event(MouseButton::Left, true, 10.0, 20.0, record(&mut events))
        .unwrap();
    assert!(mouse.move_to(300.0, 400.0, &mut fail).is_err());
    assert!(mouse
        .button_event(MouseButton::Left, false, 500.0, 600.0, &mut fail)
        .is_err());
    assert!(mouse
        .button_event(MouseButton::Right, true, 700.0, 800.0, &mut fail)
        .is_err());
    mouse.release_all(record(&mut events)).unwrap();
    assert_eq!(
        events.last(),
        Some(&RecordedEvent(
            CGEventType::LeftMouseUp,
            CGMouseButton::Left,
            10.0,
            20.0
        ))
    );
    assert_eq!(events.len(), 3);
    mouse.move_to(30.0, 40.0, record(&mut events)).unwrap();
    assert!(matches!(events.last().unwrap().0, CGEventType::MouseMoved));
}

#[test]
fn cleanup_releases_every_held_button_at_last_successful_move_and_is_repeatable() {
    let mut mouse = MouseState::default();
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        mouse
            .button_event(button, true, 10.0, 20.0, |_, _, _| Ok(()))
            .unwrap();
    }
    mouse.move_to(-100.5, 300.25, |_, _, _| Ok(())).unwrap();
    assert!(mouse
        .move_to(900.0, 999.0, |_, _, _| anyhow::bail!("failed move"))
        .is_err());
    let mut events = Vec::new();
    mouse.release_all(record(&mut events)).unwrap();
    mouse.release_all(record(&mut events)).unwrap();
    assert_eq!(
        events,
        vec![
            RecordedEvent(
                CGEventType::LeftMouseUp,
                CGMouseButton::Left,
                -100.5,
                300.25
            ),
            RecordedEvent(
                CGEventType::RightMouseUp,
                CGMouseButton::Right,
                -100.5,
                300.25
            ),
            RecordedEvent(
                CGEventType::OtherMouseUp,
                CGMouseButton::Center,
                -100.5,
                300.25
            ),
        ]
    );
    mouse.move_to(1.0, 2.0, record(&mut events)).unwrap();
    assert!(matches!(events.last().unwrap().0, CGEventType::MouseMoved));
}

#[test]
fn cleanup_continues_after_failures_and_retries_only_failed_buttons() {
    let mut mouse = MouseState::default();
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        mouse
            .button_event(button, true, 15.0, 25.0, |_, _, _| Ok(()))
            .unwrap();
    }
    let mut attempts = Vec::new();
    let result = mouse.release_all(|event_type, button, point| {
        attempts.push(RecordedEvent(event_type, button, point.x, point.y));
        if matches!(button, CGMouseButton::Left | CGMouseButton::Center) {
            anyhow::bail!("failed release for {button:?}");
        }
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(
        attempts,
        vec![
            RecordedEvent(CGEventType::LeftMouseUp, CGMouseButton::Left, 15.0, 25.0),
            RecordedEvent(CGEventType::RightMouseUp, CGMouseButton::Right, 15.0, 25.0),
            RecordedEvent(CGEventType::OtherMouseUp, CGMouseButton::Center, 15.0, 25.0),
        ]
    );
    let mut retries = Vec::new();
    mouse.release_all(record(&mut retries)).unwrap();
    mouse.release_all(record(&mut retries)).unwrap();
    assert_eq!(
        retries,
        vec![
            RecordedEvent(CGEventType::LeftMouseUp, CGMouseButton::Left, 15.0, 25.0),
            RecordedEvent(CGEventType::OtherMouseUp, CGMouseButton::Center, 15.0, 25.0),
        ]
    );
    mouse.move_to(1.0, 2.0, record(&mut retries)).unwrap();
    assert!(matches!(retries.last().unwrap().0, CGEventType::MouseMoved));
}

#[test]
fn cleanup_without_held_buttons_does_not_post() {
    MouseState::default()
        .release_all(|_, _, _| panic!("cleanup posted with no held buttons"))
        .unwrap();
}

#[test]
fn keeps_small_scroll_values() {
    assert_eq!(normalize_scroll_lines(0), 0);
    assert_eq!(normalize_scroll_lines(1), 1);
    assert_eq!(normalize_scroll_lines(-3), -3);
}

#[test]
fn converts_windows_wheel_detents_to_lines() {
    assert_eq!(normalize_scroll_lines(120), 1);
    assert_eq!(normalize_scroll_lines(-120), -1);
    assert_eq!(normalize_scroll_lines(240), 2);
    assert_eq!(normalize_scroll_lines(-241), -3);
}
