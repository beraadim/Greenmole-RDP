use anyhow::{bail, ensure, Context, Result};
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_graphics::display::{
    CGDisplay, CGDisplayCopyAllDisplayModes, CGDisplayModeGetHeight, CGDisplayModeGetPixelHeight,
    CGDisplayModeGetPixelWidth, CGDisplayModeGetWidth,
};
use objc2::rc::autoreleasepool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::virtual_display::VirtualDisplay;
use crate::{Command, DisplayBackend, DisplayGeometry};

const SETTLE_TIMEOUT: Duration = Duration::from_secs(3);
const SETTLE_POLL: Duration = Duration::from_millis(50);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(
        max_displays: u32,
        online_displays: *mut u32,
        display_count: *mut u32,
    ) -> i32;
}

fn online_display_ids() -> Result<Vec<u32>> {
    let mut capacity = 16u32;
    for _ in 0..4 {
        let mut ids = vec![0; capacity as usize];
        let mut count = 0;
        // SAFETY: the writable ID buffer has the advertised capacity, and count
        // is a valid output pointer. A full result is retried to avoid truncation.
        let status = unsafe { CGGetOnlineDisplayList(capacity, ids.as_mut_ptr(), &mut count) };
        ensure!(status == 0, "Could not enumerate online displays: {status}");
        if count < capacity {
            ids.truncate(count as usize);
            return Ok(ids);
        }
        capacity *= 2;
    }
    bail!("Online display list exceeded the enumeration limit")
}

pub(crate) struct NativeDisplay {
    display_id: u32,
    virtual_display: Option<VirtualDisplay>,
    session_layout: Option<crate::layout::Snapshot>,
}

impl NativeDisplay {
    pub(crate) fn new(backend: DisplayBackend, width: u16, height: u16) -> Result<Self> {
        match backend {
            DisplayBackend::Main => {
                let display_id = CGDisplay::main().id;
                ensure!(display_id != 0, "No main display is available");
                let owner = Self {
                    display_id,
                    virtual_display: None,
                    session_layout: None,
                };
                let geometry = owner.geometry()?;
                tracing::info!(?geometry, "Pinned main display without changing its mode");
                Ok(owner)
            }
            DisplayBackend::Virtual => {
                let virtual_display = VirtualDisplay::new(width, height)?;
                let owner = Self {
                    display_id: virtual_display.id(),
                    virtual_display: Some(virtual_display),
                    session_layout: None,
                };
                owner.wait_for_mode(width, height)?;
                Ok(owner)
            }
        }
    }

    pub(crate) fn geometry(&self) -> Result<DisplayGeometry> {
        let display = CGDisplay::new(self.display_id);
        // CoreGraphics flags can report true for an ID that has disappeared.
        // Membership must be established before requesting its mode or bounds.
        ensure!(
            online_display_ids()?.contains(&self.display_id),
            "Selected display {} is no longer online",
            self.display_id
        );
        if self.virtual_display.is_some() {
            ensure!(
                CGDisplay::active_displays()
                    .map_err(|error| anyhow::anyhow!(
                        "Could not enumerate active displays: {error}"
                    ))?
                    .contains(&self.display_id),
                "Selected virtual display {} is not active",
                self.display_id
            );
            ensure!(
                !display.is_in_mirror_set(),
                "Virtual display {} is mirrored; an extended display is required",
                self.display_id
            );
        }
        // A sleeping physical screen may be online but inactive. Startup only
        // needs its valid mode and bounds; connection handling wakes it later.
        let bounds = display.bounds();
        let mode = display.display_mode().context(
            "Selected display has no current mode; cannot verify framebuffer dimensions",
        )?;
        let (pixel_width, pixel_height) = (mode.pixel_width(), mode.pixel_height());
        let geometry = DisplayGeometry {
            display_id: self.display_id,
            origin_x: bounds.origin.x,
            origin_y: bounds.origin.y,
            logical_width: bounds.size.width,
            logical_height: bounds.size.height,
            pixel_width: u32::try_from(pixel_width).context("Display pixel width is too large")?,
            pixel_height: u32::try_from(pixel_height)
                .context("Display pixel height is too large")?,
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
            "Selected display reported invalid geometry: {geometry:?}"
        );
        Ok(geometry)
    }

    fn live_layout(&self) -> Result<Vec<crate::layout::Placement>> {
        let ids = CGDisplay::active_displays()
            .map_err(|error| anyhow::anyhow!("Could not query display layout: {error}"))?;
        ensure!(
            ids.len() <= 64,
            "Display layout exceeded the enumeration limit"
        );
        ids.into_iter()
            .map(|id| {
                let display = CGDisplay::new(id);
                ensure!(
                    !display.is_in_mirror_set(),
                    "Temporary primary display requires extended displays"
                );
                let bounds = display.bounds();
                fn coordinate(value: f64) -> Result<i32> {
                    ensure!(
                        value.is_finite()
                            && value.fract() == 0.0
                            && value >= f64::from(i32::MIN)
                            && value <= f64::from(i32::MAX),
                        "Invalid display layout coordinate"
                    );
                    Ok(value as i32)
                }
                Ok(crate::layout::Placement {
                    id,
                    x: coordinate(bounds.origin.x)?,
                    y: coordinate(bounds.origin.y)?,
                    width: coordinate(bounds.size.width)?,
                    height: coordinate(bounds.size.height)?,
                })
            })
            .collect()
    }

    pub(crate) fn begin_remote_session(&mut self) -> Result<DisplayGeometry> {
        let Some(virtual_display) = &self.virtual_display else {
            return self.geometry();
        };
        if self.session_layout.is_some() {
            CGDisplay::active_displays().map_err(|error| {
                anyhow::anyhow!("Could not refresh the primary display: {error}")
            })?;
            ensure!(
                CGDisplay::main().id == self.display_id,
                "The remote primary display changed during the connection"
            );
            return self.geometry();
        }
        let snapshot = crate::layout::Snapshot {
            primary: CGDisplay::main().id,
            displays: self.live_layout()?,
        };
        if snapshot.primary == self.display_id {
            // The user may have selected this screen as primary while idle.
            // Preserve that explicit arrangement on disconnect; do not invent
            // a physical primary that differs from the pre-connection layout.
            self.session_layout = Some(snapshot);
            return self.geometry();
        }
        let displays = snapshot.promoted(self.display_id)?;
        let serial = virtual_display.serial();
        // Retain the original before native mutation so teardown can restore it
        // even after a configuration timeout or a partially reported failure.
        self.session_layout = Some(snapshot);
        let geometry = crate::helper::DisplayHelper::configure_layout(
            self.display_id,
            serial,
            self.display_id,
            displays,
        )?;
        tracing::info!(
            display_id = self.display_id,
            ?geometry,
            "Remote virtual display became the temporary primary display"
        );
        Ok(geometry)
    }

    pub(crate) fn end_remote_session(&mut self) -> Result<DisplayGeometry> {
        let Some(snapshot) = &self.session_layout else {
            return self.geometry();
        };
        if snapshot.primary == self.display_id {
            self.session_layout = None;
            return self.geometry();
        }
        let live = self.live_layout()?;
        let (primary, displays) = snapshot.restored(&live, self.display_id)?;
        let serial = self
            .virtual_display
            .as_ref()
            .context("Missing owned virtual display")?
            .serial();
        let geometry = crate::helper::DisplayHelper::configure_layout(
            self.display_id,
            serial,
            primary,
            displays,
        )?;
        self.session_layout = None;
        tracing::info!(primary, "Restored the physical primary display and layout");
        Ok(geometry)
    }

    fn matches_mode(&self, geometry: &DisplayGeometry, width: u16, height: u16) -> bool {
        geometry.pixel_width == u32::from(width)
            && geometry.pixel_height == u32::from(height)
            && geometry.logical_width == f64::from(width)
            && geometry.logical_height == f64::from(height)
    }

    pub(crate) fn prepare_capture(&mut self, width: u16, height: u16) -> Result<DisplayGeometry> {
        if self.virtual_display.is_none() {
            return self.geometry();
        }
        if let Ok(geometry) = self.geometry() {
            if self.matches_mode(&geometry, width, height) {
                return Ok(geometry);
            }
        }
        self.virtual_display
            .as_mut()
            .expect("virtual owner checked")
            .apply_mode(width, height)?;
        let geometry = self.wait_for_mode(width, height)?;
        tracing::info!(?geometry, "Virtual display mode ready for capture");
        Ok(geometry)
    }

    fn wait_for_mode(&self, width: u16, height: u16) -> Result<DisplayGeometry> {
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        let mut selected_mode = false;
        tracing::debug!(
            display_id = self.display_id,
            width,
            height,
            "Waiting for virtual display mode"
        );
        loop {
            // Refresh the process's active topology before reading live bounds.
            // This does not fix the separate stale CGDisplayMode cache on Tahoe.
            CGDisplay::active_displays()
                .map_err(|error| anyhow::anyhow!("Could not enumerate active displays: {error}"))?;
            let observed = self.geometry();
            if let Ok(geometry) = &observed {
                if self.matches_mode(geometry, width, height) {
                    return Ok(*geometry);
                }
            }
            if !selected_mode {
                // macOS may choose a different advertised mode after settings
                // change. Selection is confined to the helper's owned display.
                selected_mode = self.select_matching_mode(width, height)?;
            }
            if Instant::now() >= deadline {
                let detail = match observed {
                    Ok(geometry) => format!("last geometry: {geometry:?}"),
                    Err(error) => format!("last error: {error:#}"),
                };
                let diagnostics = self.display_diagnostics();
                bail!("Virtual display {} did not settle to {width}x{height} within 3 seconds ({detail}; {diagnostics})", self.display_id);
            }
            std::thread::sleep(SETTLE_POLL);
        }
    }

    fn select_matching_mode(&self, width: u16, height: u16) -> Result<bool> {
        ensure!(
            self.virtual_display.is_some(),
            "Physical display mode changes are forbidden"
        );
        let display = CGDisplay::new(self.display_id);
        let active = CGDisplay::active_displays()
            .map_err(|error| anyhow::anyhow!("Could not enumerate active displays: {error}"))?;
        if !online_display_ids()?.contains(&self.display_id) || !active.contains(&self.display_id) {
            return Ok(false);
        }
        ensure!(
            !display.is_in_mirror_set(),
            "Refusing to change a mirrored virtual display mode"
        );
        let serial = self
            .virtual_display
            .as_ref()
            .expect("virtual owner checked")
            .serial();
        crate::helper::DisplayHelper::select_mode(self.display_id, serial, width, height)?;
        Ok(true)
    }

    fn display_diagnostics(&self) -> String {
        // These read-only observations distinguish a display that never became
        // active from macOS's stale per-process CGDisplayMode cache. The raw
        // pixels-wide/high functions report logical dimensions in HiDPI modes;
        // they are diagnostic data, never a substitute for framebuffer size.
        let active_ids = CGDisplay::active_displays();
        let display = CGDisplay::new(self.display_id);
        let bounds = display.bounds();
        let mode = display.display_mode().map(|mode| {
            (
                mode.width(),
                mode.height(),
                mode.pixel_width(),
                mode.pixel_height(),
            )
        });
        // SAFETY: The returned array is owned; contained modes are borrowed only
        // until it is released. Limit the diagnostic list to avoid noisy logs.
        let available = unsafe {
            let raw = CGDisplayCopyAllDisplayModes(self.display_id, std::ptr::null());
            if raw.is_null() {
                "unavailable".to_owned()
            } else {
                let modes: CFArray<*const std::ffi::c_void> = CFArray::wrap_under_create_rule(raw);
                let count = modes.len() as usize;
                let sizes: Vec<_> = modes
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index < 8 || *index >= count.saturating_sub(8))
                    .filter_map(|(index, value)| {
                        let mode = *value as core_graphics::sys::CGDisplayModeRef;
                        (!mode.is_null()).then(|| {
                            (
                                index,
                                CGDisplayModeGetWidth(mode),
                                CGDisplayModeGetHeight(mode),
                                CGDisplayModeGetPixelWidth(mode),
                                CGDisplayModeGetPixelHeight(mode),
                            )
                        })
                    })
                    .collect();
                format!("count={count}, first_and_last_modes={sizes:?}")
            }
        };
        format!(
            "active_ids={active_ids:?}, online={}, active={}, mirrored={}, bounds={bounds:?}, raw_dimensions={}x{}, current_mode={mode:?}, available_modes=({available})",
            display.is_online(), display.is_active(), display.is_in_mirror_set(),
            display.pixels_wide(), display.pixels_high()
        )
    }
}

impl Drop for NativeDisplay {
    fn drop(&mut self) {
        if self.session_layout.is_some() {
            if let Err(error) = self.end_remote_session() {
                // The protocol helper intentionally has no tracing subscriber.
                eprintln!("Could not restore the display layout during owner teardown: {error:#}");
                tracing::warn!(%error, "Could not restore the display layout during owner teardown");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_rejects_unknown_display_id() {
        let display_id = u32::MAX;
        assert!(!online_display_ids().unwrap().contains(&display_id));
        let selected = NativeDisplay {
            display_id,
            virtual_display: None,
            session_layout: None,
        };
        let error = selected.geometry().unwrap_err();
        assert!(error.to_string().contains("no longer online"));
    }

    #[test]
    fn disconnected_window_action_cannot_create_a_display_or_call_appkit() {
        let mut owner = WorkerOwner::Virtual {
            helper: None,
            initial_width: 1280,
            initial_height: 720,
        };
        let (request, _preparation) = crate::window_move::Request::new(|_| {
            panic!("disconnected action must never reach application windows")
        });
        let error = owner
            .move_windows(request.budget, request.own_windows)
            .unwrap_err();
        assert!(error.to_string().contains("Connect a remote client"));
        assert_eq!(owner.display_id(), 0);
    }
}

enum WorkerOwner {
    Main(NativeDisplay),
    Virtual {
        helper: Option<crate::helper::DisplayHelper>,
        initial_width: u16,
        initial_height: u16,
    },
}

impl WorkerOwner {
    fn move_windows(
        &mut self,
        budget: crate::window_move::Budget,
        own_windows: Box<dyn FnOnce(DisplayGeometry) -> Result<usize> + Send>,
    ) -> Result<crate::WindowMoveReport> {
        budget.check()?;
        ensure!(
            matches!(
                self,
                Self::Virtual {
                    helper: Some(_),
                    ..
                }
            ),
            "Connect a remote client before moving windows to its virtual display"
        );
        // Geometry comes from the owner helper, never from a remembered ID
        // that macOS could have reassigned. Ownership survives this whole call.
        let geometry = self.geometry()?;
        budget.check()?;
        crate::manual_windows::move_windows(geometry, budget, own_windows)
    }

    fn begin_remote_session(&mut self) -> Result<DisplayGeometry> {
        match self {
            Self::Main(owner) => owner.begin_remote_session(),
            Self::Virtual {
                helper,
                initial_width,
                initial_height,
            } => {
                if helper.is_none() {
                    *helper = Some(crate::helper::DisplayHelper::new(
                        *initial_width,
                        *initial_height,
                    )?);
                }
                helper
                    .as_mut()
                    .expect("initialized session display")
                    .begin_remote_session()
            }
        }
    }

    fn end_remote_session(&mut self) -> Result<()> {
        match self {
            Self::Main(owner) => owner.end_remote_session().map(|_| ()),
            Self::Virtual { helper, .. } => {
                let Some(mut owner) = helper.take() else {
                    return Ok(());
                };
                let display_id = owner.id();
                let result = owner.end_remote_session().map(|_| ());
                // Drop and reap before acknowledging disconnect. The next
                // queued activation may then create its own display safely.
                drop(owner);
                let removed = wait_for_display_removal(display_id);
                result.and(removed)
            }
        }
    }
    fn geometry(&mut self) -> Result<DisplayGeometry> {
        match self {
            Self::Main(owner) => owner.geometry(),
            Self::Virtual { helper, .. } => helper
                .as_mut()
                .context("No remote display exists while disconnected")?
                .geometry(),
        }
    }

    fn prepare_capture(&mut self, width: u16, height: u16) -> Result<DisplayGeometry> {
        match self {
            Self::Main(owner) => owner.prepare_capture(width, height),
            Self::Virtual { helper, .. } => helper
                .as_mut()
                .context("Cannot prepare capture before the remote connection begins")?
                .prepare_capture(width, height),
        }
    }

    fn prepare_remote_capture(&mut self, width: u16, height: u16) -> Result<DisplayGeometry> {
        if let Self::Virtual { helper, .. } = self {
            if helper.is_none() {
                // Use the negotiated client dimensions for the very first
                // mode; no startup-size screen or extra initial resize exists.
                *helper = Some(crate::helper::DisplayHelper::new(width, height)?);
            }
        }
        let result = self
            .begin_remote_session()
            .and_then(|_| self.prepare_capture(width, height));
        if result.is_err() {
            if let Err(error) = self.end_remote_session() {
                tracing::warn!(%error, "Could not fully clean up failed remote display activation");
            }
        }
        result
    }

    fn display_id(&self) -> u32 {
        match self {
            Self::Main(owner) => owner.display_id,
            Self::Virtual { helper, .. } => helper.as_ref().map_or(0, |owner| owner.id()),
        }
    }
}

fn wait_for_display_removal(display_id: u32) -> Result<()> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        // Check membership, not stale online/active flags for a removed ID.
        if !online_display_ids()?.contains(&display_id) {
            tracing::info!(display_id, "Removed the disconnected remote display");
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Disconnected virtual display {display_id} was not removed within 3 seconds"
        );
        std::thread::sleep(SETTLE_POLL);
    }
}

pub(crate) fn run_worker(
    backend: DisplayBackend,
    width: u16,
    height: u16,
    receiver: mpsc::Receiver<Command>,
    ready: mpsc::Sender<Result<()>>,
    stopping: Arc<AtomicBool>,
    stopped: mpsc::Sender<()>,
) {
    let mut owner = match autoreleasepool(|_| match backend {
        DisplayBackend::Main => NativeDisplay::new(backend, width, height).map(WorkerOwner::Main),
        DisplayBackend::Virtual => Ok(WorkerOwner::Virtual {
            helper: None,
            initial_width: width,
            initial_height: height,
        }),
    }) {
        Ok(owner) => owner,
        Err(error) => {
            tracing::error!(%error, ?backend, "Display controller initialization failed");
            let _ = ready.send(Err(error));
            let _ = stopped.send(());
            return;
        }
    };
    if ready.send(Ok(())).is_ok() {
        while let Ok(command) = receiver.recv() {
            if stopping.load(Ordering::Acquire) || matches!(command, Command::Shutdown) {
                break;
            }
            autoreleasepool(|_| match command {
                Command::Shutdown => {}
                Command::Geometry(reply) => {
                    let _ = reply.send(owner.geometry());
                }
                Command::BeginRemoteSession(reply) => {
                    let _ = reply.send(owner.begin_remote_session());
                }
                Command::EndRemoteSession(reply) => {
                    let result = owner.end_remote_session();
                    if let Err(error) = &result {
                        tracing::warn!(%error, "Could not restore the display layout after disconnect");
                    }
                    let _ = reply.send(result);
                }
                Command::MoveWindows(request) => {
                    let result = owner.move_windows(request.budget, request.own_windows);
                    if let Err(error) = &result {
                        tracing::warn!(%error, "Manual window relocation could not run");
                    }
                    let _ = request.reply.send(result);
                }
                Command::PrepareRemoteCapture {
                    width,
                    height,
                    reply,
                } => {
                    let result = owner.prepare_remote_capture(width, height);
                    let _ = reply.send(result);
                }
                Command::PrepareCapture {
                    width,
                    height,
                    reply,
                } => {
                    let result = owner.prepare_capture(width, height);
                    if let Err(error) = &result {
                        tracing::error!(%error, display_id = owner.display_id(), width, height,
                            "Could not prepare selected display; capture will not switch displays");
                    }
                    let _ = reply.send(result);
                }
            });
        }
    }
    let display_id = owner.display_id();
    autoreleasepool(|_| drop(owner));
    let _ = stopped.send(());
    tracing::info!(
        display_id,
        ?backend,
        "Display controller released on its owning thread"
    );
}
