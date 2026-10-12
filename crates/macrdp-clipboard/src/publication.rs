//! Copy generations cover request, decoding and final publication. The mutex
//! serializes our own poller/publications; the native change count also detects
//! local copies made while image conversion or file I/O was running.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{pasteboard::PasteboardBridge, remote_formats::RemoteFormat};

pub(crate) const FORMAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CopyToken {
    pub generation: u64,
    change_count: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RemoteOffer {
    pub format: Option<RemoteFormat>,
    pub token: CopyToken,
}

pub(crate) struct PublicationState(Mutex<State>);

struct State {
    generation: u64,
    last_change_count: i64,
    request_deadline: Option<Instant>,
    work_deadline: Option<Instant>,
    active: bool,
}

impl PublicationState {
    pub fn new(change_count: i64) -> Self {
        Self(Mutex::new(State {
            generation: 0,
            last_change_count: change_count,
            request_deadline: None,
            work_deadline: None,
            active: true,
        }))
    }

    /// A panic during native publication leaves clipboard ownership uncertain.
    /// Recover the guard only to shut down safely, never to resume publishing.
    /// Keep the poison marker so every later access remains fail-closed too.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            state.active = false;
            state.request_deadline = None;
            state.work_deadline = None;
            state
        })
    }

    pub fn remote_copy(&self, pb: &PasteboardBridge) -> CopyToken {
        let mut state = self.lock();
        // A local copy already present before this remote offer is older. Record
        // it now, otherwise the next poll would incorrectly cancel this offer.
        if state.active {
            state.generation = state.generation.wrapping_add(1);
            state.last_change_count = pb.change_count();
        }
        CopyToken {
            generation: state.generation,
            change_count: state.last_change_count,
        }
    }

    pub fn local_copy_changed(&self, pb: &PasteboardBridge) -> bool {
        let mut state = self.lock();
        if !state.active {
            return false;
        }
        let current = pb.change_count();
        if current == state.last_change_count {
            return false;
        }
        state.last_change_count = current;
        state.generation = state.generation.wrapping_add(1);
        true
    }

    pub fn is_current(&self, token: CopyToken, pb: &PasteboardBridge) -> bool {
        let state = self.lock();
        state.active
            && state.generation == token.generation
            && pb.change_count() == token.change_count
    }

    pub fn publish(
        &self,
        token: CopyToken,
        pb: &PasteboardBridge,
        write: impl FnOnce(&PasteboardBridge) -> i64,
    ) -> bool {
        let mut state = self.lock();
        // Recheck after conversion/file completion, immediately before changing
        // NSPasteboard. macOS has no compare-and-swap pasteboard API; unrelated
        // applications are outside this lock, so keep this section minimal.
        if !state.active
            || token.generation != state.generation
            || pb.change_count() != token.change_count
        {
            tracing::debug!(
                generation = token.generation,
                "Discarded superseded clipboard publication"
            );
            return false;
        }
        let count = write(pb);
        if count < 0 {
            return false;
        }
        state.last_change_count = count;
        true
    }

    pub fn start_request(&self, now: Instant) {
        let mut state = self.lock();
        if state.active {
            state.request_deadline = Some(now + FORMAT_REQUEST_TIMEOUT);
        }
    }

    pub fn finish_request(&self) {
        self.lock().request_deadline = None;
    }

    pub fn begin_work(&self, now: Instant) {
        let mut state = self.lock();
        if state.active {
            state.work_deadline = Some(now + FORMAT_REQUEST_TIMEOUT);
        }
    }

    pub fn finish_work(&self) {
        self.lock().work_deadline = None;
    }

    /// Responses are untagged: after timeout only a new connection can safely
    /// establish request/response alignment. Never issue a replacement request
    /// and accidentally decode a late response using its new format.
    pub fn expire(&self, now: Instant) -> bool {
        let mut state = self.lock();
        if state.active
            && (state
                .request_deadline
                .is_some_and(|deadline| now >= deadline)
                || state.work_deadline.is_some_and(|deadline| now >= deadline))
        {
            state.active = false;
            state.request_deadline = None;
            state.work_deadline = None;
            return true;
        }
        false
    }

    pub fn stop(&self) {
        let mut state = self.lock();
        state.active = false;
        state.request_deadline = None;
        state.work_deadline = None;
    }

    pub fn is_active(&self) -> bool {
        self.lock().active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_during_publication_permanently_invalidates_state_without_poison_panics() {
        let pb = PasteboardBridge::isolated();
        pb.write_string("existing");
        let state = PublicationState::new(pb.change_count());
        let token = state.remote_copy(&pb);
        state.start_request(Instant::now());
        state.begin_work(Instant::now());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.publish(token, &pb, |_| panic!("native publication failed"));
        }));
        assert!(result.is_err());
        assert!(state.0.is_poisoned());
        assert!(!state.is_active());
        state.stop();
        state.start_request(Instant::now());
        state.begin_work(Instant::now());
        state.finish_request();
        state.finish_work();
        assert!(!state.expire(Instant::now() + FORMAT_REQUEST_TIMEOUT));
        assert!(!state.local_copy_changed(&pb));
        let next = state.remote_copy(&pb);
        assert!(!state.is_current(next, &pb));
        assert!(!state.publish(next, &pb, |_| panic!("poisoned state resumed publication")));
        let guard = state.lock();
        assert!(guard.request_deadline.is_none());
        assert!(guard.work_deadline.is_none());
        assert_eq!(pb.read_string().as_deref(), Some("existing"));
    }

    #[test]
    fn local_copy_during_conversion_cannot_be_overwritten_even_before_polling() {
        let pb = PasteboardBridge::isolated();
        pb.write_string("before");
        let state = PublicationState::new(pb.change_count());
        let token = state.remote_copy(&pb);
        pb.write_string("new local copy");
        assert!(!state.publish(token, &pb, |pb| pb.write_string("stale remote")));
        assert_eq!(pb.read_string().as_deref(), Some("new local copy"));
    }

    #[test]
    fn newer_remote_offer_supersedes_work_and_successful_publication_does_not_echo() {
        let pb = PasteboardBridge::isolated();
        let state = PublicationState::new(pb.change_count());
        let stale = state.remote_copy(&pb);
        let current = state.remote_copy(&pb);
        assert!(!state.publish(stale, &pb, |_| panic!("stale write executed")));
        assert!(state.publish(current, &pb, |pb| pb.write_string("current")));
        assert!(!state.local_copy_changed(&pb));
    }

    #[test]
    fn missing_response_fails_closed_and_late_response_cannot_reopen_publication() {
        let pb = PasteboardBridge::isolated();
        let state = PublicationState::new(pb.change_count());
        let token = state.remote_copy(&pb);
        let now = Instant::now();
        state.start_request(now);
        assert!(!state.expire(now + FORMAT_REQUEST_TIMEOUT - Duration::from_millis(1)));
        assert!(state.expire(now + FORMAT_REQUEST_TIMEOUT));
        state.finish_request();
        assert!(!state.publish(token, &pb, |_| panic!("late write executed")));
        let next = state.remote_copy(&pb);
        assert!(!state.is_current(next, &pb));
        assert!(!state.expire(now + FORMAT_REQUEST_TIMEOUT));
    }
}
