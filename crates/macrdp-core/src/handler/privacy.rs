//! Admission and release ordering for the independent local-session guard.
//!
//! This stays outside capture: capture can fail or never start at all. The guard
//! owns its own process group and retains local barriers if locking cannot be
//! verified. No typed content or routing tag belongs in diagnostic output.

use anyhow::{ensure, Context, Result};
use ironrdp_server::gfx::ConnectionOutcome;
use macrdp_privacy::PrivateSessionGuard;

trait Guard: Send {
    fn finish(&mut self, force_lock: bool) -> Result<()>;
    fn healthy(&self) -> bool;
}

impl Guard for PrivateSessionGuard {
    fn finish(&mut self, force_lock: bool) -> Result<()> {
        PrivateSessionGuard::finish(self, force_lock)
    }
    fn healthy(&self) -> bool {
        self.is_healthy()
    }
}

#[derive(Default)]
pub(super) struct PrivateSession {
    enabled: bool,
    guard: Option<Box<dyn Guard>>,
}

impl PrivateSession {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            guard: None,
        }
    }

    /// Must run before wake, input injection, virtual-display promotion or capture.
    pub(super) fn prepare(&mut self) -> Result<()> {
        self.prepare_with(|tag| {
            Ok(Box::new(macrdp_privacy::start_guard(
                std::process::id(),
                tag,
            )?))
        })
    }

    fn prepare_with(&mut self, start: impl FnOnce(i64) -> Result<Box<dyn Guard>>) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        ensure!(
            self.guard.is_none(),
            "Previous private session protection is still pending"
        );
        let mut bytes = [0u8; 8];
        getrandom::getrandom(&mut bytes).context("Cannot create private input routing tag")?;
        // Nonzero distinguishes guarded events from the normal injector. This
        // is routing metadata, not an authentication boundary against local code.
        let tag = (i64::from_ne_bytes(bytes) & i64::MAX) | 1;
        let guard = start(tag).context("Private session protection is unavailable")?;
        ensure!(
            guard.healthy(),
            "Private session guard did not become ready"
        );
        self.guard = Some(guard);
        macrdp_input::set_remote_event_tag(tag);
        tracing::info!("Private session guard admitted remote control");
        Ok(())
    }

    pub(super) fn input_allowed(&self) -> bool {
        !self.enabled || self.guard.as_ref().is_some_and(|guard| guard.healthy())
    }

    pub(super) fn redact_input_diagnostics(&self) -> bool {
        self.enabled
    }

    /// Stop posting remote input before calling. Only a deliberate client
    /// disconnect can preserve an initially unlocked desktop. Network failures,
    /// cancellation, Stop and unknown outcomes always require a verified lock.
    pub(super) fn finish(&mut self, outcome: Option<ConnectionOutcome>) -> Result<()> {
        if let Some(guard) = &mut self.guard {
            guard.finish(outcome != Some(ConnectionOutcome::ClientDisconnect))?;
            // Do not take/drop the guard or clear routing state on failure: its
            // barriers must remain alive and subsequent admission must fail.
            self.guard.take();
            macrdp_input::set_remote_event_tag(0);
            tracing::info!("Private session protection completed before local desktop restoration");
        }
        Ok(())
    }
}

impl Drop for PrivateSession {
    fn drop(&mut self) {
        if let Err(error) = self.finish(None) {
            tracing::error!(%error, "Private session guard retains protection while lock recovery is pending");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct TestGuard {
        calls: Arc<Mutex<Vec<bool>>>,
        fails: bool,
    }
    impl Guard for TestGuard {
        fn finish(&mut self, force: bool) -> Result<()> {
            self.calls.lock().unwrap().push(force);
            ensure!(!self.fails, "Synthetic lock verification failed");
            Ok(())
        }
        fn healthy(&self) -> bool {
            true
        }
    }

    #[test]
    fn only_deliberate_disconnect_may_restore_an_initially_unlocked_session() {
        for (outcome, force) in [
            (Some(ConnectionOutcome::ClientDisconnect), false),
            (Some(ConnectionOutcome::PeerEof), true),
            (Some(ConnectionOutcome::ServerShutdown), true),
            (Some(ConnectionOutcome::CaptureError), true),
            (None, true),
        ] {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let mut session = PrivateSession {
                enabled: true,
                guard: Some(Box::new(TestGuard {
                    calls: calls.clone(),
                    fails: false,
                })),
            };
            session.finish(outcome).unwrap();
            assert!(session.guard.is_none());
            assert_eq!(*calls.lock().unwrap(), vec![force]);
        }
    }

    #[test]
    fn failed_relock_retains_owner_and_rejects_new_admission() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut session = PrivateSession {
            enabled: true,
            guard: Some(Box::new(TestGuard { calls, fails: true })),
        };
        assert!(session.finish(None).is_err());
        assert!(session.guard.is_some());
        assert!(session
            .prepare_with(|_| panic!("must not replace pending guard"))
            .is_err());
        // Allow synthetic cleanup without touching the host's input or lock state.
        session.guard.take();
    }

    #[test]
    fn disabled_mode_never_starts_guard_and_failed_admission_never_allows_input() {
        let mut normal = PrivateSession::default();
        normal
            .prepare_with(|_| panic!("normal mode must not start a guard"))
            .unwrap();
        assert!(normal.input_allowed());
        let mut private = PrivateSession::new(true);
        assert!(!private.input_allowed());
        assert!(private
            .prepare_with(|_| anyhow::bail!("synthetic capability failure"))
            .is_err());
        assert!(!private.input_allowed());
    }
}
