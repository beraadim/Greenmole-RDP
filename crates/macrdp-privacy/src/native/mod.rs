mod appkit;
pub(crate) mod ownership;
mod system;
mod tap;

use crate::policy::{keyboard_barrier_failure, Action, EndReason, LockState, Recovery};
use crate::protocol::{read_message, write_message, Message};
use anyhow::{ensure, Context, Result};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use objc2::rc::autoreleasepool;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub(crate) use system::lock_session_and_verify;
const LEASE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(25);

struct Shared {
    epoch: Instant,
    last_heartbeat_ms: AtomicU64,
    last_main_check_ms: AtomicU64,
    reason: AtomicU8,
    finish: AtomicU8,
    inhibited: AtomicBool,
    active: AtomicBool,
    stop: AtomicBool,
    tap_address: AtomicUsize,
}

impl Shared {
    fn new() -> Self {
        Self {
            epoch: Instant::now(),
            last_heartbeat_ms: AtomicU64::new(0),
            last_main_check_ms: AtomicU64::new(0),
            reason: AtomicU8::new(0),
            finish: AtomicU8::new(0),
            inhibited: AtomicBool::new(false),
            active: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            tap_address: AtomicUsize::new(0),
        }
    }
    fn heartbeat(&self) {
        self.last_heartbeat_ms
            .store(self.epoch.elapsed().as_millis() as u64, Ordering::Release);
    }
    fn expired(&self) -> bool {
        self.epoch
            .elapsed()
            .as_millis()
            .saturating_sub(self.last_heartbeat_ms.load(Ordering::Acquire) as u128)
            >= LEASE.as_millis()
    }
    fn main_checked(&self) {
        self.last_main_check_ms
            .store(self.epoch.elapsed().as_millis() as u64, Ordering::Release);
    }
    fn main_stalled(&self) -> bool {
        self.epoch
            .elapsed()
            .as_millis()
            .saturating_sub(self.last_main_check_ms.load(Ordering::Acquire) as u128)
            >= Duration::from_secs(2).as_millis()
    }
    fn fail(&self, reason: EndReason) {
        self.inhibited.store(true, Ordering::Release);
        let _ = self
            .reason
            .compare_exchange(0, reason.code(), Ordering::AcqRel, Ordering::Acquire);
    }
    fn failure(&self) -> Option<EndReason> {
        match self.reason.load(Ordering::Acquire) {
            0 => None,
            1 => Some(EndReason::PipeClosed),
            2 => Some(EndReason::Malformed),
            3 => Some(EndReason::HeartbeatExpired),
            4 => Some(EndReason::WorkerExited),
            5 => Some(EndReason::TapLost),
            6 => Some(EndReason::DisplayChanged),
            7 => Some(EndReason::SecureInput),
            _ => Some(EndReason::GuardStalled),
        }
    }
}

pub(crate) fn run_helper() -> Result<()> {
    ensure!(
        unsafe { libc::getpgrp() } == std::process::id() as i32,
        "Private-session helper must have its own process group"
    );
    let shared = Arc::new(Shared::new());
    let (start_tx, start_rx) = mpsc::sync_channel(1);
    let input_state = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("private-session-lease-reader".into())
        .spawn(move || {
            let mut input = io::stdin().lock();
            let start = read_message(&mut input);
            if let Ok(Some(Message::Start { remote_pid, tag })) = start {
                input_state.heartbeat();
                if start_tx.send(Ok((remote_pid, tag))).is_err() {
                    return;
                }
            } else {
                let _ = start_tx.send(Err(anyhow::anyhow!(
                    "Invalid private-session startup frame"
                )));
                return;
            }
            loop {
                match read_message(&mut input) {
                    Ok(Some(Message::Heartbeat)) => input_state.heartbeat(),
                    Ok(Some(Message::Finish { force_lock })) => {
                        input_state.heartbeat();
                        input_state.inhibited.store(true, Ordering::Release);
                        // Force-lock cannot be downgraded by a later message.
                        input_state
                            .finish
                            .fetch_max(if force_lock { 2 } else { 1 }, Ordering::AcqRel);
                    }
                    Ok(None) => {
                        input_state.fail(EndReason::PipeClosed);
                        return;
                    }
                    _ => {
                        input_state.fail(EndReason::Malformed);
                        return;
                    }
                }
            }
        })
        .context("Could not start independent private-session lease reader")?;
    let (remote_pid, tag) = start_rx
        .recv_timeout(LEASE)
        .context("Timed out receiving private-session startup")??;
    ensure!(
        remote_pid == unsafe { libc::getppid() } as u32 && remote_pid > 1,
        "Private-session helper routing identity does not match its parent"
    );
    // Acquisition precedes snapshot and all AppKit/display/input work. No new
    // generation can bypass a previous helper retaining unverified barriers.
    let generation = ownership::acquire()?;
    let initial = system::lock_state();
    ensure!(!system::secure_event_input_enabled(),
        "Private-session admission refused: macOS Secure Event Input bypasses keyboard filtering; no guarded session was started");
    let lock = system::LockApi::load()?;
    let background_lock = system::LockApi::load()?;
    let watch_state = Arc::clone(&shared);
    // This thread remains independent of AppKit and the tap's run loop. A slow
    // native operation cannot indefinitely extend the server's heartbeat lease.
    let watchdog = std::thread::Builder::new()
        .name("private-session-watchdog".into())
        .spawn(move || {
            let mut requested = false;
            let mut last_lock = Instant::now() - Duration::from_secs(2);
            while !watch_state.stop.load(Ordering::Acquire) {
                if unsafe { libc::getppid() } != remote_pid as i32 {
                    watch_state.fail(EndReason::WorkerExited);
                }
                if watch_state.expired() {
                    watch_state.fail(EndReason::HeartbeatExpired);
                }
                let active = watch_state.active.load(Ordering::Acquire);
                if active && !tap::enabled(watch_state.tap_address.load(Ordering::Acquire)) {
                    watch_state.fail(EndReason::TapLost);
                }
                if active && watch_state.main_stalled() {
                    watch_state.fail(EndReason::GuardStalled);
                }
                if !active && watch_state.failure().is_some() {
                    // No session has been admitted yet. AppKit setup may itself be
                    // stuck after creating a tap: process death safely releases any
                    // partial barriers without locking an otherwise idle desktop.
                    unsafe {
                        libc::_exit(70);
                    }
                }
                if watch_state.active.load(Ordering::Acquire)
                    && watch_state.failure().is_some()
                    && (!requested || last_lock.elapsed() >= Duration::from_secs(1))
                {
                    // Request lock before terminating the worker. The tap and cover
                    // owner remain alive until the main loop positively verifies it.
                    background_lock.request();
                    system::terminate_owned_worker(remote_pid);
                    requested = true;
                    last_lock = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
        .context("Could not start independent private-session watchdog")?;
    let tap = tap::InputBarrier::create(Arc::clone(&shared), remote_pid, tag)?;
    shared.tap_address.store(tap.address(), Ordering::Release);
    let mut covers = autoreleasepool(|_| appkit::Covers::create())?;
    autoreleasepool(|_| covers.refresh())?;
    ensure!(
        tap.is_enabled(),
        "Private-session input barrier disappeared during admission"
    );
    ensure!(
        !system::secure_event_input_enabled(),
        "Private-session admission refused: macOS Secure Event Input became active during setup"
    );
    ensure!(
        shared.failure().is_none() && !shared.expired(),
        "Private-session owner disappeared during admission"
    );
    shared.main_checked();
    shared.active.store(true, Ordering::Release);
    let result = helper_loop(&shared, remote_pid, initial, &lock, &tap, &mut covers);
    shared.active.store(false, Ordering::Release);
    shared.stop.store(true, Ordering::Release);
    let _ = watchdog.join();
    let locked = result?;
    drop(covers);
    drop(tap);
    drop(generation);
    // The reply cannot claim cleanup while either native barrier or the
    // generation lease remains owned by this helper.
    let _ = write_message(&mut io::stdout().lock(), Message::Finished { locked });
    Ok(())
}

fn helper_loop(
    shared: &Shared,
    remote_pid: u32,
    initial: LockState,
    lock: &system::LockApi,
    tap: &tap::InputBarrier,
    covers: &mut appkit::Covers,
) -> Result<bool> {
    let mut output = io::stdout().lock();
    // A stalled/dead parent cannot block the main loop on a full reply pipe.
    let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
    ensure!(
        flags >= 0
            && unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == 0,
        "Could not bound private-session reply writes"
    );
    if write_message(&mut output, Message::Ready { initial }).is_err() {
        shared.fail(EndReason::PipeClosed);
    }
    let mut last_reply = Instant::now();
    let mut last_covers = Instant::now();
    let mut recovery: Option<Recovery> = None;
    let mut reported_fault = false;
    let mut last_lock = Instant::now() - Duration::from_secs(2);
    let mut recovery_started = None;
    let mut last_pending_warning = None;
    loop {
        if let Some(reason) =
            keyboard_barrier_failure(tap.is_enabled(), system::secure_event_input_enabled())
        {
            shared.fail(reason);
        }
        shared.main_checked();
        if last_covers.elapsed() >= Duration::from_millis(100) {
            if autoreleasepool(|_| covers.refresh()).is_err() {
                shared.fail(EndReason::DisplayChanged);
            }
            last_covers = Instant::now();
        }
        let failure = shared.failure();
        let finish = shared.finish.load(Ordering::Acquire);
        if recovery.is_none() && (failure.is_some() || finish != 0) {
            let reason = failure.unwrap_or(EndReason::Normal {
                force_lock: finish == 2,
            });
            let (state, actions) = Recovery::begin(initial, reason);
            eprintln!("Private-session recovery started (reason {}, initial {:?}); remote input inhibited", reason.code(), initial);
            apply_actions(shared, remote_pid, lock, &actions);
            last_lock = Instant::now();
            recovery_started = Some(Instant::now());
            recovery = Some(state);
        } else if let Some(state) = recovery.as_mut() {
            if failure.is_some() {
                apply_actions(shared, remote_pid, lock, &state.escalate());
            } else if finish == 2 {
                apply_actions(shared, remote_pid, lock, &state.force_lock());
            }
        }
        if let Some(state) = recovery.as_mut() {
            // The input/lease watchdog can discover failure while the main
            // loop services AppKit. Recheck before any normal-unlocked release.
            if shared.failure().is_some() {
                apply_actions(shared, remote_pid, lock, &state.escalate());
            }
            let locked = system::lock_state() == LockState::Locked;
            if state.poll(locked) == Action::ReleaseBarriers {
                // Local control becomes available only after required locking.
                // Caller must not restore/remove its virtual display sooner.
                covers.close();
                shared.active.store(false, Ordering::Release);
                shared.stop.store(true, Ordering::Release);
                return Ok(locked);
            }
            if last_lock.elapsed() >= Duration::from_secs(1) {
                lock.request();
                last_lock = Instant::now();
            }
            if recovery_started.is_some_and(|at| at.elapsed() >= Duration::from_secs(8))
                && last_pending_warning
                    .map_or(true, |at: Instant| at.elapsed() >= Duration::from_secs(30))
            {
                eprintln!("Private-session recovery is still waiting for positive macOS lock confirmation; local covers, input blocking and generation lease remain active");
                last_pending_warning = Some(Instant::now());
            }
        }
        if failure.is_some() && !reported_fault {
            let _ = write_message(
                &mut output,
                Message::Fault {
                    code: failure.unwrap().code(),
                },
            );
            reported_fault = true;
        } else if last_reply.elapsed() >= Duration::from_secs(1) && !reported_fault {
            if write_message(&mut output, Message::Alive).is_err() {
                shared.fail(EndReason::PipeClosed);
            }
            last_reply = Instant::now();
        }
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, POLL, false);
    }
}

fn apply_actions(shared: &Shared, remote_pid: u32, lock: &system::LockApi, actions: &[Action]) {
    for action in actions {
        match action {
            Action::InhibitRemote => shared.inhibited.store(true, Ordering::Release),
            Action::RequestLock => lock.request(),
            Action::TerminateWorker => system::terminate_owned_worker(remote_pid),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_heartbeat_does_not_extend_a_stalled_main_guard_check() {
        let mut shared = Shared::new();
        shared.epoch = Instant::now() - Duration::from_secs(3);
        shared.heartbeat();
        assert!(!shared.expired());
        assert!(shared.main_stalled());
        shared.main_checked();
        assert!(!shared.main_stalled());
        shared.epoch = Instant::now() - Duration::from_secs(10);
        assert!(shared.expired());
    }

    #[test]
    fn secure_input_fault_inhibits_immediately_without_native_effects() {
        let shared = Shared::new();
        shared.fail(EndReason::SecureInput);
        assert!(shared.inhibited.load(Ordering::Acquire));
        assert_eq!(shared.failure(), Some(EndReason::SecureInput));
        shared.fail(EndReason::TapLost);
        assert_eq!(
            shared.failure(),
            Some(EndReason::SecureInput),
            "retain first diagnostic cause"
        );
    }
}
