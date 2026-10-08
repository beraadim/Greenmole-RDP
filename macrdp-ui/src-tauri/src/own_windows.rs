//! Move our own window through AppKit on its owning thread. Accessibility must
//! never query our PID: querying WKWebView accessibility previously crashed it.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use macrdp_core::DisplayGeometry;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSScreen, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, NSString};
use tauri::Manager;

/// Called on the serialized display worker while it retains the target screen.
/// A timed-out pending main-thread task is cancelled before the worker releases
/// that screen. The lock also lets an already-running native move finish first.
pub fn move_main_window(app: &tauri::AppHandle, target: DisplayGeometry) -> Result<usize> {
    let cancelled = Arc::new(Mutex::new(false));
    let main_cancelled = Arc::clone(&cancelled);
    let (sender, receiver) = mpsc::channel();
    let main_app = app.clone();
    app.run_on_main_thread(move || {
        let Ok(cancelled) = main_cancelled.lock() else {
            return;
        };
        if *cancelled {
            return;
        }
        let result = move_on_main_thread(&main_app, target);
        let _ = sender.send(result);
    })
    .context("Could not schedule the macrdp window move")?;
    match receiver.recv_timeout(Duration::from_secs(1)) {
        Ok(result) => result,
        Err(_) => {
            // Native AppKit frame reads/writes do not message another process.
            // If the callback started, it holds this lock through completion.
            *cancelled
                .lock()
                .map_err(|_| anyhow::anyhow!("Window move lock failed"))? = true;
            if let Ok(result) = receiver.try_recv() {
                return result;
            }
            bail!("The macrdp window is busy. Please try again.")
        }
    }
}

fn move_on_main_thread(app: &tauri::AppHandle, target: DisplayGeometry) -> Result<usize> {
    let mtm = MainThreadMarker::new().context("Window move requires the main thread")?;
    let screens = NSScreen::screens(mtm);
    let screen_key = NSString::from_str("NSScreenNumber");
    let screen = screens
        .iter()
        .find(|screen| {
            screen
                .deviceDescription()
                .objectForKey(&screen_key)
                .and_then(|value| {
                    value
                        .downcast_ref::<NSNumber>()
                        .map(|id| id.unsignedIntValue())
                })
                == Some(target.display_id)
        })
        .context("The remote display is no longer available to macrdp")?;
    let Some(window) = app.get_webview_window("main") else {
        return Ok(0);
    };
    let pointer = window
        .ns_window()
        .context("Could not access the macrdp window")?;
    ensure!(!pointer.is_null(), "The macrdp window is unavailable");
    // SAFETY: Tauri owns this NSWindow and the retained webview handle above
    // stays alive for this callback. All access occurs on AppKit's main thread.
    let native = unsafe { &*pointer.cast::<NSWindow>() };
    if !native.isVisible()
        || native.isMiniaturized()
        || native.styleMask().contains(NSWindowStyleMask::FullScreen)
    {
        return Ok(0);
    }
    let Some(frame) = fitted_frame(native.frame(), screen.visibleFrame())? else {
        return Ok(0);
    };
    native.setFrame_display(frame, true);
    tracing::info!(
        display_id = target.display_id,
        "Moved macrdp window through AppKit"
    );
    Ok(1)
}

/// AppKit rectangles share a bottom-left logical coordinate space. Using the
/// selected NSScreen's visible frame avoids Retina scaling and menu/Dock math.
fn fitted_frame(frame: NSRect, available: NSRect) -> Result<Option<NSRect>> {
    for rect in [frame, available] {
        ensure!(
            rect.origin.x.is_finite()
                && rect.origin.y.is_finite()
                && rect.size.width.is_finite()
                && rect.size.width > 0.0
                && rect.size.height.is_finite()
                && rect.size.height > 0.0
                && (rect.origin.x + rect.size.width).is_finite()
                && (rect.origin.y + rect.size.height).is_finite(),
            "Invalid window or display bounds"
        );
    }
    let width = frame.size.width.min(available.size.width);
    let height = frame.size.height.min(available.size.height);
    let x = frame.origin.x.clamp(
        available.origin.x,
        available.origin.x + available.size.width - width,
    );
    let y = frame.origin.y.clamp(
        available.origin.y,
        available.origin.y + available.size.height - height,
    );
    let fitted = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    Ok((fitted != frame).then_some(fitted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    #[test]
    fn main_window_fits_negative_display_origin_and_reserved_dock_area() {
        let available = rect(-1920.0, 80.0, 1920.0, 970.0);
        let result = fitted_frame(rect(100.0, 0.0, 960.0, 620.0), available)
            .unwrap()
            .unwrap();
        assert_eq!(result, rect(-960.0, 80.0, 960.0, 620.0));
    }

    #[test]
    fn oversized_window_fits_small_remote_viewport_without_pixel_scaling() {
        let available = rect(0.0, 24.0, 640.0, 430.0);
        assert_eq!(
            fitted_frame(rect(2560.0, 100.0, 960.0, 620.0), available).unwrap(),
            Some(available)
        );
        assert_eq!(fitted_frame(available, available).unwrap(), None);
    }

    #[test]
    fn invalid_geometry_never_reaches_appkit() {
        assert!(fitted_frame(
            rect(f64::NAN, 0.0, 900.0, 600.0),
            rect(0.0, 0.0, 1920.0, 1080.0)
        )
        .is_err());
        assert!(fitted_frame(rect(0.0, 0.0, 900.0, 600.0), rect(0.0, 0.0, 0.0, 1080.0)).is_err());
    }
}
