//! Bound the lifetime of Command flags to a single translated keyboard event.

use anyhow::Result;
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType};
use core_graphics::event_source::CGEventSourceStateID;

use super::{CGEventSourceFlagsState, KeyboardInjector};

const LEFT_COMMAND: u16 = 0x37;
const RIGHT_COMMAND: u16 = 0x36;
// Device-dependent flags from IOKit/hidsystem/IOLLEvent.h. Clear these along
// with the aggregate Command flag so applications cannot infer a held side.
const LEFT_COMMAND_MASK: u64 = 0x0000_0008;
const RIGHT_COMMAND_MASK: u64 = 0x0000_0010;

unsafe extern "C" {
    fn CGEventSourceKeyState(state_id: CGEventSourceStateID, key: u16) -> bool;
}

#[derive(Clone, Copy)]
struct ModifierSnapshot {
    flags: CGEventFlags,
    left_command: bool,
    right_command: bool,
}

impl ModifierSnapshot {
    fn current() -> Self {
        // Shortcut events use a private source. Query the HID key table to
        // distinguish a real Command key (including remote Alt) from Command
        // flags left by an earlier shortcut; never clear a genuinely held key.
        unsafe {
            Self {
                flags: CGEventSourceFlagsState(CGEventSourceStateID::HIDSystemState),
                left_command: CGEventSourceKeyState(
                    CGEventSourceStateID::HIDSystemState,
                    LEFT_COMMAND,
                ),
                right_command: CGEventSourceKeyState(
                    CGEventSourceStateID::HIDSystemState,
                    RIGHT_COMMAND,
                ),
            }
        }
    }

    fn restored_flags(self) -> CGEventFlags {
        let mut bits = self.flags.bits()
            & !(CGEventFlags::CGEventFlagCommand.bits() | LEFT_COMMAND_MASK | RIGHT_COMMAND_MASK);
        if self.left_command || self.right_command {
            bits |= CGEventFlags::CGEventFlagCommand.bits();
        }
        if self.left_command {
            bits |= LEFT_COMMAND_MASK;
        }
        if self.right_command {
            bits |= RIGHT_COMMAND_MASK;
        }
        CGEventFlags::from_bits_retain(bits)
    }
}

pub(super) fn post(keycode: u16, pressed: bool, shifted: bool) -> Result<()> {
    // Allocate both events before posting either: a failed allocation must not
    // leave Command active with no prepared release. Post is infallible and
    // there is no await/cancellation point between the two ordered events.
    let events = events(keycode, pressed, shifted, ModifierSnapshot::current())?;
    for event in events {
        event.post(CGEventTapLocation::HID);
    }
    tracing::trace!("Restored native modifiers after translated Command event");
    Ok(())
}

fn events(
    keycode: u16,
    pressed: bool,
    shifted: bool,
    native: ModifierSnapshot,
) -> Result<[CGEvent; 2]> {
    let source = KeyboardInjector::private_source()?;
    let shortcut = CGEvent::new_keyboard_event(source.clone(), keycode, pressed)
        .map_err(|_| anyhow::anyhow!("Failed to create Command shortcut event"))?;
    let mut flags = CGEventFlags::CGEventFlagCommand;
    flags.set(CGEventFlags::CGEventFlagShift, shifted);
    flags.set(CGEventFlags::CGEventFlagNumericPad, keycode == 0x4C);
    shortcut.set_flags(flags);

    // Even a key-up carrying Command can latch that modifier in macOS. End
    // every press, repeat AND release with a modifier event, so lost client
    // key-ups and cleanup cannot leave the temporary flag behind either.
    let command_key = if native.right_command && !native.left_command {
        RIGHT_COMMAND
    } else {
        LEFT_COMMAND
    };
    let restore = CGEvent::new_keyboard_event(
        source,
        command_key,
        native.left_command || native.right_command,
    )
    .map_err(|_| anyhow::anyhow!("Failed to create Command modifier restoration event"))?;
    restore.set_type(CGEventType::FlagsChanged);
    restore.set_flags(native.restored_flags());
    Ok([shortcut, restore])
}

#[cfg(test)]
mod tests;
