//! Opt-in regression for the server's own AppKit window entering the AX worker.
//! Creates one disposable fixture; never accesses other applications through AX.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Native self-window regression skipped: macOS is required");
}

#[cfg(target_os = "macos")]
fn main() {
    if !std::env::args().any(|arg| arg == "--ignored") {
        eprintln!(
            "Native self-window regression skipped; pass --ignored to create a disposable window"
        );
        return;
    }
    objc2::rc::autoreleasepool(|_| native::run());
}

// Compile the production worker boundaries directly so this test exercises the
// actual safety checks without exposing native AX handles in the public API.
#[cfg(target_os = "macos")]
#[allow(dead_code, unused_imports)]
#[path = "../src/accessibility.rs"]
mod accessibility;
#[cfg(target_os = "macos")]
#[allow(dead_code)]
#[path = "../src/discovery.rs"]
mod discovery;
#[cfg(target_os = "macos")]
#[allow(dead_code)]
#[path = "../src/geometry.rs"]
mod geometry;
#[cfg(target_os = "macos")]
#[allow(unused_imports)]
#[path = "../src/process_filter.rs"]
mod process_filter;

#[cfg(target_os = "macos")]
mod native {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::number::CFNumber;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex};
    use core_foundation_sys::base::CFGetTypeID;
    use core_foundation_sys::dictionary::{CFDictionaryGetTypeID, CFDictionaryGetValue};
    use core_graphics::window::{
        kCGWindowListExcludeDesktopElements, kCGWindowNumber, kCGWindowOwnerPID,
        CGWindowListCopyWindowInfo,
    };
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEventMask, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_foundation::{ns_string, NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize};
    use std::sync::mpsc::{self, TryRecvError};
    use std::time::{Duration, Instant};

    struct Fixture(Retained<NSWindow>);

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Fixture and its retained NSWindow stay on the real main thread,
            // including assertion failures and worker timeouts.
            self.0.close();
        }
    }

    fn pump_events(application: &NSApplication) {
        objc2::rc::autoreleasepool(|_| {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
            // SAFETY: Foundation's immutable run-loop mode is valid for the
            // lifetime of the process; all AppKit calls run on the main thread.
            let mode = unsafe { NSDefaultRunLoopMode };
            if let Some(event) = application.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&until),
                mode,
                true,
            ) {
                application.sendEvent(&event);
            }
            application.updateWindows();
        });
    }

    fn fixture_is_in_unfiltered_window_list(pid: i32, window_number: i32) -> bool {
        // Use the same raw enumeration as discovery, before its PID filter. A
        // visible self-owned window must really be present for this regression
        // to pass; an empty WindowServer response must not produce a false pass.
        let raw = unsafe { CGWindowListCopyWindowInfo(kCGWindowListExcludeDesktopElements, 0) };
        assert!(!raw.is_null(), "WindowServer returned no window list");
        let info = unsafe { CFType::wrap_under_create_rule(raw.cast()) };
        assert_eq!(info.type_of(), unsafe { CFArrayGetTypeID() });
        let count = unsafe { CFArrayGetCount(info.as_CFTypeRef().cast()) };
        for index in 0..count {
            let dictionary = unsafe { CFArrayGetValueAtIndex(info.as_CFTypeRef().cast(), index) };
            if dictionary.is_null()
                || unsafe { CFGetTypeID(dictionary) } != unsafe { CFDictionaryGetTypeID() }
            {
                continue;
            }
            let owner =
                unsafe { CFDictionaryGetValue(dictionary.cast(), kCGWindowOwnerPID.cast()) };
            let number = unsafe { CFDictionaryGetValue(dictionary.cast(), kCGWindowNumber.cast()) };
            if owner.is_null()
                || number.is_null()
                || unsafe { CFGetTypeID(owner) } != CFNumber::type_id()
                || unsafe { CFGetTypeID(number) } != CFNumber::type_id()
            {
                continue;
            }
            let owner = unsafe { CFNumber::wrap_under_get_rule(owner.cast()) };
            let number = unsafe { CFNumber::wrap_under_get_rule(number.cast()) };
            if owner.to_i32() == Some(pid) && number.to_i32() == Some(window_number) {
                return true;
            }
        }
        false
    }

    pub fn run() {
        let main_thread =
            MainThreadMarker::new().expect("fixture must run on the real main thread");
        let application = NSApplication::sharedApplication(main_thread);
        application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        application.finishLaunching();
        // SAFETY: a standard, retained AppKit window is created on the main
        // thread. Disable release-on-close so the Retained value owns its lifetime.
        let fixture = Fixture(unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(main_thread),
                NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(320.0, 180.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        });
        fixture
            .0
            .setTitle(ns_string!("macrdp native regression fixture"));
        fixture.0.orderFrontRegardless();
        let pid = i32::try_from(std::process::id()).expect("invalid process ID");
        let window_number =
            i32::try_from(fixture.0.windowNumber()).expect("invalid fixture number");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture_is_in_unfiltered_window_list(pid, window_number) {
            assert!(
                Instant::now() < deadline,
                "fixture never appeared in WindowServer"
            );
            pump_events(&application);
        }

        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            // The original bug enumerated the server's own window, then called
            // its AppKit accessibility implementation synchronously off-thread.
            // Both production entry points must exclude it without TCC approval.
            let includes_self = crate::discovery::application_pids().contains(&pid);
            let self_windows = crate::accessibility::windows(pid)
                .map(|windows| windows.len())
                .map_err(|error| error.to_string());
            sender.send((includes_self, self_windows)).ok();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let (includes_self, self_windows) = loop {
            match receiver.try_recv() {
                Ok(result) => break result,
                Err(TryRecvError::Disconnected) => panic!("native worker exited without a result"),
                Err(TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "native self-window worker timed out"
                    );
                    pump_events(&application);
                }
            }
        };
        worker.join().expect("native worker panicked");
        eprintln!(
            "Native boundary results: discovery includes self = {includes_self}, AX self-window count = {self_windows:?}"
        );
        assert!(
            !includes_self && matches!(self_windows, Ok(0)),
            "production discovery and AX boundaries must both exclude the server's own window"
        );
        eprintln!(
            "Native self-window regression passed: visible fixture excluded from discovery and AX"
        );
    }
}
