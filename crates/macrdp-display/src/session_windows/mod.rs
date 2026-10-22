//! Connection-owned window snapshots. Native AX references stay in the display
//! helper; no window titles, contents, or process identifiers are persisted.

mod engine;
pub(crate) use engine::PhaseReport;
#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
mod parking;
#[cfg(target_os = "macos")]
mod parking_discovery;
#[cfg(target_os = "macos")]
pub(crate) use native::SessionWindows;

#[cfg(test)]
mod tests;
