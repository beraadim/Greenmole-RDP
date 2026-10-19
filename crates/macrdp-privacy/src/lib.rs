//! Best-effort exclusive control of the existing macOS login session.
//!
//! A separately supervised helper owns the input tap and black physical-display
//! panels. It never unlocks a session. Failures lock the session before releasing
//! those barriers; an unverified lock leaves the helper and its generation lease
//! alive. This is not a boundary against root, hostile Accessibility clients,
//! power controls, or macOS itself disabling input filtering.

use anyhow::Result;

#[cfg(target_os = "macos")]
mod native;
#[cfg(any(target_os = "macos", test))]
mod policy;
#[cfg(target_os = "macos")]
mod process;
#[cfg(any(target_os = "macos", test))]
mod protocol;

const HELPER_ARGUMENT: &str = "--macrdp-private-session-helper";

/// Owns the heartbeat lease for one admitted private connection. Dropping an
/// active guard requests a forced lock; use `finish` to report cleanup errors.
pub struct PrivateSessionGuard {
    #[cfg(target_os = "macos")]
    inner: process::ParentGuard,
}

/// Call before waking the login session or activating its virtual desktop.
/// Admission succeeds only after the helper has sampled the initial lock state,
/// acquired the generation lease, and installed both local barriers.
pub fn start_guard(remote_pid: u32, event_tag: i64) -> Result<PrivateSessionGuard> {
    anyhow::ensure!(
        remote_pid > 1 && event_tag != 0,
        "Invalid private-session routing identity"
    );
    anyhow::ensure!(
        remote_pid == std::process::id(),
        "Private-session guard must belong to its calling server worker"
    );
    anyhow::ensure!(
        !std::env::args_os().any(|arg| arg == HELPER_ARGUMENT),
        "Private-session helper entry hook was not dispatched"
    );
    #[cfg(target_os = "macos")]
    {
        Ok(PrivateSessionGuard {
            inner: process::ParentGuard::start(remote_pid, event_tag)?,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("Private sessions require macOS")
    }
}

impl PrivateSessionGuard {
    /// Inhibit remote input before calling this, and restore/remove the virtual
    /// display only after it succeeds. `force_lock` is for emergency recovery.
    pub fn finish(&mut self, force_lock: bool) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.inner.finish(force_lock)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = force_lock;
            anyhow::bail!("Private sessions require macOS")
        }
    }

    pub fn is_healthy(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.inner.is_healthy()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }
}

/// Lock without synthetic key presses or shell commands, then require positive
/// WindowServer confirmation. Intended only for active private-session recovery.
/// An error is never evidence that it is safe to expose the desktop.
pub fn lock_session_and_verify() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        native::lock_session_and_verify()
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("Private sessions require macOS")
    }
}

/// Probe the independent guard's generation lease without creating directories
/// or changing a running guard. A retained lease means local recovery is pending.
pub fn guard_recovery_pending() -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        native::ownership::recovery_pending()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}

/// Dispatch this before AppKit, logging initialization, or the normal server
/// helper. A matching invocation must exit after the returned result.
pub fn run_guard_helper_if_requested() -> Option<Result<()>> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARGUMENT)) {
        return None;
    }
    if arguments.next().is_some() {
        return Some(Err(anyhow::anyhow!(
            "Private-session helper accepts no extra arguments"
        )));
    }
    #[cfg(target_os = "macos")]
    {
        Some(native::run_helper())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(Err(anyhow::anyhow!(
            "Private-session helper requires macOS"
        )))
    }
}
