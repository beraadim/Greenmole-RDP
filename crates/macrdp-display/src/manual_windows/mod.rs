//! Bounded relocation of ordinary windows for explicit remote and local actions.
//! No snapshots or per-window subscriptions are installed.

#[cfg(target_os = "macos")]
pub(crate) mod accessibility;
#[cfg(target_os = "macos")]
pub(crate) mod discovery;
mod frame_setter;
pub(crate) mod geometry;
pub(crate) mod process_filter;

#[cfg(target_os = "macos")]
pub(crate) fn move_windows(
    target: crate::DisplayGeometry,
    budget: crate::window_move::Budget,
    own_windows: Box<dyn FnOnce(crate::DisplayGeometry) -> anyhow::Result<usize> + Send>,
) -> anyhow::Result<crate::WindowMoveReport> {
    move_windows_with_policy(target, budget, own_windows, false)
}

#[cfg(target_os = "macos")]
pub(crate) fn recover_stranded_windows(
    target: crate::DisplayGeometry,
    budget: crate::window_move::Budget,
) -> anyhow::Result<crate::WindowMoveReport> {
    move_windows_with_policy(target, budget, Box::new(|_| Ok(0)), true)
}

#[cfg(target_os = "macos")]
fn move_windows_with_policy(
    target: crate::DisplayGeometry,
    mut budget: crate::window_move::Budget,
    own_windows: Box<dyn FnOnce(crate::DisplayGeometry) -> anyhow::Result<usize> + Send>,
    stranded_only: bool,
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
        "Invalid window relocation display geometry"
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
                if stranded_only && frame.has_accessible_titlebar(&screens) {
                    return Ok(Some(false));
                }
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
        stranded_only,
        "Finished bounded window relocation"
    );
    Ok(report)
}
