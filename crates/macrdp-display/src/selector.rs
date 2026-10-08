//! One-shot mode selection avoids both stale mode lists and synchronous
//! reconfiguration inside the process that owns the virtual display.

use anyhow::{bail, ensure, Context, Result};
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_graphics::display::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayOrigin, CGConfigureDisplayWithDisplayMode, CGConfigureOption, CGDisplay,
    CGDisplayCopyAllDisplayModes, CGDisplayModeGetHeight, CGDisplayModeGetPixelHeight,
    CGDisplayModeGetPixelWidth, CGDisplayModeGetWidth,
};
use std::sync::mpsc;
use std::time::Duration;

use crate::virtual_display::{PRODUCT_ID, VENDOR_ID};
use crate::DisplayGeometry;

pub(crate) fn select_mode(
    display_id: u32,
    serial: u32,
    width: u16,
    height: u16,
) -> Result<DisplayGeometry> {
    let _finished = native_deadline()?;
    select_native(display_id, serial, width, height)
}

fn native_deadline() -> Result<mpsc::Sender<()>> {
    // A native synchronous call can hang. This process must stop independently
    // even if its owner is killed while pumping reconfiguration callbacks.
    let (finished, completion) = mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("display-selector-deadline".to_owned())
        .spawn(move || {
            if matches!(
                completion.recv_timeout(Duration::from_secs(2)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                eprintln!("Display configuration selector exceeded its two-second native deadline");
                std::process::exit(124);
            }
        })
        .context("Could not start mode selector deadline")?;
    Ok(finished)
}

fn owned_display(display_id: u32, serial: u32) -> Result<CGDisplay> {
    let active = CGDisplay::active_displays()
        .map_err(|error| anyhow::anyhow!("Could not enumerate selector displays: {error}"))?;
    let display = CGDisplay::new(display_id);
    ensure!(
        display_id != 0
            && active.contains(&display_id)
            && display.is_online()
            && display.is_active()
            && !display.is_builtin()
            && !display.is_in_mirror_set()
            && display.vendor_number() == VENDOR_ID
            && display.model_number() == PRODUCT_ID
            && serial != 0
            && display.serial_number() == serial,
        "Configuration target does not match the owned extended virtual display"
    );
    Ok(display)
}

fn select_native(display_id: u32, serial: u32, width: u16, height: u16) -> Result<DisplayGeometry> {
    let display = owned_display(display_id, serial)?;
    // SAFETY: The copied array owns the mode objects through the synchronous
    // operation. The target identity is verified above and no physical screen
    // or unrelated virtual display is permitted.
    unsafe {
        let raw = CGDisplayCopyAllDisplayModes(display_id, std::ptr::null());
        ensure!(
            !raw.is_null(),
            "Mode selector could not read available modes"
        );
        let modes: CFArray<*const std::ffi::c_void> = CFArray::wrap_under_create_rule(raw);
        let mut selected = false;
        for value in modes.iter() {
            let mode = *value as core_graphics::sys::CGDisplayModeRef;
            if !mode.is_null()
                && CGDisplayModeGetWidth(mode) == usize::from(width)
                && CGDisplayModeGetHeight(mode) == usize::from(height)
                && CGDisplayModeGetPixelWidth(mode) == usize::from(width)
                && CGDisplayModeGetPixelHeight(mode) == usize::from(height)
            {
                let mut configuration = std::ptr::null_mut();
                let status = CGBeginDisplayConfiguration(&mut configuration);
                ensure!(
                    status == 0 && !configuration.is_null(),
                    "Mode selector could not begin configuration: CoreGraphics error {status}"
                );
                let status = CGConfigureDisplayWithDisplayMode(
                    configuration,
                    display_id,
                    mode,
                    std::ptr::null(),
                );
                if status != 0 {
                    let cancel_status = CGCancelDisplayConfiguration(configuration);
                    bail!("Mode selector could not stage {width}x{height}: CoreGraphics error {status}; cancel status {cancel_status}");
                }
                // A direct SetDisplayMode is application-scoped and reverts when
                // this short-lived selector exits. Session scope preserves only
                // this owned virtual mode without writing permanent preferences.
                // Complete consumes configuration even when it reports failure;
                // it must not be cancelled or reused after this call.
                let status = CGCompleteDisplayConfiguration(
                    configuration,
                    CGConfigureOption::ConfigureForSession,
                );
                ensure!(status == 0,
                    "Mode selector could not commit {width}x{height} for the session: CoreGraphics error {status}");
                selected = true;
                break;
            }
        }
        if !selected {
            bail!(
                "Fresh mode selector did not find exact {width}x{height} among {} modes",
                modes.len()
            );
        }
    }
    let mode = display
        .display_mode()
        .context("Mode selector cannot verify current mode")?;
    let bounds = display.bounds();
    ensure!(
        mode.pixel_width() == u64::from(width)
            && mode.pixel_height() == u64::from(height)
            && bounds.size.width == f64::from(width)
            && bounds.size.height == f64::from(height),
        "Mode selector did not reach exact {width}x{height}"
    );
    Ok(DisplayGeometry {
        display_id,
        origin_x: bounds.origin.x,
        origin_y: bounds.origin.y,
        logical_width: bounds.size.width,
        logical_height: bounds.size.height,
        pixel_width: u32::from(width),
        pixel_height: u32::from(height),
    })
}

pub(crate) fn configure_layout(
    display_id: u32,
    serial: u32,
    primary: u32,
    displays: &[crate::layout::Placement],
) -> Result<DisplayGeometry> {
    let _finished = native_deadline()?;
    let display = owned_display(display_id, serial)?;
    crate::layout::validate(displays, primary, display_id)?;
    let active = CGDisplay::active_displays()
        .map_err(|error| anyhow::anyhow!("Could not enumerate layout displays: {error}"))?;
    ensure!(
        active.len() == displays.len() && displays.iter().all(|d| active.contains(&d.id)),
        "Display topology changed before layout configuration"
    );
    for placement in displays {
        let screen = CGDisplay::new(placement.id);
        let bounds = screen.bounds();
        ensure!(
            !screen.is_in_mirror_set()
                && bounds.size.width == f64::from(placement.width)
                && bounds.size.height == f64::from(placement.height),
            "Display dimensions or mirroring changed before layout configuration"
        );
    }
    // SAFETY: every display is active, dimensions and owned target identity
    // were checked. This transaction only changes origins, never physical modes.
    unsafe {
        let mut configuration = std::ptr::null_mut();
        let status = CGBeginDisplayConfiguration(&mut configuration);
        ensure!(
            status == 0 && !configuration.is_null(),
            "Could not begin display layout configuration: CoreGraphics error {status}"
        );
        // Move the former primary away before assigning the new origin. All
        // staged changes commit atomically, with session scope only.
        for placement in displays
            .iter()
            .filter(|d| d.id != primary)
            .chain(displays.iter().filter(|d| d.id == primary))
        {
            let status =
                CGConfigureDisplayOrigin(configuration, placement.id, placement.x, placement.y);
            if status != 0 {
                let cancel_status = CGCancelDisplayConfiguration(configuration);
                bail!("Could not stage display {} origin: CoreGraphics error {status}; cancel status {cancel_status}", placement.id);
            }
        }
        let status =
            CGCompleteDisplayConfiguration(configuration, CGConfigureOption::ConfigureForSession);
        ensure!(
            status == 0,
            "Could not commit display layout: CoreGraphics error {status}"
        );
    }
    ensure!(
        CGDisplay::main().id == primary,
        "macOS did not activate the requested primary display"
    );
    for placement in displays {
        let bounds = CGDisplay::new(placement.id).bounds();
        if bounds.origin.x != f64::from(placement.x) || bounds.origin.y != f64::from(placement.y) {
            // Hotplugging or changing physical modes can invalidate the saved
            // arrangement. Quartz resolves gaps/overlaps; retain actual bounds
            // rather than treating a successfully restored primary as failure.
            eprintln!(
                "macOS adjusted display {} origin to {},{} instead of {},{}",
                placement.id, bounds.origin.x, bounds.origin.y, placement.x, placement.y
            );
        }
    }
    let mode = display
        .display_mode()
        .context("Cannot verify virtual display after layout change")?;
    let bounds = display.bounds();
    Ok(DisplayGeometry {
        display_id,
        origin_x: bounds.origin.x,
        origin_y: bounds.origin.y,
        logical_width: bounds.size.width,
        logical_height: bounds.size.height,
        pixel_width: u32::try_from(mode.pixel_width()).context("Display width overflow")?,
        pixel_height: u32::try_from(mode.pixel_height()).context("Display height overflow")?,
    })
}
