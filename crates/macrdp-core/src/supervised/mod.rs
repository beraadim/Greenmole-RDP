//! Process supervision keeps native capture failures outside the application.
//! A failed owner is fully retired, its display removed, then a replacement can
//! listen for a new client. A quiet desktop is never considered a health failure.

mod child;
mod generation;
mod handle;
#[cfg(test)]
mod lifecycle_tests;
mod policy;
mod process;
#[cfg(test)]
#[path = "tests.rs"]
mod process_fault_tests;
mod protocol;
mod runtime;

use crate::ConfigUpdate;
use anyhow::Result;
use macrdp_display::{DisplayGeometry, WindowMoveReport};
use protocol::Snapshot;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;

pub use handle::{
    start_supervised_server, start_supervised_server_with_perf, SupervisedServerHandle,
    SupervisedWindowMovePreparation, SupervisionDiagnostics,
};
const HELPER_ARGUMENT: &str = "--macrdp-server-helper";
const START_TIMEOUT: Duration = Duration::from_secs(20);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(8);
// Do not interrupt a healthy window/layout restoration during Stop.
const STOP_GRACE: Duration = Duration::from_secs(20);
const RESTART_WINDOW: Duration = Duration::from_secs(300);
const MAX_RESTARTS: usize = 2;
static SUPERVISOR_OWNED: AtomicBool = AtomicBool::new(false);

pub fn supervisor_resources_pending() -> bool {
    SUPERVISOR_OWNED.load(Ordering::Acquire)
}

struct SupervisorLease {
    safe_to_release: bool,
}
impl Drop for SupervisorLease {
    fn drop(&mut self) {
        if self.safe_to_release {
            SUPERVISOR_OWNED.store(false, Ordering::Release);
        }
    }
}

struct StartupCancellation {
    shared: Arc<Shared>,
    armed: bool,
}
impl Drop for StartupCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.shared.stopping.store(true, Ordering::Release);
            let _ = self.shared.commands.try_send(ParentCommand::Stop);
        }
    }
}

type OwnWindows = Box<dyn FnOnce(DisplayGeometry) -> Result<usize> + Send>;
struct WindowRequest {
    own_windows: Option<OwnWindows>,
    result: mpsc::SyncSender<Result<WindowMoveReport>>,
    cancelled: Arc<AtomicBool>,
}

enum ParentCommand {
    Update {
        update: ConfigUpdate,
        result: tokio::sync::oneshot::Sender<Result<()>>,
    },
    Move {
        id: u64,
        request: WindowRequest,
    },
    OwnResult {
        id: u64,
        result: std::result::Result<usize, String>,
    },
    Stop,
}

struct Shared {
    snapshot: Mutex<Snapshot>,
    terminal_error: Mutex<Option<String>>,
    finished: AtomicBool,
    stopping: AtomicBool,
    worker_pid: AtomicU32,
    restart_count: AtomicU32,
    recovery_result: Mutex<&'static str>,
    commands: tokio::sync::mpsc::Sender<ParentCommand>,
}

/// Must dispatch before UI initialization. The child never initializes a webview
/// or writes the supervisor's rotating diagnostic file.
pub fn run_server_helper_if_requested() -> Option<Result<()>> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARGUMENT)) {
        return None;
    }
    if arguments.next().is_some() {
        return Some(Err(anyhow::anyhow!("Unexpected server helper arguments")));
    }
    let result = child::run();
    if let Err(error) = &result {
        eprintln!("Server helper stopped: {error:#}");
    }
    // Native work can outlive a runtime. Process exit is the safe ownership
    // boundary; its separate display owner receives EOF and restores layout.
    std::process::exit(if result.is_ok() { 0 } else { 70 });
}
