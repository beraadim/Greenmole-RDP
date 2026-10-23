//! Assertion ownership independent of IOKit, so cleanup can be tested without
//! waking a display or creating/releasing any real power assertion.

use super::{
    IOPMAssertionID, IOReturn, K_IORETURN_BAD_ARGUMENT, K_IORETURN_NOT_FOUND, K_IORETURN_SUCCESS,
};
use std::time::Instant;

#[derive(Default)]
pub(super) struct WakeLease {
    pub display_sleep_assertion_id: Option<IOPMAssertionID>,
    pub user_activity_assertion_id: Option<IOPMAssertionID>,
    pub last_user_activity: Option<Instant>,
}

impl WakeLease {
    pub(super) fn release_with(&mut self, mut release: impl FnMut(IOPMAssertionID) -> IOReturn) {
        self.last_user_activity = None;
        release_owned(
            &mut self.display_sleep_assertion_id,
            "display_sleep",
            &mut release,
        );
        release_owned(
            &mut self.user_activity_assertion_id,
            "user_activity",
            &mut release,
        );
        // Report the real ownership result, never a disconnect's intention.
        tracing::info!(target: "macrdp_transition", component = "wake_lease",
            worker_pid = std::process::id(), phase = "cleanup",
            state = if self.display_sleep_assertion_id.is_none()
                && self.user_activity_assertion_id.is_none() { "complete" } else { "pending" },
            "Remote wake assertion cleanup observed");
    }
}

fn release_owned(
    owned: &mut Option<IOPMAssertionID>,
    assertion_kind: &'static str,
    release: &mut impl FnMut(IOPMAssertionID) -> IOReturn,
) {
    let Some(assertion_id) = *owned else { return };
    match release(assertion_id) {
        // UserIsActive assertions time out and release themselves. Their saved
        // ID may already be gone; cleanup is complete and this is not an error.
        // powerd's assertion lookup returns BadArgument for an expired ID too;
        // these IDs come only from successful IOKit creates/declarations.
        K_IORETURN_SUCCESS | K_IORETURN_NOT_FOUND | K_IORETURN_BAD_ARGUMENT => *owned = None,
        return_code => {
            // Do not lose ownership of a potentially active hold. Disconnect
            // cleanup still tries the other assertion, and Drop/a later cleanup
            // retries only this failed release. No input content is logged.
            tracing::warn!(
                assertion_kind,
                return_code,
                "Failed to release remote wake assertion; retaining ownership for cleanup retry"
            );
        }
    }
}

#[cfg(test)]
mod tests;
