//! Bounded manual recovery of ordinary windows on an active remote display.
//! No snapshots, subscriptions, or disconnect restoration are installed.

#[cfg(target_os = "macos")]
mod accessibility;
#[cfg(target_os = "macos")]
mod discovery;
mod geometry;
mod process_filter;

#[cfg(target_os = "macos")]
pub(crate) fn move_windows(
    target: crate::DisplayGeometry,
    mut budget: crate::window_move::Budget,
    own_windows: Box<dyn FnOnce(crate::DisplayGeometry) -> anyhow::Result<usize> + Send>,
) -> anyhow::Result<crate::WindowMoveReport> {
    use anyhow::ensure;
    use std::time::{Duration, Instant};

    budget.check()?;
    ensure!(
        accessibility::trusted(),
        "Enable Accessibility access for macrdp in System Settings > Privacy & Security > Accessibility to move application windows"
    );
    let bounds = geometry::Frame::from(target);
    ensure!(
        target.display_id != 0 && bounds.valid(),
        "Invalid remote display geometry"
    );
    let screens = discovery::screens()?;
    let applications = discovery::application_pids()?;
    budget.check()?;
    // Own AppKit windows require the application's main thread; never query
    // them through AX from this worker (doing so previously crashed AppKit).
    let (moved, own_failed) = match own_windows(target) {
        Ok(moved) => (moved, 0),
        Err(error) => {
            tracing::warn!(%error, "Could not move macrdp's own windows; continuing with other applications");
            (0, 1)
        }
    };
    budget.deadline = budget.deadline.min(Instant::now() + Duration::from_secs(5));
    let mut report = crate::WindowMoveReport {
        target,
        moved,
        already_visible: 0,
        skipped: 0,
        failed: own_failed,
        app_failures: 0,
        timed_out: false,
    };
    'apps: for pid in applications {
        if budget.expired() {
            report.timed_out = true;
            break;
        }
        let windows = match accessibility::windows(pid, &budget) {
            Ok(windows) => windows,
            Err(error) => {
                report.app_failures += 1;
                tracing::debug!(pid, %error, "Could not inspect an application's windows");
                continue;
            }
        };
        for window in windows {
            if budget.expired() {
                report.timed_out = true;
                break 'apps;
            }
            let result = (|| -> Result<_, accessibility::AxError> {
                let Some(resizable) = window.movable()? else {
                    return Ok(None);
                };
                let frame = window.frame()?;
                let destination = frame.on_remote_display(bounds, &screens, resizable);
                if frame.close_to(destination) {
                    return Ok(Some(false));
                }
                window.set_frame(destination, resizable)?;
                Ok(Some(true))
            })();
            match result {
                Ok(Some(true)) => report.moved += 1,
                Ok(Some(false)) => report.already_visible += 1,
                Ok(None) => report.skipped += 1,
                Err(error) => {
                    report.failed += 1;
                    tracing::debug!(pid, %error, "Could not move an application window");
                }
            }
        }
    }
    report.timed_out |= budget.expired();
    tracing::info!(
        display_id = target.display_id,
        moved = report.moved,
        already_visible = report.already_visible,
        skipped = report.skipped,
        failed = report.failed,
        app_failures = report.app_failures,
        timed_out = report.timed_out,
        "Finished manual move of windows to the remote display"
    );
    Ok(report)
}
