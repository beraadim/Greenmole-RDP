//! Synthetic handlers and in-memory PDUs only; no native input is posted.

use super::*;
use crate::gfx::NativeInputObservation;
use crate::{KeyboardEvent, MouseEvent};
use std::time::Instant;

struct ObservedHandler(Arc<std::sync::Mutex<GfxState>>);

impl ObservedHandler {
    fn native_return(&self) {
        self.0
            .lock()
            .unwrap()
            .input
            .record_native_callback(NativeInputObservation {
                post_attempts: 1,
                post_returns: 1,
                last_post_at: Some(Instant::now()),
                post_ms: vec![0.25],
                ..Default::default()
            });
    }
}

impl RdpServerInputHandler for ObservedHandler {
    fn keyboard(&mut self, _: KeyboardEvent) {
        self.native_return();
    }
    fn mouse(&mut self, _: MouseEvent) {
        self.native_return();
    }
}

fn fixture() -> (RdpServer, Arc<std::sync::Mutex<GfxState>>) {
    let state = Arc::new(std::sync::Mutex::new(GfxState::new(800, 600, false, false)));
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_input_handler(ObservedHandler(state.clone()))
        .with_no_display()
        .build();
    server.set_gfx_state(state.clone());
    (server, state)
}

#[tokio::test]
async fn handler_lock_wait_is_measured_from_original_read_and_native_return_is_correlated() {
    use std::future::Future as _;
    let (mut server, state) = fixture();
    let handler = server.handler.clone();
    let held = handler.lock().await;
    let at = Instant::now() - Duration::from_millis(80);
    let _context = server.additional_input.context(Some(at));
    let input = FastPathInput::single(FastPathInputEvent::SyncEvent(crate::SynchronizeFlags::empty()));
    let dispatch = server.handle_fastpath(input);
    tokio::pin!(dispatch);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(dispatch.as_mut().poll(cx).is_pending()))
            .await
    );
    let pending = state.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(pending.received_events, 1);
    assert_eq!(pending.dispatched_events, 0);
    drop(held);
    dispatch.await;
    let completed = state.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(completed.completed_dispatches, 1);
    assert!(completed.dispatch_wait.max_ms.unwrap() >= 80.0);
    assert!(completed.read_to_post.max_ms.unwrap() >= 80.0);
    assert_eq!(completed.native_post.max_ms, Some(0.25));
}

#[tokio::test]
async fn fast_slow_and_additional_input_are_counted_without_qoe_or_unused_metadata() {
    use ironrdp_ainput::{ClientPdu, MouseEventFlags, MousePdu};
    use ironrdp_dvc::DvcProcessor as _;
    use ironrdp_pdu::input::{InputEvent, SyncPdu, UnusedPdu};
    let (mut server, state) = fixture();
    let at = Instant::now() - Duration::from_millis(15);
    let _context = server.additional_input.context(Some(at));
    let mut additional = AInputHandler::new(
        server.additional_input.clone(),
        server.input_activity.clone(),
    );
    // The untrusted client timestamp is deliberately unrelated to server time.
    additional
        .process(
            1,
            &encode_vec(&ClientPdu::Mouse(MousePdu {
                flags: MouseEventFlags::MOVE,
                x: 0,
                y: 0,
                time: u64::MAX,
            }))
            .unwrap(),
        )
        .unwrap();
    server
        .handle_fastpath(
            FastPathInput::new(vec![
                FastPathInputEvent::QoeEvent(u32::MAX),
                FastPathInputEvent::SyncEvent(crate::SynchronizeFlags::empty()),
            ])
            .unwrap(),
        )
        .await;
    server
        .handle_input_event(InputEventPdu(vec![
            InputEvent::Unused(UnusedPdu),
            InputEvent::Sync(SyncPdu {
                flags: ironrdp_pdu::input::sync::SyncToggleFlags::empty(),
            }),
        ]))
        .await;
    let snapshot = state.lock().unwrap().input.snapshot(Instant::now());
    assert_eq!(snapshot.received_events, 3);
    assert_eq!(snapshot.dispatched_events, 3);
    assert_eq!(snapshot.completed_dispatches, 3);
    assert_eq!(snapshot.native_callbacks, 3);
    assert_eq!(snapshot.post_returns, 3);
    assert_eq!(snapshot.untimed_events, 0);
    assert!(snapshot.dispatch_wait.max_ms.unwrap() >= 15.0);
}

#[test]
fn resize_preserves_input_history_and_reconnect_clears_it() {
    let mut state = GfxState::new(800, 600, false, false);
    let now = Instant::now();
    state.input.record_received(Some(now), 4);
    state.input.record_native_callback(NativeInputObservation {
        kind: crate::gfx::InputCallbackKind::MouseButtonDown,
        mouse_button_down_posts: 1,
        mouse_drag_posts: 2,
        ..Default::default()
    });
    state.record_capture_queue(1.0);
    assert_eq!(state.total_captures_delivered, 1);
    state.reset_connection_state(true);
    assert_eq!(state.input.snapshot(now).received_events, 4);
    assert_eq!(state.input.snapshot(now).mouse_button_down_events, 1);
    assert_eq!(state.input.snapshot(now).mouse_button_down_posts, 1);
    assert_eq!(state.input.snapshot(now).mouse_drag_posts, 2);
    state.begin_connection(None);
    let snapshot = state.input.snapshot(now);
    assert_eq!(snapshot.received_events, 0);
    assert_eq!(snapshot.last_receive_age_ms, None);
    assert_eq!(snapshot.post_returns, 0);
    assert_eq!(snapshot.mouse_button_down_events, 0);
    assert_eq!(snapshot.mouse_button_down_posts, 0);
    assert_eq!(snapshot.mouse_drag_posts, 0);
}
