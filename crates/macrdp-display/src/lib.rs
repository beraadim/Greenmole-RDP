//! Selected-display ownership and geometry for remote desktop capture.
//!
//! Virtual displays are extended displays in the existing macOS login session.
//! A controller creates its virtual display when a remote connection activates,
//! makes it primary, and restores the previous layout before removing it on
//! disconnect. Resizing reuses the active connection's display.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[cfg(target_os = "macos")]
mod helper;
#[cfg(any(target_os = "macos", test))]
mod layout;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod manual_windows;
#[cfg(any(target_os = "macos", test))]
mod protocol;
mod resolution;
#[cfg(target_os = "macos")]
mod selector;
#[cfg(target_os = "macos")]
mod virtual_display;
mod window_move;
pub use resolution::{resolve_desktop_size, resolve_virtual_desktop_size};
pub use window_move::{WindowMovePreparation, WindowMoveReport};
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
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

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
    BeginRemoteSession(mpsc::Sender<Result<DisplayGeometry>>),
    EndRemoteSession(mpsc::Sender<Result<()>>),
    MoveWindows(window_move::Request),
    PrepareRemoteCapture {
        width: u16,
        height: u16,
        reply: mpsc::Sender<Result<DisplayGeometry>>,
    },
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
    shutdown_complete: AtomicBool,
    stopped: Mutex<mpsc::Receiver<()>>,
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        if self.shutdown_complete.load(Ordering::Acquire) {
            return;
        }
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
                "Display worker did not confirm release within 10 seconds");
        }
    }
}

/// Cheap, thread-safe handle. Only values and commands cross the channel;
/// Virtual-display objects stay in one helper process per active connection;
/// main-display queries stay on their worker. No helper or virtual screen exists
/// while disconnected. Dropping the final handle also shuts down a live helper.
#[derive(Clone, Debug)]
pub struct DisplayController(Arc<ControllerInner>);

/// A request already ordered against disconnect/shutdown on the display worker.
/// Waiting belongs on a blocking task; dropping this value never reorders it.
pub struct DisplayPreparation(mpsc::Receiver<Result<DisplayGeometry>>);

impl DisplayPreparation {
    pub fn wait(self) -> Result<DisplayGeometry> {
        self.0
            .recv_timeout(REQUEST_TIMEOUT)
            .context("Timed out preparing the remote desktop display")?
    }
}

impl DisplayController {
    pub fn new(backend: DisplayBackend, width: u16, height: u16) -> Result<Self> {
        Self::new_with_owner(backend, width, height, ())
    }

    /// Keep the caller's native ownership token on the display worker, including
    /// initialization after a requester times out and final native destruction.
    pub fn new_with_owner(
        backend: DisplayBackend,
        width: u16,
        height: u16,
        owner: impl Send + 'static,
    ) -> Result<Self> {
        anyhow::ensure!(
            !std::env::args_os().any(|arg| arg == HELPER_ARGUMENT),
            "Display helper entry hook was not called; call run_display_helper_if_requested before application initialization"
        );
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (backend, width, height, owner);
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
                    let _owner = owner;
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
                shutdown_complete: AtomicBool::new(false),
                stopped: Mutex::new(stopped_rx),
            })))
        }
    }

    pub fn backend(&self) -> DisplayBackend {
        self.0.backend
    }

    pub fn shutdown_complete(&self) -> bool {
        self.0.shutdown_complete.load(Ordering::Acquire)
    }

    /// Retire the controller after capture and restoration. Only a dedicated
    /// cleanup owner may block here; its lease survives any native shutdown stall.
    /// Successful acknowledgment also makes the last handle's Drop nonblocking.
    pub fn shutdown_owned(&self) -> Result<()> {
        let stopped = self.0.stopped.lock().unwrap();
        if self.shutdown_complete() {
            return Ok(());
        }
        self.0.stopping.store(true, Ordering::Release);
        let _ = self.0.commands.try_send(Command::Shutdown);
        let result = stopped
            .recv()
            .context("Display worker exited without shutdown acknowledgment");
        // Even a disconnected reply means the worker has exited; retain the
        // error for diagnostics, but do not block Drop on an exhausted receiver.
        self.0.shutdown_complete.store(true, Ordering::Release);
        result
    }

    /// Queue a single, user-requested relocation of external application windows.
    /// This never creates a display or enables automatic window management.
    pub fn queue_move_windows_to_remote(&self) -> Result<WindowMovePreparation> {
        self.queue_move_windows_to_remote_with(|_| Ok(0))
    }

    /// Also move this application's own windows through an AppKit-safe callback.
    /// The display worker holds the active session until the callback and AX
    /// work finish. The callback must finish within one second, and cancel any
    /// scheduled main-thread work before returning an error or timing out.
    /// Never block the application main thread waiting for this operation.
    pub fn queue_move_windows_to_remote_with(
        &self,
        own_windows: impl FnOnce(DisplayGeometry) -> Result<usize> + Send + 'static,
    ) -> Result<WindowMovePreparation> {
        anyhow::ensure!(
            self.backend() == DisplayBackend::Virtual,
            "Moving windows requires a remote virtual display"
        );
        let (request, preparation) = window_move::Request::new(own_windows);
        self.0
            .commands
            .try_send(Command::MoveWindows(request))
            .map_err(|error| {
                anyhow::anyhow!("Display controller worker is busy or unavailable: {error}")
            })?;
        Ok(preparation)
    }

    /// Virtual framebuffers are clamped to 320×200 through 3840×2160 pixels;
    /// Each dimension rounds down to a multiple of eight. Main is unchanged.
    pub fn normalize_size(&self, width: u16, height: u16) -> (u16, u16) {
        normalize_size(self.0.backend, width, height)
    }

    /// Query current bounds of the selected display, including layout changes.
    /// An idle virtual controller returns an error; no capture screen exists.
    pub fn geometry(&self) -> Result<DisplayGeometry> {
        let (reply, receiver) = mpsc::channel();
        self.0
            .commands
            .try_send(Command::Geometry(reply))
            .map_err(|error| {
                anyhow::anyhow!("Display controller worker is busy or unavailable: {error}")
            })?;
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .context("Timed out querying display geometry")?
    }

    /// Temporarily make an owned virtual display primary for the connected
    /// remote desktop. Call before capture and input geometry are committed.
    /// Repeated calls preserve the original layout until end_remote_session.
    pub fn begin_remote_session(&self) -> Result<DisplayGeometry> {
        let (reply, receiver) = mpsc::channel();
        self.0
            .commands
            .try_send(Command::BeginRemoteSession(reply))
            .map_err(|error| {
                anyhow::anyhow!("Display controller worker is busy or unavailable: {error}")
            })?;
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .context("Timed out beginning the remote display session")?
    }

    /// Restore the physical primary and display origins, then remove and reap
    /// the session's virtual display. Main-display capture is unchanged.
    pub fn end_remote_session(&self) -> Result<()> {
        let (reply, receiver) = mpsc::channel();
        self.0
            .commands
            .try_send(Command::EndRemoteSession(reply))
            .map_err(|error| {
                anyhow::anyhow!("Display controller worker is busy or unavailable: {error}")
            })?;
        receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .context("Timed out restoring the physical display layout")?
    }

    /// Shutdown barrier for a dedicated native cleanup owner. Unlike the
    /// ordinary request timeout, this returns only when the display worker has
    /// actually completed restoration and virtual-display removal. Never call
    /// from a UI or async executor thread; the owner must retain its lease while
    /// waiting, including when the command queue is full or native work stalls.
    pub fn end_remote_session_owned(&self) -> Result<()> {
        let (reply, receiver) = mpsc::channel();
        self.0
            .commands
            .send(Command::EndRemoteSession(reply))
            .map_err(|_| anyhow::anyhow!("Display controller worker unavailable during cleanup"))?;
        receiver
            .recv()
            .context("Display restoration worker exited without acknowledgment")?
    }

    /// Queue disconnect restoration without blocking the input callback. The
    /// worker processes this before a subsequent begin_remote_session request.
    pub fn end_remote_session_async(&self) -> Result<()> {
        let (reply, _receiver) = mpsc::channel();
        self.0
            .commands
            .try_send(Command::EndRemoteSession(reply))
            .map_err(|error| {
                anyhow::anyhow!("Display controller worker is busy or unavailable: {error}")
            })
    }

    /// Queue session activation and mode preparation before yielding an async
    /// caller. A concurrent cancellation therefore queues restoration after
    /// this work, even if the blocking waiter has not started yet.
    pub fn queue_remote_capture(&self, width: u16, height: u16) -> Result<DisplayPreparation> {
        let (width, height) = self.normalize_size(width, height);
        let (reply, receiver) = mpsc::channel();
        self.0
            .commands
            .try_send(Command::PrepareRemoteCapture {
                width,
                height,
                reply,
            })
            .map_err(|error| {
                anyhow::anyhow!("Display controller worker is busy or unavailable: {error}")
            })?;
        Ok(DisplayPreparation(receiver))
    }

    /// Make an active virtual mode match the normalized RDP framebuffer dimensions.
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
            .map_err(|error| {
                anyhow::anyhow!("Display controller worker is busy or unavailable: {error}")
            })?;
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
    fn capture_disconnect_and_reconnect_are_queued_in_order_even_if_waiter_is_dropped() {
        let (commands, receiver) = mpsc::sync_channel(8);
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let controller = DisplayController(Arc::new(ControllerInner {
            backend: DisplayBackend::Virtual,
            commands,
            stopping: Arc::new(AtomicBool::new(false)),
            shutdown_complete: AtomicBool::new(false),
            stopped: Mutex::new(stopped_rx),
        }));
        let preparation = controller.queue_remote_capture(1921, 1081).unwrap();
        // Async cancellation may discard the waiter before its blocking task
        // starts. The activation request is nevertheless already on the queue.
        drop(preparation);
        controller.end_remote_session_async().unwrap();
        drop(controller.queue_remote_capture(1280, 720).unwrap());
        assert!(matches!(
            receiver.recv().unwrap(),
            Command::PrepareRemoteCapture {
                width: 1920,
                height: 1080,
                ..
            }
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            Command::EndRemoteSession(_)
        ));
        assert!(matches!(
            receiver.recv().unwrap(),
            Command::PrepareRemoteCapture {
                width: 1280,
                height: 720,
                ..
            }
        ));
        stopped_tx.send(()).unwrap();
        drop(controller);
    }

    #[test]
    fn owned_restoration_waits_for_queue_and_actual_worker_acknowledgment() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let controller = DisplayController(Arc::new(ControllerInner {
            backend: DisplayBackend::Virtual,
            commands,
            stopping: Arc::new(AtomicBool::new(false)),
            shutdown_complete: AtomicBool::new(false),
            stopped: Mutex::new(stopped_rx),
        }));
        // A queued operation already fills the bounded command channel.
        drop(controller.queue_remote_capture(1280, 720).unwrap());
        let cleanup_owner = controller.clone();
        let (done, result) = mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            done.send(cleanup_owner.end_remote_session_owned()).unwrap();
        });
        assert!(result.recv_timeout(Duration::from_millis(10)).is_err());
        assert!(matches!(
            receiver.recv().unwrap(),
            Command::PrepareRemoteCapture { .. }
        ));
        let Command::EndRemoteSession(reply) = receiver.recv().unwrap() else {
            panic!("cleanup must follow queued activation")
        };
        assert!(result.recv_timeout(Duration::from_millis(10)).is_err());
        reply
            .send(Err(anyhow::anyhow!("simulated restoration failure")))
            .unwrap();
        assert!(result
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("simulated restoration failure"));
        cleanup.join().unwrap();
        stopped_tx.send(()).unwrap();
        drop(controller);
    }

    #[test]
    fn owned_shutdown_is_pending_until_native_worker_acknowledges_and_drop_does_not_wait_again() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let controller = DisplayController(Arc::new(ControllerInner {
            backend: DisplayBackend::Virtual,
            commands,
            stopping: Arc::new(AtomicBool::new(false)),
            shutdown_complete: AtomicBool::new(false),
            stopped: Mutex::new(stopped_rx),
        }));
        let retained = controller.clone();
        let (done, completed) = mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            retained.shutdown_owned().unwrap();
            done.send(()).unwrap();
        });
        assert!(matches!(receiver.recv().unwrap(), Command::Shutdown));
        assert!(!controller.shutdown_complete());
        assert!(completed.recv_timeout(Duration::from_millis(10)).is_err());
        stopped_tx.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        cleanup.join().unwrap();
        assert!(controller.shutdown_complete());
        // Keep the sender alive: rereading the consumed acknowledgment would
        // block for ten seconds, rather than immediately see a disconnected pipe.
        let (dropped, drop_done) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(controller);
            dropped.send(()).unwrap();
        });
        drop_done.recv_timeout(Duration::from_secs(2)).unwrap();
        dropper.join().unwrap();
        drop(stopped_tx);
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
            shutdown_complete: AtomicBool::new(false),
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

    #[test]
    fn manual_window_actions_share_disconnect_ordering_and_expire_when_abandoned() {
        let (commands, receiver) = mpsc::sync_channel(8);
        let (stopped_tx, stopped_rx) = mpsc::channel();
        let controller = DisplayController(Arc::new(ControllerInner {
            backend: DisplayBackend::Virtual,
            commands,
            stopping: Arc::new(AtomicBool::new(false)),
            shutdown_complete: AtomicBool::new(false),
            stopped: Mutex::new(stopped_rx),
        }));
        let action = controller.queue_move_windows_to_remote().unwrap();
        controller.end_remote_session_async().unwrap();
        let Command::MoveWindows(request) = receiver.recv().unwrap() else {
            panic!("window action must precede disconnect")
        };
        assert!(request.budget.check().is_ok());
        assert!(matches!(
            receiver.recv().unwrap(),
            Command::EndRemoteSession(_)
        ));
        drop(action);
        assert!(request.budget.check().is_err());
        stopped_tx.send(()).unwrap();
        drop(controller);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_never_substitutes_another_display() {
        for backend in [DisplayBackend::Main, DisplayBackend::Virtual] {
            assert!(DisplayController::new(backend, 1920, 1080).is_err());
        }
    }
}
