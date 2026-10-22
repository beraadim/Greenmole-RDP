//! Read-only candidate discovery for parking a retained remote display.
//!
//! Limit Accessibility work to processes with ordinary windows intersecting the
//! departing remote screen. Window names and contents are never requested.

use crate::manual_windows::geometry::Frame;
use crate::manual_windows::process_filter::is_external_application;
use anyhow::{ensure, Context, Result};
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex};
use core_foundation_sys::base::{CFGetTypeID, CFTypeRef};
use core_foundation_sys::dictionary::{
    CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef,
};
use core_foundation_sys::string::CFStringRef;
use core_graphics::geometry::CGRect;
use core_graphics::window::{
    kCGWindowBounds, kCGWindowLayer, kCGWindowListExcludeDesktopElements, kCGWindowOwnerPID,
    CGWindowListCopyWindowInfo,
};
use std::collections::BTreeSet;

const MAX_DISCOVERED_WINDOWS: isize = 16384;

pub(super) fn candidate_pids(remote: Frame) -> Result<BTreeSet<i32>> {
    ensure!(remote.valid(), "Invalid retained display candidate bounds");
    let raw = unsafe { CGWindowListCopyWindowInfo(kCGWindowListExcludeDesktopElements, 0) };
    ensure!(
        !raw.is_null(),
        "Could not discover parking window candidates"
    );
    let info = unsafe { CFType::wrap_under_create_rule(raw.cast()) };
    ensure!(
        info.type_of() == unsafe { CFArrayGetTypeID() },
        "Parking window list has an invalid native type"
    );
    let count = unsafe { CFArrayGetCount(info.as_CFTypeRef().cast()) };
    ensure!(
        count <= MAX_DISCOVERED_WINDOWS,
        "Parking window discovery exceeded its limit"
    );
    let mut pids = BTreeSet::new();
    for index in 0..count {
        let dictionary = unsafe { CFArrayGetValueAtIndex(info.as_CFTypeRef().cast(), index) };
        ensure!(
            !dictionary.is_null()
                && unsafe { CFGetTypeID(dictionary) } == unsafe { CFDictionaryGetTypeID() },
            "Parking window record has an invalid native type"
        );
        let dictionary = dictionary.cast();
        let layer = number(dictionary, unsafe { kCGWindowLayer })?
            .context("Parking window layer is unavailable")?;
        if layer != 0 {
            continue;
        }
        let pid = number(dictionary, unsafe { kCGWindowOwnerPID })?
            .context("Parking window owner is unavailable")?;
        if !is_external_application(pid) {
            continue;
        }
        let bounds = unsafe { CFDictionaryGetValue(dictionary, kCGWindowBounds.cast()) };
        ensure!(
            !bounds.is_null()
                && unsafe { CFGetTypeID(bounds) } == unsafe { CFDictionaryGetTypeID() },
            "Parking window bounds are unavailable"
        );
        let bounds = unsafe { CFDictionary::wrap_under_get_rule(bounds.cast()) };
        let bounds = CGRect::from_dict_representation(&bounds)
            .context("Parking window bounds are invalid")?;
        let frame = Frame {
            x: bounds.origin.x,
            y: bounds.origin.y,
            width: bounds.size.width,
            height: bounds.size.height,
        };
        // A broad intersection filter avoids assuming AX and CG decorations
        // have identical bounds. The AX engine makes the stricter ownership
        // and titlebar-accessibility decision before any move.
        if candidate(layer, pid, frame, remote) {
            pids.insert(pid);
        }
    }
    Ok(pids)
}

fn number(dictionary: CFDictionaryRef, key: CFStringRef) -> Result<Option<i32>> {
    let raw: CFTypeRef = unsafe { CFDictionaryGetValue(dictionary, key.cast()) };
    if raw.is_null() {
        return Ok(None);
    }
    ensure!(
        unsafe { CFGetTypeID(raw) } == CFNumber::type_id(),
        "Parking window metadata has an invalid numeric type"
    );
    Ok(unsafe { CFNumber::wrap_under_get_rule(raw.cast()) }.to_i32())
}

fn candidate(layer: i32, pid: i32, bounds: Frame, remote: Frame) -> bool {
    layer == 0
        && is_external_application(pid)
        && bounds.valid()
        && bounds.intersection(remote) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_scan_excludes_local_windows_overlays_and_our_own_process() {
        let remote = Frame {
            x: -1280.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
        };
        let remote_window = Frame {
            x: -1200.0,
            y: 40.0,
            width: 800.0,
            height: 600.0,
        };
        let local_window = Frame {
            x: 50.0,
            ..remote_window
        };
        let external_pid = if std::process::id() == 1 { 2 } else { 1 };
        assert!(candidate(0, external_pid, remote_window, remote));
        assert!(!candidate(0, external_pid, local_window, remote));
        assert!(!candidate(25, external_pid, remote_window, remote));
        assert!(!candidate(
            0,
            std::process::id() as i32,
            remote_window,
            remote
        ));
        assert!(!candidate(0, -1, remote_window, remote));
        assert!(!candidate(
            0,
            external_pid,
            Frame {
                width: 0.0,
                ..remote_window
            },
            remote
        ));
    }
}
