//! Join platform posting observations to the current transport dispatch without
//! copying any input payload into diagnostics.

use super::MacInputHandler;
use ironrdp_server::gfx::NativeInputObservation;
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
                callback_blocked: result.blocked,
                callback_unavailable: result.unavailable,
                callback_failed: result.failed,
                post_attempts: posts.post_attempts,
                post_returns: posts.post_returns,
                post_errors: posts.post_errors,
                post_samples_dropped: posts.post_samples_dropped,
                last_post_at: posts.last_post_at,
                post_ms: posts.post_ms,
            });
    }
}
