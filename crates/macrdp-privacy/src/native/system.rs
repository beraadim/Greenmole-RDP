use crate::policy::LockState;
use anyhow::{ensure, Context, Result};
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFGetTypeID, CFRelease};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use std::time::{Duration, Instant};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}
#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

/// Carbon marks this query not thread-safe. Call only from the helper main
/// thread, never its input callback, lease reader, or background watchdog.
pub(super) fn secure_event_input_enabled() -> bool {
    unsafe { IsSecureEventInputEnabled() != 0 }
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: *const std::ffi::c_void) -> bool;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(
        number: *const std::ffi::c_void,
        kind: isize,
        value: *mut std::ffi::c_void,
    ) -> bool;
}

/// The lock key is private and may disappear. Absence, non-Boolean values, and
/// unavailable GUI sessions are deliberately Unknown, never inferred unlocked.
pub(super) fn lock_state() -> LockState {
    let dictionary = unsafe { CGSessionCopyCurrentDictionary() };
    if dictionary.is_null() {
        return LockState::Unknown;
    }
    let key = CFString::new("CGSSessionScreenIsLocked");
    let value = unsafe { CFDictionaryGetValue(dictionary, key.as_concrete_TypeRef().cast()) };
    let state = if value.is_null() {
        LockState::Unknown
    } else if unsafe { CFGetTypeID(value) } == unsafe { CFBooleanGetTypeID() } {
        if unsafe { CFBooleanGetValue(value) } {
            LockState::Locked
        } else {
            LockState::Unlocked
        }
    } else if unsafe { CFGetTypeID(value) } == unsafe { CFNumberGetTypeID() } {
        let mut number = -1i64;
        // kCFNumberSInt64Type = 4; only exact Boolean-like values count.
        if unsafe { CFNumberGetValue(value, 4, (&mut number as *mut i64).cast()) } {
            match number {
                0 => LockState::Unlocked,
                1 => LockState::Locked,
                _ => LockState::Unknown,
            }
        } else {
            LockState::Unknown
        }
    } else {
        LockState::Unknown
    };
    unsafe { CFRelease(dictionary.cast()) };
    state
}

/// Resolve only the lock entry point; there is no private unlock API here. Keep
/// the framework loaded for the lifetime of the function pointer. Checking it
/// during admission has no lock/screen-saver side effects.
pub(super) struct LockApi {
    handle: usize,
    entry: usize,
}

impl LockApi {
    pub(super) fn load() -> Result<Self> {
        let mut handle = std::ptr::null_mut();
        for path in [
            b"/System/Library/PrivateFrameworks/login.framework/login\0".as_slice(),
            b"/System/Library/PrivateFrameworks/login.framework/Versions/A/login\0".as_slice(),
        ] {
            handle =
                unsafe { libc::dlopen(path.as_ptr().cast(), libc::RTLD_LOCAL | libc::RTLD_NOW) };
            if !handle.is_null() {
                break;
            }
        }
        ensure!(
            !handle.is_null(),
            "This macOS version does not provide the required screen-lock framework"
        );
        let entry = unsafe { libc::dlsym(handle, b"SACLockScreenImmediate\0".as_ptr().cast()) };
        if entry.is_null() {
            unsafe { libc::dlclose(handle) };
            anyhow::bail!(
                "This macOS version does not provide the required immediate screen-lock capability"
            );
        }
        Ok(Self {
            handle: handle as usize,
            entry: entry as usize,
        })
    }

    pub(super) fn request(&self) {
        // This private function's return value is not treated as success. Only
        // the independently queried WindowServer lock state permits release.
        let lock: unsafe extern "C" fn() = unsafe { std::mem::transmute(self.entry) };
        unsafe {
            lock();
        }
    }
}

impl Drop for LockApi {
    fn drop(&mut self) {
        unsafe {
            libc::dlclose(self.handle as *mut _);
        }
    }
}

pub(crate) fn lock_session_and_verify() -> Result<()> {
    let api = LockApi::load().context("Cannot safely recover the private session")?;
    api.request();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if lock_state() == LockState::Locked {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    anyhow::bail!("macOS did not positively confirm screen lock; private-session recovery must remain pending")
}

pub(super) fn terminate_owned_worker(pid: u32) {
    // Refuse to signal after reparenting. macOS has no pidfd-style atomic
    // check-and-signal API here; the narrow parent-exit/PID-reuse race between
    // getppid and kill is a platform limitation, not an authentication boundary.
    if unsafe { libc::getppid() } == pid as i32 && pid > 1 {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}
