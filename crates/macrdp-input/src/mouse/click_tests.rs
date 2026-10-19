use super::{MouseButton, MouseInjector, MouseState};
use core_graphics::event::{CGEventType, EventField};
use core_graphics::geometry::CGPoint;
use std::time::{Duration, Instant};

fn button(
    mouse: &mut MouseState,
    which: MouseButton,
    down: bool,
    point: (f64, f64),
    at: Instant,
) -> i64 {
    let mut count = 0;
    mouse
        .button_event_at(which, down, point.0, point.1, at, |_, _, _, clicks| {
            count = clicks;
            Ok(())
        })
        .unwrap();
    count
}

fn click(mouse: &mut MouseState, which: MouseButton, point: (f64, f64), at: Instant) -> i64 {
    // Guacamole sends a position before every transition, including stationary clicks.
    mouse
        .move_to(point.0, point.1, |_, _, _, count| {
            assert_eq!(count, 0);
            Ok(())
        })
        .unwrap();
    let count = button(mouse, which, true, point, at);
    mouse
        .move_to(point.0, point.1, |_, _, _, _| panic!("stationary drag"))
        .unwrap();
    assert_eq!(
        button(mouse, which, false, point, at + Duration::from_millis(20)),
        count
    );
    count
}

#[test]
fn double_and_triple_click_counts_survive_guacamole_position_packets_for_every_button() {
    for which in MouseButton::ALL {
        let mut mouse = MouseState::default();
        let now = Instant::now();
        for count in 1..=3 {
            assert_eq!(
                click(
                    &mut mouse,
                    which,
                    (-100.5, 20.25),
                    now + Duration::from_millis((count - 1) * 100)
                ),
                count as i64
            );
        }
    }
}

#[test]
fn interval_is_between_downs_and_respects_the_host_setting() {
    let now = Instant::now();
    let mut mouse = MouseState {
        double_click_interval: Duration::from_millis(200),
        ..Default::default()
    };
    assert_eq!(
        button(&mut mouse, MouseButton::Left, true, (0.0, 0.0), now),
        1
    );
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            false,
            (0.0, 0.0),
            now + Duration::from_millis(190)
        ),
        1
    );
    // A recent release does not extend the double-click window of a long press.
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (0.0, 0.0),
            now + Duration::from_millis(201)
        ),
        1
    );
    // A setting change applies to the next press without replacing input state.
    mouse.double_click_interval = Duration::from_millis(800);
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (0.0, 0.0),
            now + Duration::from_millis(900)
        ),
        2
    );
}

#[test]
fn small_jitter_is_allowed_but_moving_away_and_back_breaks_the_sequence() {
    let now = Instant::now();
    let mut mouse = MouseState::default();
    assert_eq!(click(&mut mouse, MouseButton::Left, (10.0, 20.0), now), 1);
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (12.0, 21.0),
            now + Duration::from_millis(100)
        ),
        2
    );
    mouse.move_to(100.0, 200.0, |_, _, _, _| Ok(())).unwrap();
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (12.0, 21.0),
            now + Duration::from_millis(200)
        ),
        1
    );
}

#[test]
fn successive_small_moves_cannot_drift_a_sequence_away_from_its_first_click() {
    let now = Instant::now();
    let mut mouse = MouseState::default();
    assert_eq!(click(&mut mouse, MouseButton::Left, (0.0, 0.0), now), 1);
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (3.0, 0.0),
            now + Duration::from_millis(100)
        ),
        2
    );
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (6.0, 0.0),
            now + Duration::from_millis(200)
        ),
        1
    );
}

#[test]
fn long_held_release_reports_zero_and_starts_a_fresh_sequence() {
    let now = Instant::now();
    let mut mouse = MouseState::default();
    click(&mut mouse, MouseButton::Left, (0.0, 0.0), now);
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            true,
            (0.0, 0.0),
            now + Duration::from_millis(100)
        ),
        2
    );
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            false,
            (0.0, 0.0),
            now + Duration::from_millis(601)
        ),
        0
    );
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (0.0, 0.0),
            now + Duration::from_millis(650)
        ),
        1
    );
}

#[test]
fn dragging_preserves_current_count_but_ends_the_following_click_sequence() {
    let now = Instant::now();
    let mut mouse = MouseState::default();
    assert_eq!(click(&mut mouse, MouseButton::Left, (10.0, 20.0), now), 1);
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            true,
            (10.0, 20.0),
            now + Duration::from_millis(100)
        ),
        2
    );
    mouse
        .move_to(100.0, 200.0, |kind, _, _, count| {
            assert!(matches!(kind, CGEventType::LeftMouseDragged));
            assert_eq!(count, 2);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            false,
            (10.0, 20.0),
            now + Duration::from_millis(120)
        ),
        2
    );
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (10.0, 20.0),
            now + Duration::from_millis(200)
        ),
        1
    );
}

#[test]
fn another_button_and_chords_cannot_join_unrelated_clicks() {
    let now = Instant::now();
    let mut mouse = MouseState::default();
    assert_eq!(click(&mut mouse, MouseButton::Left, (0.0, 0.0), now), 1);
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Right,
            (0.0, 0.0),
            now + Duration::from_millis(50)
        ),
        1
    );
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            true,
            (0.0, 0.0),
            now + Duration::from_millis(100)
        ),
        1
    );
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Right,
            true,
            (0.0, 0.0),
            now + Duration::from_millis(110)
        ),
        1
    );
    button(
        &mut mouse,
        MouseButton::Right,
        false,
        (0.0, 0.0),
        now + Duration::from_millis(120),
    );
    button(
        &mut mouse,
        MouseButton::Left,
        false,
        (0.0, 0.0),
        now + Duration::from_millis(130),
    );
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (0.0, 0.0),
            now + Duration::from_millis(200)
        ),
        1
    );
}

#[test]
fn failed_posts_and_duplicate_transitions_do_not_advance_or_cancel_clicks() {
    let now = Instant::now();
    let mut mouse = MouseState::default();
    click(&mut mouse, MouseButton::Left, (0.0, 0.0), now);
    assert!(mouse
        .move_to(200.0, 200.0, |_, _, _, _| anyhow::bail!("failed move"))
        .is_err());
    assert!(mouse
        .button_event_at(
            MouseButton::Right,
            true,
            200.0,
            200.0,
            now + Duration::from_millis(50),
            |_, _, _, _| anyhow::bail!("failed other button")
        )
        .is_err());
    for _ in 0..2 {
        assert!(mouse
            .button_event_at(
                MouseButton::Left,
                true,
                0.0,
                0.0,
                now + Duration::from_millis(100),
                |_, _, _, count| {
                    assert_eq!(count, 2);
                    anyhow::bail!("failed down")
                }
            )
            .is_err());
    }
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            true,
            (0.0, 0.0),
            now + Duration::from_millis(100)
        ),
        2
    );
    mouse
        .button_event_at(
            MouseButton::Left,
            true,
            0.0,
            0.0,
            now + Duration::from_millis(110),
            |_, _, _, _| panic!("duplicate down posted"),
        )
        .unwrap();
    assert!(mouse
        .button_event_at(
            MouseButton::Left,
            false,
            200.0,
            200.0,
            now + Duration::from_millis(120),
            |_, _, _, count| {
                assert_eq!(count, 0);
                anyhow::bail!("failed up")
            }
        )
        .is_err());
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            false,
            (0.0, 0.0),
            now + Duration::from_millis(130)
        ),
        2
    );
    mouse
        .button_event_at(
            MouseButton::Left,
            false,
            200.0,
            200.0,
            now + Duration::from_millis(140),
            |_, _, _, _| panic!("duplicate up posted"),
        )
        .unwrap();
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (0.0, 0.0),
            now + Duration::from_millis(200)
        ),
        3
    );
}

#[test]
fn cleanup_preserves_failed_release_count_but_clears_history_even_with_no_held_button() {
    let now = Instant::now();
    let mut mouse = MouseState::default();
    click(&mut mouse, MouseButton::Left, (0.0, 0.0), now);
    assert_eq!(
        button(
            &mut mouse,
            MouseButton::Left,
            true,
            (0.0, 0.0),
            now + Duration::from_millis(100)
        ),
        2
    );
    assert!(mouse
        .release_all_at(now + Duration::from_millis(120), |_, _, _, count| {
            assert_eq!(count, 2);
            anyhow::bail!("cleanup failed")
        })
        .is_err());
    mouse
        .release_all_at(now + Duration::from_millis(130), |_, _, _, count| {
            assert_eq!(count, 2);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (0.0, 0.0),
            now + Duration::from_millis(200)
        ),
        1
    );
    mouse
        .release_all_at(now + Duration::from_millis(250), |_, _, _, _| {
            panic!("nothing held")
        })
        .unwrap();
    assert_eq!(
        click(
            &mut mouse,
            MouseButton::Left,
            (0.0, 0.0),
            now + Duration::from_millis(300)
        ),
        1
    );
}

#[test]
fn native_double_click_interval_is_readable_from_an_input_worker() {
    // Exercise the AppKit class method on a worker, without creating an
    // application, reading the desktop or posting native input.
    let interval = std::thread::spawn(super::system_double_click_interval)
        .join()
        .expect("native interval reader panicked");
    assert!(!interval.is_zero());
}

#[test]
fn native_mouse_events_have_the_requested_count_without_posting() {
    for (kind, button) in [
        (CGEventType::LeftMouseDown, 0),
        (CGEventType::LeftMouseDragged, 0),
        (CGEventType::LeftMouseUp, 0),
        (CGEventType::RightMouseDown, 1),
        (CGEventType::OtherMouseDown, 4),
    ] {
        for count in 1..=3 {
            let event =
                MouseInjector::create_mouse_event(kind, button, CGPoint::new(-100.5, 20.25), count)
                    .unwrap();
            assert_eq!(
                event.get_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE),
                count
            );
            assert_eq!(
                event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER),
                i64::from(button)
            );
        }
    }
}

#[test]
fn native_normal_and_private_scroll_payloads_can_be_stamped_without_posting() {
    use core_graphics::event::CGEventFlags;
    use core_graphics::event_source::CGEventSourceStateID;
    for flags in [None, Some(CGEventFlags::CGEventFlagShift)] {
        let event = MouseInjector::create_scroll_event(2, -3, flags).unwrap();
        crate::event_tag::stamp_remote_event(&event).unwrap();
        assert_eq!(
            event.get_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID),
            i64::from(std::process::id())
        );
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1),
            -3
        );
        assert_eq!(
            event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2),
            2
        );
        if let Some(flags) = flags {
            assert_eq!(event.get_flags(), flags);
            let state = event.get_integer_value_field(EventField::EVENT_SOURCE_STATE_ID);
            assert_ne!(state, CGEventSourceStateID::CombinedSessionState as i64);
            assert_ne!(state, CGEventSourceStateID::HIDSystemState as i64);
        }
    }
}
