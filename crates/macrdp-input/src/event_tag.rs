//! Identify injected input to the private-session input filter.
//!
//! The tag is a routing marker, not authentication against other processes
//! already trusted to inject input. The guard also checks the worker PID.

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, EventField};
use core_graphics::event_source::CGEventSource;
use foreign_types::ForeignType;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

static REMOTE_EVENT_TAG: AtomicI64 = AtomicI64::new(0);
static REMOTE_MODIFIER_FLAGS: AtomicU64 = AtomicU64::new(0);

extern "C" {
    fn CGEventCreateSourceFromEvent(
        event: *mut core_graphics::sys::CGEvent,
    ) -> *mut core_graphics::sys::CGEventSource;
    fn CGEventSourceSetUserData(source: *mut core_graphics::sys::CGEventSource, data: i64);
    fn CGEventSetSource(
        event: *mut core_graphics::sys::CGEvent,
        source: *mut core_graphics::sys::CGEventSource,
    );
}

/// Select the ephemeral marker shared with the active private-session guard.
///
/// Set this before enabling remote input, and keep it set until all held keys
/// and buttons have been released. Zero restores ordinary, unguarded input.
/// Session transitions must happen while the caller has quiesced input.
pub fn set_remote_event_tag(tag: i64) {
    REMOTE_MODIFIER_FLAGS.store(0, Ordering::Release);
    REMOTE_EVENT_TAG.store(tag, Ordering::Release);
}

/// Stop marking input for a private session, after its input has been released.
pub fn clear_remote_event_tag() {
    set_remote_event_tag(0);
}

pub(crate) fn remote_event_tag() -> i64 {
    REMOTE_EVENT_TAG.load(Ordering::Acquire)
}

pub(crate) fn private_modifier_flags() -> Option<CGEventFlags> {
    (remote_event_tag() != 0)
        .then(|| CGEventFlags::from_bits_retain(REMOTE_MODIFIER_FLAGS.load(Ordering::Acquire)))
}

pub(crate) fn set_private_modifier_flags(flags: CGEventFlags) {
    REMOTE_MODIFIER_FLAGS.store(flags.bits(), Ordering::Release);
}

/// Stamp at the posting boundary so events prepared before a session transition
/// cannot retain the previous session's marker. Never change event flags here.
pub(crate) fn stamp_remote_event(event: &CGEvent) -> anyhow::Result<()> {
    stamp(event, remote_event_tag())
}

fn stamp(event: &CGEvent, tag: i64) -> anyhow::Result<()> {
    let worker_pid = i64::from(std::process::id());
    if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == tag
        && event.get_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID) == worker_pid
    {
        // Ordinary unguarded events already have the correct zero marker and
        // worker identity; avoid allocating another source on that hot path.
        return Ok(());
    }
    // Reattach the source after updating its user data. On macOS, changing
    // only the per-event integer field can retain cached source user data,
    // especially when clearing/replacing a marker on an existing event.
    let source_ref = unsafe { CGEventCreateSourceFromEvent(event.as_ptr()) };
    anyhow::ensure!(
        !source_ref.is_null(),
        "Failed to create remote input event source"
    );
    let source = unsafe { CGEventSource::from_ptr(source_ref) };
    unsafe {
        CGEventSourceSetUserData(source.as_ptr(), tag);
        CGEventSetSource(event.as_ptr(), source.as_ptr());
    }
    event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, tag);
    event.set_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID, worker_pid);
    Ok(())
}

/// All remote input uses this posting boundary, including cleanup releases and
/// modifier restoration. Keep raw posting out of individual input paths.
pub(crate) fn post_remote_event(event: &CGEvent) -> anyhow::Result<()> {
    post_remote_events(std::slice::from_ref(event))
}

/// Prepare an ordered group completely before posting any member. Shortcut
/// modifier restoration must remain guaranteed even if source allocation fails.
pub(crate) fn post_remote_events(events: &[CGEvent]) -> anyhow::Result<()> {
    for event in events {
        stamp_remote_event(event)?;
    }
    for event in events {
        event.post(CGEventTapLocation::HID);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
