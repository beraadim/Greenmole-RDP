//! AppKit objects stay on the helper's main thread. Panels do not become key or
//! activate an application, and the macrdp virtual display is never covered.

use anyhow::{ensure, Context, Result};
use core_graphics::display::CGDisplay;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSRect, NSString};
use std::collections::BTreeMap;
use std::ffi::CStr;

#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGShieldingWindowLevel() -> i32;
    fn CGGetOnlineDisplayList(maximum: u32, displays: *mut u32, count: *mut u32) -> i32;
}

fn class(name: &'static [u8]) -> Result<&'static AnyClass> {
    AnyClass::get(CStr::from_bytes_with_nul(name).unwrap())
        .context("Required AppKit private-session class is unavailable")
}

fn physical_ids() -> Result<Vec<u32>> {
    let mut all = [0u32; 64];
    let mut count = 0;
    ensure!(
        unsafe { CGGetOnlineDisplayList(all.len() as u32, all.as_mut_ptr(), &mut count) } == 0
            && count <= all.len() as u32,
        "Could not enumerate displays for private-session protection"
    );
    let mut ids: Vec<_> = all[..count as usize]
        .iter()
        .copied()
        .filter(|&id| {
            let display = CGDisplay::new(id);
            !(display.vendor_number() == 0x4D52 && display.model_number() == 1)
        })
        .collect();
    ids.sort_unstable();
    ensure!(
        ids.len() <= 32,
        "Too many displays for private-session protection"
    );
    Ok(ids)
}

fn screen_frames() -> Result<BTreeMap<u32, NSRect>> {
    let screen = class(b"NSScreen\0")?;
    let key = NSString::from_str("NSScreenNumber");
    let screens: *mut AnyObject = unsafe { msg_send![screen, screens] };
    ensure!(
        !screens.is_null(),
        "AppKit cannot enumerate physical screens"
    );
    let count: usize = unsafe { msg_send![screens, count] };
    ensure!(count <= 64, "Unexpected AppKit screen count");
    let mut frames = BTreeMap::new();
    for index in 0..count {
        let item: *mut AnyObject = unsafe { msg_send![screens, objectAtIndex: index] };
        let description: *mut AnyObject = unsafe { msg_send![item, deviceDescription] };
        let number: *mut AnyObject = unsafe { msg_send![description, objectForKey: &*key] };
        ensure!(
            !number.is_null(),
            "A physical screen has no stable display identifier"
        );
        let id: u32 = unsafe { msg_send![number, unsignedIntValue] };
        let frame: NSRect = unsafe { msg_send![item, frame] };
        ensure!(
            frame.origin.x.is_finite()
                && frame.origin.y.is_finite()
                && frame.size.width.is_finite()
                && frame.size.height.is_finite()
                && frame.size.width > 0.0
                && frame.size.height > 0.0,
            "A physical screen has invalid bounds"
        );
        frames.insert(id, frame);
    }
    Ok(frames)
}

pub(super) struct Covers {
    ids: Vec<u32>,
    panels: BTreeMap<u32, Retained<AnyObject>>,
    frames: BTreeMap<u32, NSRect>,
}

impl Covers {
    pub(super) fn create() -> Result<Self> {
        let thread = class(b"NSThread\0")?;
        let main: Bool = unsafe { msg_send![thread, isMainThread] };
        ensure!(
            main.as_bool(),
            "Private-session panels must be created on the helper main thread"
        );
        let app = class(b"NSApplication\0")?;
        let app: *mut AnyObject = unsafe { msg_send![app, sharedApplication] };
        let accepted: Bool = unsafe { msg_send![app, setActivationPolicy: 2isize] };
        ensure!(
            accepted.as_bool(),
            "AppKit could not apply a non-activating protection policy"
        );
        let ids = physical_ids()?;
        ensure!(
            !ids.is_empty(),
            "Private-session protection requires an identifiable physical display"
        );
        let frames = screen_frames()?;
        let panel_class = class(b"NSPanel\0")?;
        let color_class = class(b"NSColor\0")?;
        let black: *mut AnyObject = unsafe { msg_send![color_class, blackColor] };
        let mut covers = Self {
            ids,
            panels: BTreeMap::new(),
            frames: BTreeMap::new(),
        };
        for &id in &covers.ids {
            let frame = *frames
                .get(&id)
                .context("A connected screen is not available to AppKit; cannot protect it")?;
            let panel: Option<Retained<AnyObject>> = unsafe {
                let allocated: Allocated<AnyObject> = msg_send![panel_class, alloc];
                msg_send![allocated, initWithContentRect: frame, styleMask: 128usize, backing: 2usize, defer: Bool::NO]
            };
            let panel = panel.context("Could not allocate a private-session protection panel")?;
            unsafe {
                let _: () = msg_send![&*panel, setReleasedWhenClosed: Bool::NO];
                let _: () = msg_send![&*panel, setBackgroundColor: black];
                let _: () = msg_send![&*panel, setOpaque: Bool::YES];
                let _: () = msg_send![&*panel, setHasShadow: Bool::NO];
                let _: () = msg_send![&*panel, setIgnoresMouseEvents: Bool::YES];
                let _: () = msg_send![&*panel, setHidesOnDeactivate: Bool::NO];
                let _: () = msg_send![&*panel, setMovable: Bool::NO];
                let _: () = msg_send![&*panel, setMovableByWindowBackground: Bool::NO];
                let _: () = msg_send![&*panel, setAccessibilityElement: Bool::NO];
                let _: () = msg_send![&*panel, setAccessibilityHidden: Bool::YES];
                let _: () = msg_send![&*panel, setCollectionBehavior: (1usize | 16 | 64 | 256)];
                let _: () = msg_send![&*panel, setLevel: i64::from(CGShieldingWindowLevel()) + 1];
                // Do not permit screen/window capture of the protection panel.
                let _: () = msg_send![&*panel, setSharingType: 0usize];
                let _: () = msg_send![&*panel, orderFrontRegardless];
                let _: () = msg_send![&*panel, displayIfNeeded];
                let visible: Bool = msg_send![&*panel, isVisible];
                ensure!(
                    visible.as_bool(),
                    "macOS did not display a private-session protection panel"
                );
            }
            covers.frames.insert(id, frame);
            covers.panels.insert(id, panel);
        }
        // Reject hotplug racing initial setup rather than admitting a client
        // with an uncovered monitor.
        ensure!(
            physical_ids()? == covers.ids,
            "Physical displays changed during private-session admission"
        );
        Ok(covers)
    }

    /// Geometry may change when the virtual display becomes primary. Cover the
    /// same monitors at their new AppKit coordinates; a topology change ends the
    /// session instead of silently expanding the security surface.
    pub(super) fn refresh(&mut self) -> Result<()> {
        ensure!(
            physical_ids()? == self.ids,
            "Physical display topology changed during private session"
        );
        let frames = screen_frames()?;
        for (&id, panel) in &self.panels {
            let frame = *frames
                .get(&id)
                .context("A protected screen disappeared from AppKit")?;
            let actual: NSRect = unsafe { msg_send![&**panel, frame] };
            if self.frames.get(&id) != Some(&frame) || actual != frame {
                unsafe {
                    let _: () = msg_send![&**panel, setFrame: frame, display: Bool::YES];
                    let _: () = msg_send![&**panel, orderFrontRegardless];
                }
                self.frames.insert(id, frame);
            }
            let actual: NSRect = unsafe { msg_send![&**panel, frame] };
            ensure!(
                actual == frame,
                "A private-session panel could not be kept on its physical display"
            );
            let visible: Bool = unsafe { msg_send![&**panel, isVisible] };
            ensure!(
                visible.as_bool(),
                "A private-session protection panel is no longer visible"
            );
        }
        Ok(())
    }

    pub(super) fn close(&mut self) {
        for panel in self.panels.values() {
            unsafe {
                let _: () = msg_send![&**panel, orderOut: std::ptr::null_mut::<AnyObject>()];
            }
        }
        self.panels.clear();
    }
}

impl Drop for Covers {
    fn drop(&mut self) {
        self.close();
    }
}
