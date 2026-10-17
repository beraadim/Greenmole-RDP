//! Reveal controls independently of capture and virtual-display ownership.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use macrdp_core::DisplayGeometry;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSScreen, NSWindow};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSString};
use tauri::Manager;

use super::{fitted_frame, valid_rect};

/// Ordinary Open/Reopen follows the pointer, including a remote virtual screen.
/// Moving every reopen to a physical monitor makes the controls disappear from
/// the remote user who requested them.
pub fn show_main_window(app: &tauri::AppHandle) -> Result<usize> {
    reveal_main_window(app, RevealTarget::Pointer)
}

/// Emergency recovery deliberately chooses a physical monitor. A failed display
/// query is carried to the main thread so it cannot suppress window visibility.
pub fn show_main_window_on_display(
    app: &tauri::AppHandle,
    target: Result<DisplayGeometry>,
) -> Result<usize> {
    reveal_main_window(app, RevealTarget::Display(target))
}

enum RevealTarget {
    Pointer,
    Display(Result<DisplayGeometry>),
}

/// Reopening never waits on the capture or display owner. A callback that has
/// not started is cancelled after the bound; AppKit already in progress is not
/// freed or interrupted when the caller stops waiting.
fn reveal_main_window(app: &tauri::AppHandle, target: RevealTarget) -> Result<usize> {
    let claimed_or_cancelled = Arc::new(AtomicBool::new(false));
    let main_cancelled = Arc::clone(&claimed_or_cancelled);
    let (sender, receiver) = mpsc::channel();
    let main_app = app.clone();
    app.run_on_main_thread(move || {
        if !main_cancelled.swap(true, Ordering::AcqRel) {
            let _ = sender.send(reveal_on_main_thread(&main_app, target));
        }
    })
    .context("Could not schedule the macrdp window reveal")?;
    match receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result,
        Err(_) => {
            // Unlike a remote move, this does not retain an owned display. A
            // native AppKit call already in progress may finish later; do not
            // make the independent recovery command wait for a hung UI thread.
            claimed_or_cancelled.store(true, Ordering::Release);
            bail!("The macrdp window is not responding. Controls could not be shown.")
        }
    }
}

fn reveal_on_main_thread(app: &tauri::AppHandle, target: RevealTarget) -> Result<usize> {
    let mtm = MainThreadMarker::new().context("Window reveal requires the main thread")?;
    let window = app
        .get_webview_window("main")
        .context("The macrdp main window is unavailable")?;
    // Fullscreen has a separate Space. Leaving it and fitting the window are
    // best effort; failed geometry must never leave a hidden window hidden.
    let placement = (|| {
        window
            .set_fullscreen(false)
            .context("Could not leave fullscreen while opening macrdp")?;
        let pointer = window
            .ns_window()
            .context("Could not access the macrdp window")?;
        ensure!(!pointer.is_null(), "The macrdp window is unavailable");
        // SAFETY: Tauri retains this NSWindow through `window`; all access is
        // confined to its AppKit main thread. Never use Accessibility on self.
        let native = unsafe { &*pointer.cast::<NSWindow>() };
        let screens = NSScreen::screens(mtm);
        let screen = match target {
            RevealTarget::Pointer => {
                let frames: Vec<_> = screens.iter().map(|screen| screen.frame()).collect();
                let index = screen_at_pointer(&frames, NSEvent::mouseLocation())
                    .context("The pointer is not on an available display")?;
                screens.iter().nth(index)
            }
            RevealTarget::Display(target) => {
                let display_id = target?.display_id;
                let screen_key = NSString::from_str("NSScreenNumber");
                screens.iter().find(|screen| {
                    screen
                        .deviceDescription()
                        .objectForKey(&screen_key)
                        .and_then(|value| {
                            value
                                .downcast_ref::<NSNumber>()
                                .map(|id| id.unsignedIntValue())
                        })
                        == Some(display_id)
                })
            }
        }
        .context("The selected display is no longer available to macrdp")?;
        let frame = fitted_frame(native.frame(), screen.visibleFrame())?;
        if let Some(frame) = frame {
            native.setFrame_display(frame, true);
        }
        Ok(usize::from(frame.is_some()))
    })();
    reveal_after_placement(placement, || {
        // Evaluate every operation before returning an error: even a failed
        // unminimize or app-unhide must not prevent show/focus from being tried.
        let operations = [
            app.show()
                .context("Could not unhide the macrdp application"),
            window
                .unminimize()
                .context("Could not restore the macrdp window"),
            window.show().context("Could not show the macrdp window"),
            window
                .set_focus()
                .context("Could not focus the macrdp window"),
        ];
        for operation in operations {
            operation?;
        }
        tracing::info!("Revealed macrdp main window");
        Ok(())
    })
}

fn reveal_after_placement(
    placement: Result<usize>,
    reveal: impl FnOnce() -> Result<()>,
) -> Result<usize> {
    let moved = placement.unwrap_or_else(|error| {
        tracing::warn!(%error, "Could not position macrdp; revealing its existing window");
        0
    });
    reveal()?;
    Ok(moved)
}

/// NSScreen and NSEvent both use AppKit's bottom-left logical coordinate space.
/// Half-open bounds make a pointer on adjacent display edges unambiguous.
fn screen_at_pointer(frames: &[NSRect], pointer: NSPoint) -> Option<usize> {
    if !pointer.x.is_finite() || !pointer.y.is_finite() {
        return None;
    }
    frames.iter().position(|frame| {
        valid_rect(*frame)
            && pointer.x >= frame.origin.x
            && pointer.y >= frame.origin.y
            && pointer.x < frame.origin.x + frame.size.width
            && pointer.y < frame.origin.y + frame.size.height
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    #[test]
    fn reopen_follows_pointer_to_negative_origin_secondary_display() {
        let displays = [
            rect(0.0, 0.0, 1920.0, 1080.0),
            rect(-2560.0, -240.0, 2560.0, 1440.0),
        ];
        assert_eq!(
            screen_at_pointer(&displays, NSPoint::new(-100.0, -100.0)),
            Some(1)
        );
        assert_eq!(
            screen_at_pointer(&displays, NSPoint::new(100.0, 100.0)),
            Some(0)
        );
    }

    #[test]
    fn remote_pointer_selects_virtual_display_without_primary_assumption() {
        // Display identity and primary status do not affect pointer selection.
        let displays = [
            rect(0.0, 0.0, 1920.0, 1080.0),
            rect(1920.0, 0.0, 1920.0, 1080.0),
        ];
        assert_eq!(
            screen_at_pointer(&displays, NSPoint::new(2500.0, 500.0)),
            Some(1)
        );
        assert_eq!(
            screen_at_pointer(&displays, NSPoint::new(1920.0, 0.0)),
            Some(1)
        );
    }

    #[test]
    fn invalid_pointer_or_display_geometry_never_selects_a_screen() {
        let displays = [
            rect(f64::NAN, 0.0, 1920.0, 1080.0),
            rect(0.0, 0.0, 0.0, 1080.0),
        ];
        assert_eq!(
            screen_at_pointer(&displays, NSPoint::new(100.0, 100.0)),
            None
        );
        assert_eq!(
            screen_at_pointer(
                &[rect(0.0, 0.0, 1920.0, 1080.0)],
                NSPoint::new(f64::NAN, 0.0)
            ),
            None
        );
        assert_eq!(screen_at_pointer(&[], NSPoint::new(0.0, 0.0)), None);
    }

    #[test]
    fn failed_screen_discovery_or_fit_still_reveals_the_window() {
        let revealed = std::cell::Cell::new(0);
        for placement in [
            Err(anyhow::anyhow!("No physical monitor is available")),
            fitted_frame(
                rect(f64::NAN, 0.0, 960.0, 620.0),
                rect(0.0, 0.0, 1920.0, 1080.0),
            )
            .map(|frame| usize::from(frame.is_some())),
        ] {
            assert_eq!(
                reveal_after_placement(placement, || {
                    revealed.set(revealed.get() + 1);
                    Ok(())
                })
                .unwrap(),
                0
            );
        }
        assert_eq!(revealed.get(), 2);
    }

    #[test]
    fn actual_reveal_failure_is_reported_even_after_placement_fails() {
        let result = reveal_after_placement(Err(anyhow::anyhow!("Display disappeared")), || {
            Err(anyhow::anyhow!("Window could not be shown"))
        });
        assert_eq!(result.unwrap_err().to_string(), "Window could not be shown");
    }
}
