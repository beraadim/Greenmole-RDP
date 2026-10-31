//! Join platform posting observations to the current transport dispatch without
//! copying any input payload into diagnostics.

use super::MacInputHandler;
use ironrdp_server::gfx::{InputCallbackKind, NativeInputObservation};
use ironrdp_server::MouseEvent;
use macrdp_input::PostObservationScope;

#[derive(Default)]
pub(super) struct CallbackResult {
    failed: bool,
    blocked: bool,
    unavailable: bool,
}

impl CallbackResult {
    pub(super) fn completed(failed: bool) -> Self {
        Self {
            failed,
            ..Self::default()
        }
    }

    pub(super) fn blocked() -> Self {
        Self {
            blocked: true,
            ..Self::default()
        }
    }

    pub(super) fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::default()
        }
    }
}

impl MacInputHandler {
    pub(super) fn observe_input_callback(
        &mut self,
        kind: InputCallbackKind,
        callback: impl FnOnce(&mut Self) -> CallbackResult,
    ) {
        let gfx = self
            .session_events
            .as_ref()
            .map(|events| events.gfx.clone());
        let Some(gfx) = gfx else {
            callback(self);
            return;
        };
        let scope = PostObservationScope::begin();
        let result = callback(self);
        let posts = scope.finish();
        gfx.lock()
            .unwrap()
            .input
            .record_native_callback(NativeInputObservation {
                kind,
                callback_blocked: result.blocked,
                callback_unavailable: result.unavailable,
                callback_failed: result.failed,
                post_attempts: posts.post_attempts,
                post_returns: posts.post_returns,
                post_errors: posts.post_errors,
                post_samples_dropped: posts.post_samples_dropped,
                keyboard_posts: posts.keyboard_posts,
                mouse_move_posts: posts.mouse_move_posts,
                mouse_button_down_posts: posts.mouse_button_down_posts,
                mouse_button_up_posts: posts.mouse_button_up_posts,
                mouse_drag_posts: posts.mouse_drag_posts,
                mouse_scroll_posts: posts.mouse_scroll_posts,
                other_posts: posts.other_posts,
                last_post_at: posts.last_post_at,
                post_ms: posts.post_ms,
            });
    }
}

/// Classify before availability/guard checks so missing posts remain visible.
pub(super) fn mouse_kind(event: &MouseEvent) -> InputCallbackKind {
    match event {
        MouseEvent::Move { .. } => InputCallbackKind::MouseMove,
        MouseEvent::RelMove { .. } => InputCallbackKind::MouseRelativeMove,
        MouseEvent::LeftPressed
        | MouseEvent::RightPressed
        | MouseEvent::MiddlePressed
        | MouseEvent::Button4Pressed
        | MouseEvent::Button5Pressed => InputCallbackKind::MouseButtonDown,
        MouseEvent::LeftReleased
        | MouseEvent::RightReleased
        | MouseEvent::MiddleReleased
        | MouseEvent::Button4Released
        | MouseEvent::Button5Released => InputCallbackKind::MouseButtonUp,
        MouseEvent::VerticalScroll { .. }
        | MouseEvent::Scroll { .. }
        | MouseEvent::ScrollHighResolution { .. } => InputCallbackKind::MouseScroll,
    }
}
