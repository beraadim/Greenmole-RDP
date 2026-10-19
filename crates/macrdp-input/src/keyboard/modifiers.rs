//! Complete modifier flags derived from remote-owned keys, never hardware.

use core_graphics::event::CGEventFlags;

#[derive(Clone, Copy)]
pub(super) struct RemoteModifiers {
    pub flags: CGEventFlags,
    pub left_command: bool,
    pub right_command: bool,
}

impl RemoteModifiers {
    pub(super) fn from_keys(held: impl Fn(u16) -> bool, caps_lock: bool) -> Self {
        let mut flags = if caps_lock {
            CGEventFlags::CGEventFlagAlphaShift
        } else {
            CGEventFlags::empty()
        };
        // Device-dependent flags from IOKit/hidsystem/IOLLEvent.h. Applications
        // may inspect the individual sides as well as the aggregate flag.
        for (code, aggregate, device) in [
            (0x38, CGEventFlags::CGEventFlagShift, 0x0000_0002),
            (0x3C, CGEventFlags::CGEventFlagShift, 0x0000_0004),
            (0x3B, CGEventFlags::CGEventFlagControl, 0x0000_0001),
            (0x3E, CGEventFlags::CGEventFlagControl, 0x0000_2000),
            (0x3A, CGEventFlags::CGEventFlagAlternate, 0x0000_0020),
            (0x3D, CGEventFlags::CGEventFlagAlternate, 0x0000_0040),
            (0x37, CGEventFlags::CGEventFlagCommand, 0x0000_0008),
            (0x36, CGEventFlags::CGEventFlagCommand, 0x0000_0010),
            (0x3F, CGEventFlags::CGEventFlagSecondaryFn, 0),
        ] {
            if held(code) {
                flags |= aggregate | CGEventFlags::from_bits_retain(device);
            }
        }
        Self {
            flags,
            left_command: held(0x37),
            right_command: held(0x36),
        }
    }
}
