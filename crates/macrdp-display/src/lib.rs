//! Selected-display ownership and geometry for remote desktop capture.
//!
//! Virtual displays are extended displays in the existing macOS login session.
//! A controller owns its display for the server lifetime; reconnecting or
//! resizing changes its mode without removing it or rearranging other displays.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[cfg(target_os = "macos")]
mod helper;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod protocol;
mod resolution;
#[cfg(target_os = "macos")]
mod selector;
#[cfg(target_os = "macos")]
mod virtual_display;
pub use resolution::resolve_desktop_size;
const HELPER_ARGUMENT: &str = "--macrdp-display-helper";

/// Main preserves the physical desktop; Virtual owns a separate extended screen.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayBackend {
    #[default]
    Main,
    Virtual,
}

/// Logical bounds use the global CoreGraphics coordinate space, which can have
/// negative origins. Pixel dimensions describe the selected display mode.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct DisplayGeometry {
    pub display_id: u32,
    pub origin_x: f64,
    pub origin_y: f64,
    pub logical_width: f64,
    pub logical_height: f64,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// Handle the private display-owner process before initializing UI, logging or
/// querying any display. When this returns `Some`, the executable must exit
/// after reporting the result; it must not start the application or server.
pub fn run_display_helper_if_requested() -> Option<Result<()>> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARGUMENT)) {
        return None;
    }
    if arguments.next().is_some() {
        return Some(Err(anyhow::anyhow!(
            "Display helper does not accept extra arguments"
        )));
    }
    #[cfg(target_os = "macos")]
    {
        Some(helper::run_child())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(Err(anyhow::anyhow!("Display helper requires macOS")))
    }
}

const MIN_VIRTUAL_WIDTH: u16 = 320;
const MIN_VIRTUAL_HEIGHT: u16 = 200;
const MAX_VIRTUAL_WIDTH: u16 = 3840;
const MAX_VIRTUAL_HEIGHT: u16 = 2160;
// Includes the native mode-settle budget, plus time for one queued operation.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

fn normalize_size(backend: DisplayBackend, width: u16, height: u16) -> (u16, u16) {
    if backend == DisplayBackend::Main {
        return (width, height);
    }
    let width = width.clamp(MIN_VIRTUAL_WIDTH, MAX_VIRTUAL_WIDTH);
    let height = height.clamp(MIN_VIRTUAL_HEIGHT, MAX_VIRTUAL_HEIGHT);
    // The native virtual mode rounds dimensions down to eight-pixel units.
    // Negotiate that representable size before configuring capture and input.
    (width & !7, height & !7)
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Command {
    Shutdown,
    Geometry(mpsc::Sender<Result<DisplayGeometry>>),
    PrepareCapture {
        width: u16,
        height: u16,
        reply: mpsc::Sender<Result<DisplayGeometry>>,
    },
}

#[derive(Debug)]
struct ControllerInner {
    backend: DisplayBackend,
    commands: mpsc::SyncSender<Command>,
    stopping: Arc<AtomicBool>,
    stopped: Mutex<mpsc::Receiver<()>>,
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // A full queue already wakes the worker, which checks stopping before
        // processing another request. This never blocks waiting for capacity.
        let _ = self.commands.try_send(Command::Shutdown);
        let stopped = match self.stopped.get_mut() {
            Ok(receiver) => receiver,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(error) = stopped.recv_timeout(SHUTDOWN_TIMEOUT) {
            tracing::warn!(%error, backend = ?self.backend,
                "Display worker did not confirm release within 5 seconds");
        }
    }
}

/// Cheap, thread-safe handle. Only values and commands cross the channel;
/// Virtual-display objects stay in one helper process; main-display queries stay
/// on their worker. Dropping the final handle shuts down and reaps the helper.
#[derive(Clone, Debug)]
pub struct DisplayController(Arc<ControllerInner>);

impl DisplayController {
    pub fn new(backend: DisplayBackend, width: u16, height: u16) -> Result<Self> {
        anyhow::ensure!(
            !std::env::args_os().any(|arg| arg == HELPER_ARGUMENT),
            "Display helper entry hook was not called; call run_display_helper_if_requested before application initialization"
        );
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (backend, width, height);
            anyhow::bail!("Display control requires macOS");
        }
        #[cfg(target_os = "macos")]
        {
            let (width, height) = normalize_size(backend, width, height);
            let (commands, receiver) = mpsc::sync_channel(8);
            let (ready_tx, ready_rx) = mpsc::channel();
            let stopping = Arc::new(AtomicBool::new(false));
            let worker_stopping = Arc::clone(&stopping);
            let (stopped_tx, stopped_rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("macrdp-display".to_owned())
                .spawn(move || {
                    macos::run_worker(
                        backend,
                        width,
                        height,
                        receiver,
                        ready_tx,
                        worker_stopping,
                        stopped_tx,
                    )
                })
                .context("Could not start display controller worker")?;
            // On timeout/error, dropping the sender closes the worker's input;
            // it releases any display it created as soon as native calls return.
            ready_rx
                .recv_timeout(REQUEST_TIMEOUT)
                .context("Display controller did not initialize within 10 seconds")??;
            Ok(Self(Arc::new(ControllerInner {
                backend,
                commands,
                stopping,
                stopped: Mutex::new(stopped_rx),
            })))
        }
    }

    pub fn backend(&self) -> DisplayBackend {
        self.0.backend
    }

    /// Virtual framebuffers are clamped to 320×200 through 3840×2160 pixels;
    /// Each dimension rounds down to a multiple of eight. Main is unchanged.
    pub fn normalize_size(&self, width: u16, height: u16) -> (u16, u16) {
        normalize_size(self.0.backend, width, height)
    }

    /// Query current bounds of the pinned display, including any layout change.
    pub fn geometry(&self) -> Result<DisplayGeometry> {
        let (reply, receiver) = mpsc::channel();
        self.0
            .commands
            .try_send(Command::Geometry(reply))
            .context("Display controller worker is busy or unavailable")?;
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .context("Timed out querying display geometry")?
    }

    /// Make the virtual mode match the normalized RDP framebuffer dimensions.
    /// Main only queries geometry and never changes physical display modes.
    pub fn prepare_capture(&self, width: u16, height: u16) -> Result<DisplayGeometry> {
        let (width, height) = self.normalize_size(width, height);
        let (reply, receiver) = mpsc::channel();
        self.0
            .commands
            .try_send(Command::PrepareCapture {
                width,
                height,
                reply,
            })
            .context("Display controller worker is busy or unavailable")?;
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .context("Timed out preparing the selected display")?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_config_defaults_to_main_and_rejects_unknown_names() {
        assert_eq!(DisplayBackend::default(), DisplayBackend::Main);
        assert_eq!(
            serde_json::from_str::<DisplayBackend>("\"virtual\"").unwrap(),
            DisplayBackend::Virtual
        );
        assert_eq!(
            serde_json::to_string(&DisplayBackend::Main).unwrap(),
            "\"main\""
        );
        assert!(serde_json::from_str::<DisplayBackend>("\"automatic\"").is_err());
    }

    #[test]
    fn virtual_sizes_are_bounded_in_framebuffer_pixels() {
        assert_eq!(normalize_size(DisplayBackend::Virtual, 0, 0), (320, 200));
        assert_eq!(
            normalize_size(DisplayBackend::Virtual, u16::MAX, u16::MAX),
            (3840, 2160)
        );
        assert_eq!(
            normalize_size(DisplayBackend::Virtual, 2560, 1305),
            (2560, 1304)
        );
    }

    #[test]
    fn virtual_modes_are_normalized_before_negotiation() {
        assert_eq!(
            normalize_size(DisplayBackend::Virtual, 1922, 1082),
            (1920, 1080)
        );
        assert_eq!(
            normalize_size(DisplayBackend::Virtual, 1928, 1088),
            (1928, 1088)
        );
        assert_eq!(
            normalize_size(DisplayBackend::Virtual, 1936, 1096),
            (1936, 1096)
        );
    }

    #[test]
    fn main_size_policy_does_not_change_requested_dimensions() {
        assert_eq!(
            normalize_size(DisplayBackend::Main, 5119, 2879),
            (5119, 2879)
        );
    }

    #[test]
    fn controller_handle_is_send_and_sync_without_native_objects() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DisplayController>();
    }

    #[test]
    fn only_the_last_handle_requests_and_waits_for_worker_release() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let controller = DisplayController(Arc::new(ControllerInner {
            backend: DisplayBackend::Virtual,
            commands,
            stopping: Arc::clone(&stopping),
            stopped: Mutex::new(stopped_rx),
        }));
        let clone = controller.clone();
        drop(controller);
        assert!(!stopping.load(Ordering::Acquire));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let worker = std::thread::spawn(move || {
            assert!(matches!(receiver.recv().unwrap(), Command::Shutdown));
            assert!(stopping.load(Ordering::Acquire));
            // Native worker sends this only after its owned display is dropped.
            stopped_tx.send(()).unwrap();
        });
        drop(clone);
        worker.join().unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_never_substitutes_another_display() {
        for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
            assert!(DisplayController::new(backend, 1920, 1080).is_err());
        }
    }
}
