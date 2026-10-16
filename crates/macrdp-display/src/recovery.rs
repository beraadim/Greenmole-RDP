//! Recovery discovery must not go through the server's possibly stuck display
//! worker. These queries never create displays, change modes, or alter layout.

use crate::{DisplayGeometry, WindowMoveReport};
use anyhow::{ensure, Result};
use std::time::{Duration, Instant};

/// Enumerate macrdp display identities. Callers supervising one server must
/// subtract their pre-launch baseline: another instance may own an existing ID.
pub fn list_owned_virtual_displays() -> Result<Vec<u32>> {
    #[cfg(target_os = "macos")]
    {
        use core_graphics::display::CGDisplay;
        Ok(crate::macos::online_display_ids()?
            .into_iter()
            .filter(|id| {
                let display = CGDisplay::new(*id);
                macrdp_identity(display.vendor_number(), display.model_number())
            })
            .collect())
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Display recovery requires macOS")
}

#[cfg(any(target_os = "macos", test))]
fn macrdp_identity(vendor: u32, product: u32) -> bool {
    // These values are the descriptor identity, not a generic virtual-display
    // heuristic. AirPlay, Sidecar and other virtual displays are left alone.
    vendor == 0x4D52 && product == 1
}

/// Wait only for IDs attributed to a supervised worker. This does not kill
/// processes or remove displays; the caller owns that independent lifecycle.
/// A zero timeout still performs the initial membership check.
pub fn wait_for_owned_virtual_display_removal(ids: &[u32], timeout: Duration) -> Result<()> {
    ensure!(ids.len() <= 64, "Too many display IDs in recovery request");
    ensure!(
        timeout <= Duration::from_secs(30),
        "Display recovery wait exceeds 30 seconds"
    );
    wait_for_removal(ids, timeout, list_owned_virtual_displays)
}

fn wait_for_removal(
    ids: &[u32],
    timeout: Duration,
    mut online: impl FnMut() -> Result<Vec<u32>>,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let deadline = Instant::now() + timeout;
    loop {
        let current = online()?;
        let remaining: Vec<_> = ids
            .iter()
            .filter(|id| current.contains(id))
            .copied()
            .collect();
        if remaining.is_empty() {
            return Ok(());
        }
        let remaining_time = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining_time.is_zero(),
            "Remote displays remain after worker shutdown: {remaining:?}"
        );
        std::thread::sleep(remaining_time.min(Duration::from_millis(50)));
    }
}

/// Select an available local display independently of any capture owner. The
/// current primary is preferred when it is not a macrdp remote display. Other
/// manufacturers' displays are usable local screens and are never reconfigured.
pub fn physical_display_geometry() -> Result<DisplayGeometry> {
    #[cfg(target_os = "macos")]
    {
        use anyhow::Context;
        use core_graphics::display::CGDisplay;
        let ids = crate::macos::online_display_ids()?;
        let primary = CGDisplay::main().id;
        let id = preferred_local_id(&ids, primary, |id| {
            let display = CGDisplay::new(id);
            macrdp_identity(display.vendor_number(), display.model_number())
        })
        .context("No local display is available for desktop recovery")?;
        let display = CGDisplay::new(id);
        let bounds = display.bounds();
        let mode = display
            .display_mode()
            .context("Local display has no current mode")?;
        let geometry = DisplayGeometry {
            display_id: id,
            origin_x: bounds.origin.x,
            origin_y: bounds.origin.y,
            logical_width: bounds.size.width,
            logical_height: bounds.size.height,
            pixel_width: u32::try_from(mode.pixel_width())
                .context("Local display width is too large")?,
            pixel_height: u32::try_from(mode.pixel_height())
                .context("Local display height is too large")?,
        };
        ensure!(
            geometry.origin_x.is_finite()
                && geometry.origin_y.is_finite()
                && geometry.logical_width.is_finite()
                && geometry.logical_width > 0.0
                && geometry.logical_height.is_finite()
                && geometry.logical_height > 0.0
                && geometry.pixel_width > 0
                && geometry.pixel_height > 0,
            "Local display reported invalid geometry"
        );
        Ok(geometry)
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Display recovery requires macOS")
}

#[cfg(any(target_os = "macos", test))]
fn preferred_local_id(
    ids: &[u32],
    primary: u32,
    mut owned: impl FnMut(u32) -> bool,
) -> Option<u32> {
    ids.iter()
        .copied()
        .filter(|id| *id != 0 && !owned(*id))
        .min_by_key(|id| (u8::from(*id != primary), *id))
}

/// Explicit recovery after the remote display has disappeared. Unlike moving
/// windows to the remote screen, this preserves accessible windows on every
/// surviving monitor and gathers only stranded ordinary windows. No permission
/// prompt is triggered; missing Accessibility access is returned to the caller.
/// Run on a blocking worker, never the UI thread.
pub fn recover_stranded_windows_on_physical_display() -> Result<WindowMoveReport> {
    #[cfg(target_os = "macos")]
    {
        ensure!(
            list_owned_virtual_displays()?.is_empty(),
            "Stop remote displays before gathering local windows"
        );
        let target = physical_display_geometry()?;
        let (request, _preparation) = crate::window_move::Request::new(|_| Ok(0));
        crate::manual_windows::recover_stranded_windows(target, request.budget)
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Display recovery requires macOS")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_requires_the_specific_macrdp_descriptor_identity() {
        assert!(macrdp_identity(0x4D52, 1));
        assert!(!macrdp_identity(0x4D52, 2));
        assert!(!macrdp_identity(0x610, 1));
        assert!(!macrdp_identity(0, 0));
    }

    #[test]
    fn recovery_prefers_surviving_primary_and_excludes_remote_even_when_primary() {
        assert_eq!(preferred_local_id(&[8, 2, 10], 8, |id| id == 10), Some(8));
        assert_eq!(preferred_local_id(&[8, 2, 10], 10, |id| id == 10), Some(2));
        assert_eq!(preferred_local_id(&[0, 10], 10, |id| id == 10), None);
    }

    #[test]
    fn removal_wait_ignores_other_instances_and_accepts_disappearance() {
        let mut calls = 0;
        wait_for_removal(&[12], Duration::from_millis(100), || {
            calls += 1;
            Ok(if calls == 1 { vec![12, 22] } else { vec![22] })
        })
        .unwrap();
        assert_eq!(calls, 2);
    }

    #[test]
    fn removal_wait_reports_stuck_display_and_propagates_discovery_failure() {
        assert!(wait_for_removal(&[12], Duration::ZERO, || Ok(vec![12])).is_err());
        assert!(wait_for_removal(&[12], Duration::ZERO, || anyhow::bail!(
            "WindowServer unavailable"
        ))
        .is_err());
        wait_for_removal(&[], Duration::ZERO, || panic!("no discovery necessary")).unwrap();
    }
}
