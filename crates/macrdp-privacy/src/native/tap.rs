use super::Shared;
use crate::policy::{eligible_tap, permits_event, EndReason};
use anyhow::{ensure, Result};
use core_foundation_sys::base::CFRelease;
use core_foundation_sys::mach_port::{
    CFMachPortCreateRunLoopSource, CFMachPortInvalidate, CFMachPortRef,
};
use core_foundation_sys::runloop::{
    kCFRunLoopCommonModes, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRemoveSource,
    CFRunLoopSourceRef,
};
use std::ffi::c_void;
use std::sync::{atomic::Ordering, Arc};

type Event = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TapInformation {
    id: u32,
    point: u32,
    options: u32,
    mask: u64,
    process: i32,
    target: i32,
    enabled: bool,
    minimum_latency: f32,
    average_latency: f32,
    maximum_latency: f32,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        point: u32,
        placement: u32,
        options: u32,
        mask: u64,
        callback: unsafe extern "C" fn(*mut c_void, u32, Event, *mut c_void) -> Event,
        context: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapIsEnabled(port: CFMachPortRef) -> bool;
    fn CGEventGetIntegerValueField(event: Event, field: u32) -> i64;
    fn CGGetEventTapList(maximum: u32, taps: *mut TapInformation, count: *mut u32) -> i32;
    fn CGPreflightListenEventAccess() -> bool;
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

struct Callback {
    shared: Arc<Shared>,
    remote_pid: u32,
    tag: i64,
}

unsafe extern "C" fn callback(
    _proxy: *mut c_void,
    kind: u32,
    event: Event,
    context: *mut c_void,
) -> Event {
    // No allocation, locks, logging, I/O, or native recovery in this callback.
    // A disabled tap is a security failure, never silently re-enabled.
    let state = &*(context as *const Callback);
    if kind == 0xffff_fffe || kind == 0xffff_ffff {
        state.shared.fail(EndReason::TapLost);
        return std::ptr::null_mut();
    }
    if event.is_null() {
        return std::ptr::null_mut();
    }
    let pid = CGEventGetIntegerValueField(event, 41);
    let tag = CGEventGetIntegerValueField(event, 42);
    if permits_event(
        state.remote_pid,
        state.tag,
        pid,
        tag,
        state.shared.inhibited.load(Ordering::Acquire),
    ) {
        event
    } else {
        std::ptr::null_mut()
    }
}

pub(super) fn enabled(port: usize) -> bool {
    port != 0 && unsafe { CGEventTapIsEnabled(port as CFMachPortRef) }
}

pub(super) struct InputBarrier {
    port: CFMachPortRef,
    source: CFRunLoopSourceRef,
    _callback: Box<Callback>,
}

impl InputBarrier {
    pub(super) fn create(shared: Arc<Shared>, remote_pid: u32, tag: i64) -> Result<Self> {
        ensure!(unsafe { AXIsProcessTrusted() } && unsafe { CGPreflightListenEventAccess() },
            "Private sessions require existing Accessibility and Input Monitoring permissions; admission was refused without prompting");
        let mut callback_state = Box::new(Callback {
            shared,
            remote_pid,
            tag,
        });
        let context = (&mut *callback_state as *mut Callback).cast();
        // A session tap filters HID and injected events entering this login
        // session. HID-level taps require root on supported macOS versions;
        // macrdp never asks users to run the full server as root.
        let port = unsafe { CGEventTapCreate(1, 0, 0, u64::MAX, callback, context) };
        ensure!(
            !port.is_null(),
            "macOS refused the active session input barrier; private-session admission was refused"
        );
        let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0) };
        if source.is_null() {
            unsafe {
                CFMachPortInvalidate(port);
                CFRelease(port.cast());
            }
            anyhow::bail!("Could not attach the private-session input barrier to its run loop");
        }
        let barrier = Self {
            port,
            source,
            _callback: callback_state,
        };
        unsafe {
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
        }
        ensure!(
            barrier.is_enabled(),
            "macOS did not enable the private-session input barrier"
        );
        // CGEventTapCreate may silently strip keyboard bits when permissions are
        // insufficient. Positive tap-list verification is part of admission.
        let mut entries = [TapInformation::default(); 256];
        let mut count = 0;
        ensure!(
            unsafe { CGGetEventTapList(entries.len() as u32, entries.as_mut_ptr(), &mut count) }
                == 0
                && count <= entries.len() as u32,
            "Could not verify the input barrier's effective event mask"
        );
        ensure!(
            entries[..count as usize].iter().any(|entry| eligible_tap(
                entry.point,
                entry.options,
                entry.mask,
                entry.process,
                entry.enabled,
                std::process::id()
            )),
            "macOS did not authorize all required keyboard, pointer, scrolling and tablet filters"
        );
        Ok(barrier)
    }

    pub(super) fn address(&self) -> usize {
        self.port as usize
    }
    pub(super) fn is_enabled(&self) -> bool {
        enabled(self.address())
    }
}

impl Drop for InputBarrier {
    fn drop(&mut self) {
        unsafe {
            CFRunLoopRemoveSource(CFRunLoopGetCurrent(), self.source, kCFRunLoopCommonModes);
            CFMachPortInvalidate(self.port);
            CFRelease(self.source.cast());
            CFRelease(self.port.cast());
        }
    }
}
