//! Bounded, read-only CoreGraphics discovery for the AX worker.

use crate::geometry::Frame;
use crate::process_filter::is_external_application;
use anyhow::{ensure, Result};
use core_foundation::base::{CFType, TCFType};
use core_foundation::number::CFNumber;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex};
use core_foundation_sys::base::CFGetTypeID;
use core_foundation_sys::dictionary::{CFDictionaryGetTypeID, CFDictionaryGetValue};
use core_graphics::display::CGDisplay;
use core_graphics::window::{
    kCGWindowListExcludeDesktopElements, kCGWindowOwnerPID, CGWindowListCopyWindowInfo,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(crate) struct Screen {
    pub id: u32,
    pub bounds: Frame,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(capacity: u32, displays: *mut u32, count: *mut u32) -> i32;
}

pub(crate) fn physical_screens(virtual_id: u32) -> Result<Vec<Screen>> {
    let mut ids = [0; 64];
    let mut count = 0;
    ensure!(
        unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } == 0,
        "Could not query physical screens for window restoration"
    );
    ensure!(
        count < ids.len() as u32,
        "Physical display enumeration exceeded its limit"
    );
    let main_id = CGDisplay::main().id;
    let mut screens: Vec<_> = ids[..count as usize]
        .iter()
        .copied()
        .filter(|id| *id != virtual_id)
        .filter_map(|id| {
            let rect = CGDisplay::new(id).bounds();
            let bounds = Frame {
                x: rect.origin.x,
                y: rect.origin.y,
                width: rect.size.width,
                height: rect.size.height,
            };
            bounds.valid().then_some(Screen { id, bounds })
        })
        .collect();
    screens.sort_by_key(|screen| screen.id != main_id);
    ensure!(
        !screens.is_empty(),
        "No physical display is available for returning windows"
    );
    Ok(screens)
}

/// The CG list includes offscreen/minimized windows and needs no AppKit runloop.
/// Only external owner PIDs are returned; window titles and contents are never
/// inspected. Our own AppKit windows must not be accessed from this worker.
pub(crate) fn application_pids() -> BTreeSet<i32> {
    let raw = unsafe { CGWindowListCopyWindowInfo(kCGWindowListExcludeDesktopElements, 0) };
    if raw.is_null() {
        return BTreeSet::new();
    }
    let info = unsafe { CFType::wrap_under_create_rule(raw.cast()) };
    if info.type_of() != unsafe { CFArrayGetTypeID() } {
        return BTreeSet::new();
    }
    let count = unsafe { CFArrayGetCount(info.as_CFTypeRef().cast()) }.min(16384);
    let mut pids = BTreeSet::new();
    for index in 0..count {
        let dictionary = unsafe { CFArrayGetValueAtIndex(info.as_CFTypeRef().cast(), index) };
        if dictionary.is_null()
            || unsafe { CFGetTypeID(dictionary) } != unsafe { CFDictionaryGetTypeID() }
        {
            continue;
        }
        let raw = unsafe { CFDictionaryGetValue(dictionary.cast(), kCGWindowOwnerPID.cast()) };
        if raw.is_null() || unsafe { CFGetTypeID(raw) } != CFNumber::type_id() {
            continue;
        }
        let value = unsafe { CFNumber::wrap_under_get_rule(raw.cast()) };
        if let Some(pid) = value.to_i32().filter(|pid| is_external_application(*pid)) {
            pids.insert(pid);
        }
    }
    pids
}
