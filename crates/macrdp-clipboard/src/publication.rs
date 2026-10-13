//! Copy generations cover request, decoding and final publication. The mutex
//! protects bookkeeping, never native calls; the native change count also detects
//! local copies made while image conversion or file I/O was running.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{pasteboard::PasteboardBridge, remote_formats::RemoteFormat};

pub(crate) const FORMAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CopyToken {
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RemoteOffer {
    pub format: Option<RemoteFormat>,
    /// Optional second flavor of the same copy. It is requested only after the
    /// first response, because CLIPRDR format responses have no request ID.
    pub text_fallback: Option<RemoteFormat>,
    pub token: CopyToken,
}

pub(crate) struct PublicationState(Mutex<State>);

struct State {
    generation: u64,
    last_change_count: i64,
    request_deadline: Option<Instant>,
    work_deadline: Option<Instant>,
    active: bool,
    publishing: bool,
}

struct NativePublication<'a>(&'a PublicationState);

impl Drop for NativePublication<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.publishing = false;
        if std::thread::panicking() {
            state.active = false;
            state.request_deadline = None;
            state.work_deadline = None;
        }
    }
}

impl PublicationState {
    pub fn new(change_count: i64) -> Self {
        Self(Mutex::new(State {
            generation: 0,
            last_change_count: change_count,
            request_deadline: None,
            work_deadline: None,
            active: true,
            publishing: false,
        }))
    }

    /// Recover poisoned bookkeeping only to shut down safely, never to resume.
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
        let count = pb.change_count();
        let mut state = self.lock();
        // A local copy already present before this remote offer is older. Record
        // it now, otherwise the next poll would incorrectly cancel this offer.
        if state.active {
            state.generation = state.generation.wrapping_add(1);
            state.last_change_count = count;
        }
        CopyToken {
            generation: state.generation,
        }
    }

    /// The RDP callback cannot perform NSPasteboard IPC. Use the last count
    /// observed by the I/O worker; an unobserved local copy wins conservatively
    /// if the worker cannot establish its ordering relative to this offer.
    pub fn remote_copy_cached(&self) -> CopyToken {
        let mut state = self.lock();
        if state.active {
            state.generation = state.generation.wrapping_add(1);
        }
        CopyToken {
            generation: state.generation,
        }
    }

    pub fn local_copy_changed(&self, pb: &PasteboardBridge) -> bool {
        self.observe_local_copy(|| pb.change_count())
    }

    fn observe_local_copy(&self, read_count: impl FnOnce() -> i64) -> bool {
        let generation = {
            let state = self.lock();
            if !state.active || state.publishing {
                return false;
            }
            state.generation
        };
        let current = read_count();
        let mut state = self.lock();
        if !state.active || state.publishing || state.generation != generation {
            return false;
        }
        if current == state.last_change_count {
            return false;
        }
        state.last_change_count = current;
        state.generation = state.generation.wrapping_add(1);
        true
    }

    pub fn is_current(&self, token: CopyToken, pb: &PasteboardBridge) -> bool {
        {
            let state = self.lock();
            if !state.active || state.generation != token.generation {
                return false;
            }
        }
        let current = pb.change_count();
        let state = self.lock();
        state.active && state.generation == token.generation && current == state.last_change_count
    }

    pub fn publish(
        &self,
        token: CopyToken,
        pb: &PasteboardBridge,
        write: impl FnOnce(&PasteboardBridge) -> i64,
    ) -> bool {
        {
            let state = self.lock();
            if !state.active || state.publishing || token.generation != state.generation {
                return false;
            }
        }
        // Even changeCount can wait for the pasteboard service. Do not hold a
        // control-state lock while obtaining this final native snapshot.
        let current = pb.change_count();
        let mut state = self.lock();
        // Recheck after conversion/file completion, immediately before changing
        // NSPasteboard. macOS has no compare-and-swap pasteboard API; unrelated
        // applications are outside this lock, so keep this section minimal.
        if !state.active
            || state.publishing
            || token.generation != state.generation
            || current != state.last_change_count
        {
            tracing::debug!(
                generation = token.generation,
                "Discarded superseded clipboard publication"
            );
            return false;
        }
        state.publishing = true;
        drop(state);
        // Never block the watchdog, enqueue, or teardown on an AppKit write.
        // A write already admitted here cannot be cancelled; subsequent writes
        // remain disabled if stop/expire occurs while it is in native code.
        let _publication = NativePublication(self);
        let count = write(pb);
        if count < 0 {
            return false;
        }
        // A newer remote offer may have arrived during this write. Preserve
        // its generation but accept this known local publication as its new
        // native baseline. Otherwise our own write would invalidate that offer.
        let mut state = self.lock();
        if state.last_change_count <= count {
            state.last_change_count = count;
        }
        drop(state);
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
    fn cached_remote_offer_preserves_an_unobserved_conflicting_local_copy() {
        let pb = PasteboardBridge::isolated();
        let state = PublicationState::new(pb.change_count());
        pb.write_string("local copy whose ordering is unknown");
        let token = state.remote_copy_cached();
        assert!(!state.is_current(token, &pb));
        assert!(!state.publish(token, &pb, |_| panic!("overwrote an unobserved local copy")));
        assert!(state.local_copy_changed(&pb));
        let next = state.remote_copy_cached();
        assert!(state.publish(next, &pb, |pb| pb.write_string("next remote copy")));
    }

    #[test]
    fn stalled_native_metadata_does_not_block_watchdog_stop_or_remote_offer_admission() {
        let state = std::sync::Arc::new(PublicationState::new(10));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let observer = state.clone();
        let thread = std::thread::spawn(move || {
            observer.observe_local_copy(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                11
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(state.remote_copy_cached().generation, 1);
        state.begin_work(Instant::now() - FORMAT_REQUEST_TIMEOUT);
        assert!(state.expire(Instant::now()));
        state.stop();
        release_tx.send(()).unwrap();
        assert!(
            !thread.join().unwrap(),
            "late metadata revived the clipboard"
        );
        assert!(!state.is_active());
    }

    #[test]
    fn newer_remote_offer_survives_a_previously_admitted_native_publication() {
        for local_copy_after_write in [false, true] {
            let pb = PasteboardBridge::isolated();
            pb.write_string("initial");
            let state = PublicationState::new(pb.change_count());
            let older = state.remote_copy(&pb);
            let mut newer = None;
            assert!(state.publish(older, &pb, |pb| {
                if local_copy_after_write {
                    let count = pb.write_string("older remote");
                    pb.write_string("new local copy before next remote offer");
                    newer = Some(state.remote_copy(pb));
                    count
                } else {
                    newer = Some(state.remote_copy(pb));
                    pb.write_string("older remote")
                }
            }));
            let newer = newer.unwrap();
            assert!(state.is_current(newer, &pb));
            assert!(state.publish(newer, &pb, |pb| pb.write_string("newer remote")));
            assert_eq!(pb.read_string().as_deref(), Some("newer remote"));
            assert!(
                !state.local_copy_changed(&pb),
                "publication was echoed as a local copy"
            );
        }
    }

    #[test]
    fn stalled_native_publication_does_not_block_watchdog_or_stop() {
        let pb = PasteboardBridge::isolated();
        let state = std::sync::Arc::new(PublicationState::new(pb.change_count()));
        let token = state.remote_copy(&pb);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let publisher = state.clone();
        let handle = std::thread::spawn(move || {
            publisher.publish(token, &pb, |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                -1
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        state.begin_work(Instant::now() - FORMAT_REQUEST_TIMEOUT);
        assert!(state.expire(Instant::now()));
        state.stop();
        assert!(!state.is_active());
        release_tx.send(()).unwrap();
        assert!(!handle.join().unwrap());
        assert!(!state.is_active(), "native completion reopened publication");
    }

    #[test]
    fn panic_during_native_publication_permanently_invalidates_state_without_holding_state_lock() {
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
        assert!(
            !state.0.is_poisoned(),
            "native writes must not hold the state mutex"
        );
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
