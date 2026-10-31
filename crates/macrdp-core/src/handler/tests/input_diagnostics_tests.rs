use super::*;
use ironrdp_server::{gfx::GfxState, MouseEvent};
use std::time::Instant;

struct Events;
impl crate::callbacks::ServerEventHandler for Events {}

fn observed_handler() -> (MacInputHandler, Arc<Mutex<GfxState>>) {
    let gfx = Arc::new(Mutex::new(GfxState::new(1920, 1200, false, false)));
    let handler = handler_for("synthetic input fixture", 0x0409).with_events(
        Arc::new(Events),
        gfx.clone(),
        Arc::new(macrdp_audio::AudioStats::default()),
    );
    (handler, gfx)
}

#[test]
fn unavailable_and_guard_blocked_callbacks_never_claim_native_posting() {
    let (mut handler, gfx) = observed_handler();
    handler.keyboard(KeyboardEvent::UnicodePressed(0x0041));
    handler.mouse(MouseEvent::Move { x: 40, y: 50 });
    let snapshot = gfx.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(snapshot.native_callbacks, 2);
    assert_eq!(snapshot.keyboard_events, 1);
    assert_eq!(snapshot.mouse_move_events, 1);
    assert_eq!(snapshot.unavailable_callbacks, 2);
    assert_eq!(snapshot.blocked_callbacks, 0);
    assert_eq!(snapshot.post_attempts, 0);
    assert!(snapshot.last_post_age_ms.is_none());

    handler.privacy = super::super::privacy::PrivateSession::new(true);
    handler.keyboard(KeyboardEvent::UnicodePressed(0x0042));
    handler.mouse(MouseEvent::Move { x: 60, y: 70 });
    let snapshot = gfx.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(snapshot.native_callbacks, 4);
    assert_eq!(snapshot.keyboard_events, 2);
    assert_eq!(snapshot.mouse_move_events, 2);
    assert_eq!(snapshot.blocked_callbacks, 2);
    assert_eq!(snapshot.unavailable_callbacks, 2);
    assert_eq!(snapshot.post_returns, 0);
    assert_eq!(snapshot.native_post.samples, 0);
}

#[test]
fn button_and_relative_callbacks_remain_distinguishable_when_no_injector_posts() {
    let (mut handler, gfx) = observed_handler();
    // A decoded absolute button packet expands to a position then transition.
    // A zero-delta relative button packet needs only the transition callback.
    for event in [
        MouseEvent::Move { x: 120, y: 80 },
        MouseEvent::LeftPressed,
        MouseEvent::Move { x: 120, y: 80 },
        MouseEvent::LeftReleased,
        MouseEvent::RightPressed,
        MouseEvent::RelMove { x: -4, y: 2 },
        MouseEvent::RightReleased,
        MouseEvent::ScrollHighResolution { x: 0, y: 65536 },
    ] {
        handler.mouse(event);
    }
    handler.keyboard(KeyboardEvent::Synchronize(
        ironrdp_server::SynchronizeFlags::empty(),
    ));
    let snapshot = gfx.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(snapshot.native_callbacks, 9);
    assert_eq!(snapshot.keyboard_events, 1);
    assert_eq!(snapshot.mouse_move_events, 2);
    assert_eq!(snapshot.mouse_relative_move_events, 1);
    assert_eq!(snapshot.mouse_button_down_events, 2);
    assert_eq!(snapshot.mouse_button_up_events, 2);
    assert_eq!(snapshot.mouse_scroll_events, 1);
    assert_eq!(snapshot.unavailable_callbacks, 9);
    assert_eq!(snapshot.mouse_button_down_posts, 0);
    assert_eq!(snapshot.mouse_button_up_posts, 0);
    assert_eq!(snapshot.mouse_drag_posts, 0);
}

struct FailingKeyboard;
impl KeyboardDevice for FailingKeyboard {
    fn inject_keycode(&self, _: u16, _: bool) -> anyhow::Result<()> {
        anyhow::bail!("synthetic allocation failure")
    }
    fn inject_command_keycode(&self, _: u16, _: bool, _: bool) -> anyhow::Result<()> {
        anyhow::bail!("synthetic allocation failure")
    }
    fn inject_unicode(&self, _: u16, _: bool) -> anyhow::Result<()> {
        anyhow::bail!("synthetic allocation failure")
    }
    fn release_transient_modifiers(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn release_all(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn current_modifier_flags(&self) -> (Vec<String>, u64) {
        (Vec::new(), 0)
    }
}

#[test]
fn callback_allocation_errors_and_connection_cleanup_have_distinct_counts() {
    let (mut handler, gfx) = observed_handler();
    handler.keyboard = Some(Box::new(FailingKeyboard));
    handler.on_connect(ClientInputContext::default(), None);
    handler.keyboard(KeyboardEvent::UnicodePressed(0x0041));
    let before = gfx.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(before.native_callbacks, 1);
    assert_eq!(before.keyboard_events, 1);
    assert_eq!(before.injection_failures, 1);
    assert_eq!(before.post_attempts, 0);
    assert_eq!(before.post_returns, 0);
    handler.on_disconnect();
    let after = gfx.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(after.native_callbacks, before.native_callbacks);
    assert_eq!(after.keyboard_events, before.keyboard_events);
    assert_eq!(after.injection_failures, before.injection_failures);
    assert_eq!(after.post_attempts, before.post_attempts);
    gfx.lock().unwrap().begin_connection(None);
    let fresh = gfx.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(fresh.native_callbacks, 0);
    assert_eq!(fresh.keyboard_events, 0);
    assert_eq!(fresh.injection_failures, 0);
    assert!(fresh.last_post_age_ms.is_none());
}
