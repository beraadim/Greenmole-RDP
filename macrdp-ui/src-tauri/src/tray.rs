use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize};

use macrdp_core::Metrics;

/// The native menu remains usable when the webview or server is unresponsive.
/// Left click opens the lightweight popover; right click exposes recovery.
pub fn setup_tray(handle: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let tray = handle
        .tray_by_id("main-tray")
        .ok_or("tray icon 'main-tray' not found")?;
    tray.set_tooltip(Some("macrdp"))?;
    let recover = MenuItem::with_id(
        handle,
        "recover-local-desktop",
        "Recover local desktop and stop server",
        true,
        None::<&str>,
    )?;
    let show = MenuItem::with_id(handle, "show-main", "Open macrdp", true, None::<&str>)?;
    let quit = MenuItem::with_id(
        handle,
        "quit-macrdp",
        "Quit macrdp and stop server",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(handle, &[&show, &recover, &quit])?;
    tray.set_menu(Some(menu))?;
    tray.set_show_menu_on_left_click(false)?;
    tray.on_menu_event(|app, event| {
        let app = app.clone();
        match event.id().as_ref() {
            "recover-local-desktop" => {
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = crate::recovery::recover_local_desktop(&app).await {
                        tracing::error!(%error, "Local desktop recovery from tray failed");
                    }
                });
            }
            "show-main" => {
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = show_main_window(&app).await {
                        tracing::warn!(%error, "Could not open macrdp controls");
                    }
                });
            }
            "quit-macrdp" => {
                if let Err(error) = crate::app_lifecycle::request_quit(&app) {
                    tracing::error!(%error, "Could not request shutdown from tray");
                }
            }
            _ => {}
        }
    });
    let app = handle.clone();
    tray.on_tray_icon_event(move |_tray, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            position,
            ..
        } = event
        {
            toggle_popover(&app, position);
        }
    });
    Ok(())
}

/// Reopen on the screen the user is operating, whether physical or remote.
/// The server and its virtual display remain running.
pub async fn show_main_window(handle: &AppHandle) -> Result<(), String> {
    let app = handle.clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        crate::own_windows::show_main_window(&app)?;
        #[cfg(not(target_os = "macos"))]
        reveal_main_window(&app)?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|error| format!("Window reveal task failed: {error}"))?
    .map_err(|error| format!("{error:#}"))
}

/// Queries run off the UI thread and bypass every server-owned mutex. AppKit
/// handles the app's own window on its main thread, without Accessibility calls.
pub async fn show_main_on_physical_display(handle: &AppHandle) -> Result<(), String> {
    let app = handle.clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        crate::own_windows::show_main_window_on_display(
            &app,
            macrdp_core::physical_display_geometry(),
        )?;
        #[cfg(not(target_os = "macos"))]
        reveal_main_window(&app)?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|error| format!("Local window recovery task failed: {error}"))?
    .map_err(|error| format!("{error:#}"))
}

#[cfg(not(target_os = "macos"))]
fn reveal_main_window(app: &AppHandle) -> anyhow::Result<()> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow::anyhow!("The macrdp main window is unavailable"))?;
    let results = [window.unminimize(), window.show(), window.set_focus()];
    for result in results {
        result?;
    }
    Ok(())
}

fn toggle_popover(handle: &AppHandle, click: PhysicalPosition<f64>) {
    let Some(window) = handle.get_webview_window("popover") else {
        return;
    };
    // The remote screen can be primary. Use the display where the user actually
    // clicked, with its real global origin, rather than the primary display.
    let monitor = handle.monitor_from_point(click.x, click.y).ok().flatten();
    let on_clicked_display = monitor.as_ref().is_some_and(|monitor| {
        window
            .current_monitor()
            .ok()
            .flatten()
            .is_some_and(|current| {
                current.position() == monitor.position() && current.size() == monitor.size()
            })
    });
    if window.is_visible().unwrap_or(false) && on_clicked_display {
        let _ = window.hide();
        return;
    }
    if let Some(monitor) = monitor {
        let scale = monitor.scale_factor();
        let size = window.outer_size().unwrap_or(PhysicalSize::new(320, 400));
        let old_scale = window.scale_factor().unwrap_or(scale);
        let desired_size = (
            size.width as f64 / old_scale * scale,
            size.height as f64 / old_scale * scale,
        );
        let origin = monitor.position();
        let extent = monitor.size();
        if let Some((x, y)) = popover_position(
            (origin.x as f64, origin.y as f64),
            (extent.width as f64, extent.height as f64),
            desired_size,
            (click.x, click.y),
            scale,
        ) {
            let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
        }
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// Keep the popover below the clicked menu bar on negative-origin and Retina
/// displays. Geometry is consistently in physical pixels; no primary-origin
/// assumption or mixed logical/physical coordinate arithmetic is needed.
fn popover_position(
    origin: (f64, f64),
    extent: (f64, f64),
    window: (f64, f64),
    click: (f64, f64),
    scale: f64,
) -> Option<(f64, f64)> {
    let values = [
        origin.0, origin.1, extent.0, extent.1, window.0, window.1, click.0, click.1, scale,
    ];
    if !values.into_iter().all(f64::is_finite)
        || extent.0 <= 0.0
        || extent.1 <= 0.0
        || window.0 <= 0.0
        || window.1 <= 0.0
        || scale <= 0.0
    {
        return None;
    }
    let right = origin.0 + (extent.0 - window.0).max(0.0);
    let bottom = origin.1 + (extent.1 - window.1).max(0.0);
    let x = (click.0 - window.0).clamp(origin.0, right);
    let y = (origin.1 + 28.0 * scale).clamp(origin.1, bottom);
    Some((x, y))
}

#[cfg(test)]
mod tests {
    use super::popover_position;

    #[test]
    fn popover_follows_clicked_secondary_display_with_negative_origin() {
        assert_eq!(
            popover_position(
                (-1920.0, -200.0),
                (1920.0, 1080.0),
                (320.0, 400.0),
                (-100.0, -190.0),
                1.0
            ),
            Some((-420.0, -172.0))
        );
    }

    #[test]
    fn popover_scales_menu_bar_and_clamps_to_clicked_monitor() {
        assert_eq!(
            popover_position(
                (0.0, 0.0),
                (3840.0, 2160.0),
                (640.0, 800.0),
                (3800.0, 10.0),
                2.0
            ),
            Some((3160.0, 56.0))
        );
        assert_eq!(
            popover_position(
                (100.0, 200.0),
                (200.0, 150.0),
                (320.0, 400.0),
                (110.0, 210.0),
                1.0
            ),
            Some((100.0, 200.0))
        );
    }

    #[test]
    fn invalid_popover_geometry_is_rejected() {
        assert!(popover_position(
            (0.0, 0.0),
            (1920.0, 1080.0),
            (320.0, 400.0),
            (1.0, 2.0),
            f64::NAN
        )
        .is_none());
        assert!(
            popover_position((0.0, 0.0), (0.0, 1080.0), (320.0, 400.0), (1.0, 2.0), 1.0).is_none()
        );
    }
}

/// Update the tray icon title to reflect the current server metrics.
///
/// On macOS the tray title is displayed as text next to the icon:
/// - When there is a connection (bitrate > 0) — show bitrate and latency
/// - Otherwise — clear the title
pub fn update_tray_status(handle: &AppHandle, state: &str, metrics: &Metrics) {
    let title = (state == "connected").then(|| {
        format!(
            "{:.1} Mbps | {:.0} ms",
            metrics.bitrate_kbps as f64 / 1000.0,
            metrics.rtt_ms,
        )
    });
    let app = handle.clone();
    // Tauri's tray methods synchronously wait for the main thread when called
    // elsewhere. Queue publication instead: status callbacks and the independent
    // local recovery command must remain responsive if AppKit is hung.
    if let Err(error) = handle.run_on_main_thread(move || {
        if let Some(tray) = app.tray_by_id("main-tray") {
            let _ = tray.set_title(title.as_deref());
        }
    }) {
        tracing::debug!(%error, "Could not queue tray status publication");
    }
}
