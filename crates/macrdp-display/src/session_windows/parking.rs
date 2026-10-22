//! Bounded evacuation before a remote display is retained in an inactive state.
//! Failure leaves display removal as the caller's normal window-recovery path.

use super::engine::{evacuate_window, PhaseReport};
use super::native::{finish_phase, physical_screens, PHASE_BUDGET};
use crate::manual_windows::{accessibility, geometry::Frame};
use crate::window_move::Budget;
use anyhow::{ensure, Result};
use core_graphics::display::CGDisplay;
use std::time::Instant;

/// Called after physical origins and saved frames are restored, while the
/// remote display is still active. Refuse parking if a bounded scan cannot
/// safely evacuate remaining ordinary windows; removal remains the fallback.
pub(super) fn evacuate_for_parking(virtual_id: u32) -> Result<PhaseReport> {
    let started = Instant::now();
    let budget = Budget::for_duration(PHASE_BUDGET);
    let mut report = PhaseReport::default();
    let result = (|| -> Result<()> {
        let active = CGDisplay::active_displays()
            .map_err(|error| anyhow::anyhow!("Could not refresh parking displays: {error}"))?;
        let primary = CGDisplay::main().id;
        ensure!(
            active.contains(&virtual_id) && active.contains(&primary) && primary != virtual_id,
            "Restore the physical primary before parking the remote display"
        );
        ensure!(
            active
                .iter()
                .filter(|id| {
                    let display = CGDisplay::new(**id);
                    display.vendor_number() == crate::virtual_display::VENDOR_ID
                        && display.model_number() == crate::virtual_display::PRODUCT_ID
                })
                .count()
                == 1,
            "Other remote displays prevent unambiguous window evacuation"
        );
        let display = CGDisplay::new(virtual_id);
        ensure!(
            display.vendor_number() == crate::virtual_display::VENDOR_ID
                && display.model_number() == crate::virtual_display::PRODUCT_ID,
            "Window evacuation target is not a macrdp virtual display"
        );
        ensure!(
            !display.is_in_mirror_set(),
            "Cannot park a mirrored remote display"
        );
        let bounds = display.bounds();
        let remote = Frame {
            x: bounds.origin.x,
            y: bounds.origin.y,
            width: bounds.size.width,
            height: bounds.size.height,
        };
        ensure!(
            remote.valid(),
            "Remote display has invalid parking geometry"
        );
        let local = physical_screens(virtual_id, primary)?;
        ensure!(
            local.iter().any(|screen| screen.primary),
            "Physical primary identity is unavailable for parking"
        );
        let candidates = super::parking_discovery::candidate_pids(remote)?;
        if !candidates.is_empty() {
            ensure!(
                accessibility::trusted(),
                "Accessibility access is unavailable for retained-display window evacuation"
            );
        }
        for pid in candidates {
            if budget.expired() {
                report.timed_out = true;
                break;
            }
            let windows = match accessibility::windows(pid, &budget) {
                Ok(windows) => windows,
                Err(_) => {
                    report.app_failures += 1;
                    continue;
                }
            };
            // The shared AX wrapper caps each application at 256 windows.
            // An exact-cap reply may be truncated, so do not claim a full
            // evacuation: retirement preserves the normal OS fallback.
            if windows.len() >= 256 {
                report.limit_reached = true;
                break;
            }
            for mut window in windows {
                evacuate_window(&mut window, remote, &local, &budget, &mut report);
                if report.timed_out || report.limit_reached {
                    break;
                }
            }
            if report.timed_out || report.limit_reached {
                break;
            }
        }
        report.timed_out |= budget.expired();
        ensure!(
            !report.timed_out && !report.limit_reached && report.failed == 0 && report.app_failures == 0,
            "Cannot verify retained-display window evacuation: failed={} app_failures={} timed_out={} limit_reached={}",
            report.failed, report.app_failures, report.timed_out, report.limit_reached
        );
        // A hotplug during AX work invalidates the destination assumptions;
        // the caller must remove the display rather than retain stale state.
        let current_local = physical_screens(virtual_id, CGDisplay::main().id)?;
        ensure!(
            current_local.len() == local.len()
                && local
                    .iter()
                    .all(|screen| current_local.iter().any(|current| {
                        current.key == screen.key
                            && current.primary == screen.primary
                            && current.frame.close_to(screen.frame)
                    })),
            "Physical display topology changed during window evacuation"
        );
        Ok(())
    })();
    finish_phase("evacuate_before_parking", &mut report, started);
    result?;
    Ok(report)
}
