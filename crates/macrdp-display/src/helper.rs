//! A persistent process owns virtual displays so WindowServer releases them even
//! after explicit mode selection. Native objects never cross process boundaries.

use anyhow::{bail, ensure, Context, Result};
use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoop};
use objc2::rc::autoreleasepool;
use std::io::{BufReader, BufWriter};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::identity::{IdentityLease, IdentityProof};
use crate::macos::NativeDisplay;
use crate::protocol::{read_message, write_message, Reply, Request};
use crate::transition::{self, Details, Phase};
use crate::{normalize_size, DisplayBackend, DisplayGeometry, HELPER_ARGUMENT};

#[path = "helper_retained.rs"]
mod retained;
#[path = "helper_transitions.rs"]
mod transitions;

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
// Allow bounded layout and window restoration before terminating an owner.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
const SESSION_TIMEOUT: Duration = Duration::from_secs(12);
const REAP_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) struct DisplayHelper {
    child: Child,
    input: Option<ChildStdin>,
    replies: mpsc::Receiver<Result<Reply>>,
    display_id: Option<u32>,
    usable: bool,
    parked: bool,
}

impl DisplayHelper {
    pub(crate) fn id(&self) -> u32 {
        self.display_id.expect("initialized helper owns a display")
    }

    fn initialize(width: u16, height: u16, identity_slot: &str) -> Result<Self> {
        ensure!(
            !std::env::args_os().any(|arg| arg == HELPER_ARGUMENT),
            "Display helper entry hook was not called; refusing recursive helper launch"
        );
        let mut helper = Self::spawn()?;
        let geometry = helper.geometry_request(Request::Initialize {
            width,
            height,
            identity_slot: identity_slot.to_owned(),
        })?;
        helper.display_id = Some(geometry.display_id);
        helper.verify_requested_size(&geometry, width, height)?;
        tracing::info!(
            pid = helper.child.id(),
            ?geometry,
            "Persistent display helper ready"
        );
        Ok(helper)
    }

    fn spawn() -> Result<Self> {
        Self::spawn_with_identity(None).map(|(helper, _)| helper)
    }

    fn spawn_with_identity(
        identity: Option<&IdentityLease>,
    ) -> Result<(Self, Option<IdentityProof>)> {
        let executable =
            std::env::current_exe().context("Could not locate display helper executable")?;
        let mut command = Command::new(executable);
        command
            .arg(HELPER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let proof = identity
            .map(|identity| identity.selector_proof(&mut command))
            .transpose()?;
        let mut child = command
            .spawn()
            .context("Could not launch display helper process")?;
        let diagnostic_output = child.stderr.take().expect("piped child stderr");
        let child_pid = child.id();
        if let Err(error) = std::thread::Builder::new()
            .name("macrdp-display-diagnostics".to_owned())
            .spawn(move || transition::forward_stderr(BufReader::new(diagnostic_output), child_pid))
        {
            // No initialization request has been sent, so this child cannot
            // own a display. Do not leave an undrained pipe or orphan helper.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Could not start display diagnostic reader");
        }
        let input = child.stdin.take().expect("piped child stdin");
        let output = child.stdout.take().expect("piped child stdout");
        let (sender, replies) = mpsc::sync_channel(1);
        let reader = std::thread::Builder::new()
            .name("macrdp-display-replies".to_owned())
            .spawn(move || {
                let mut reader = BufReader::new(output);
                loop {
                    let reply = match read_message::<Reply>(&mut reader) {
                        Ok(Some(reply)) => Ok(reply),
                        Ok(None) => Err(anyhow::anyhow!(
                            "Display helper exited or closed its output"
                        )),
                        Err(error) => Err(error),
                    };
                    let failed = reply.is_err();
                    if sender.send(reply).is_err() || failed {
                        break;
                    }
                }
            });
        let helper = Self {
            child,
            input: Some(input),
            replies,
            display_id: None,
            usable: true,
            parked: false,
        };
        reader.context("Could not start display helper reply reader")?;
        Ok((helper, proof))
    }

    pub(crate) fn validate_identity(identity: &IdentityLease) -> Result<()> {
        let (mut selector, proof) = Self::spawn_with_identity(Some(identity))?;
        let request = Request::ValidateIdentity {
            identity: proof.expect("requested identity proof"),
        };
        write_message(
            selector
                .input
                .as_mut()
                .context("Selector input is closed")?,
            &request,
        )?;
        match selector.receive_reply(Duration::from_millis(2500), false)? {
            Reply::IdentityAvailable {} => {
                selector.usable = false;
                Ok(())
            }
            Reply::Error { message } => bail!("Display identity preflight: {message}"),
            _ => bail!("Unexpected display identity preflight reply"),
        }
    }

    fn geometry_request(&mut self, request: Request) -> Result<DisplayGeometry> {
        self.geometry_request_with_options(request, REPLY_TIMEOUT, false)
    }

    pub(crate) fn select_mode(
        display_id: u32,
        identity: &IdentityLease,
        width: u16,
        height: u16,
    ) -> Result<DisplayGeometry> {
        // This intentionally launches a one-shot selector from the owner helper;
        // it never creates another display owner or recursively launches itself.
        let (mut selector, proof) = Self::spawn_with_identity(Some(identity))?;
        selector.display_id = Some(display_id);
        let geometry = selector.geometry_request_with_options(
            Request::SelectMode {
                display_id,
                identity: proof.expect("requested identity proof"),
                width,
                height,
            },
            Duration::from_millis(2500),
            true,
        )?;
        selector.verify_requested_size(&geometry, width, height)?;
        selector.usable = false; // The one-shot process exits after its reply.
        Ok(geometry)
    }

    pub(crate) fn configure_layout(
        display_id: u32,
        identity: &IdentityLease,
        primary: u32,
        displays: Vec<crate::layout::Placement>,
    ) -> Result<DisplayGeometry> {
        let (mut selector, proof) = Self::spawn_with_identity(Some(identity))?;
        selector.display_id = Some(display_id);
        let geometry = selector.geometry_request_with_options(
            Request::ConfigureLayout {
                display_id,
                identity: proof.expect("requested identity proof"),
                primary,
                displays,
            },
            Duration::from_millis(2500),
            true,
        )?;
        selector.usable = false;
        Ok(geometry)
    }

    fn receive_reply(&self, timeout: Duration, service_owner_events: bool) -> Result<Reply> {
        if !service_owner_events {
            return self
                .replies
                .recv_timeout(timeout)
                .context("Display helper did not reply within its operation deadline")?;
        }
        let deadline = Instant::now() + timeout;
        loop {
            match self.replies.try_recv() {
                Ok(reply) => return reply,
                Err(mpsc::TryRecvError::Disconnected) => bail!("Mode selector closed its output"),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            ensure!(
                Instant::now() < deadline,
                "Mode selector did not reply within 2.5 seconds"
            );
            // The owner must service reconfiguration while the other process
            // commits its synchronous display configuration transaction.
            CFRunLoop::run_in_mode(
                unsafe { kCFRunLoopDefaultMode },
                Duration::from_millis(10),
                false,
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn geometry_request_with_options(
        &mut self,
        request: Request,
        timeout: Duration,
        service_owner_events: bool,
    ) -> Result<DisplayGeometry> {
        ensure!(self.usable, "Display helper is no longer available");
        let result = (|| {
            write_message(
                self.input
                    .as_mut()
                    .context("Display helper input is closed")?,
                &request,
            )?;
            let reply = self.receive_reply(timeout, service_owner_events)?;
            let geometry = match reply {
                Reply::Geometry { geometry, windows } => {
                    log_window_status(windows);
                    geometry
                }
                Reply::Error { message } => bail!("Display helper: {message}"),
                Reply::IdentityAvailable {} => bail!("Unexpected identity preflight reply"),
                Reply::Stopped {} => bail!("Display helper stopped during a geometry request"),
                Reply::DisplayEnabled { .. } | Reply::Parked { .. } | Reply::ParkRefused { .. } => {
                    bail!("Unexpected display state acknowledgment during a geometry request")
                }
            };
            ensure!(
                geometry.display_id != 0
                    && self.display_id.map_or(true, |id| id == geometry.display_id)
                    && geometry.origin_x.is_finite()
                    && geometry.origin_y.is_finite()
                    && geometry.logical_width.is_finite()
                    && geometry.logical_width > 0.0
                    && geometry.logical_height.is_finite()
                    && geometry.logical_height > 0.0
                    && geometry.pixel_width > 0
                    && geometry.pixel_height > 0,
                "Display helper returned invalid geometry: {geometry:?}"
            );
            Ok(geometry)
        })();
        if result.is_err() {
            // Never reuse an out-of-sync stream or consume a late response as
            // the answer to another request. Process death releases the display.
            self.abort();
        }
        result
    }

    pub(crate) fn geometry(&mut self) -> Result<DisplayGeometry> {
        ensure!(
            !self.parked,
            "The retained virtual display is disabled while disconnected"
        );
        self.geometry_request(Request::Geometry {})
    }

    fn begin_remote_session_inner(&mut self) -> Result<DisplayGeometry> {
        let geometry = self.geometry_request_with_options(
            Request::BeginRemoteSession {},
            SESSION_TIMEOUT,
            false,
        )?;
        self.parked = false;
        tracing::info!(
            display_id = self.id(),
            ?geometry,
            "Remote virtual display is primary for this connection"
        );
        Ok(geometry)
    }

    fn end_remote_session_inner(&mut self) -> Result<DisplayGeometry> {
        let geometry = self.geometry_request_with_options(
            Request::EndRemoteSession {},
            SESSION_TIMEOUT,
            false,
        )?;
        tracing::info!(
            display_id = self.id(),
            "Restored the local primary display after the remote connection"
        );
        Ok(geometry)
    }

    fn prepare_capture_inner(&mut self, width: u16, height: u16) -> Result<DisplayGeometry> {
        ensure!(
            !self.parked,
            "Cannot capture a disabled retained virtual display"
        );
        let geometry = self.geometry_request_with_options(
            Request::PrepareCapture { width, height },
            SESSION_TIMEOUT,
            false,
        )?;
        if let Err(error) = self.verify_requested_size(&geometry, width, height) {
            self.abort();
            return Err(error);
        }
        tracing::info!(
            pid = self.child.id(),
            ?geometry,
            "Virtual display mode ready for capture"
        );
        Ok(geometry)
    }

    fn verify_requested_size(
        &self,
        geometry: &DisplayGeometry,
        width: u16,
        height: u16,
    ) -> Result<()> {
        ensure!(
            geometry.pixel_width == u32::from(width)
                && geometry.pixel_height == u32::from(height)
                && geometry.logical_width == f64::from(width)
                && geometry.logical_height == f64::from(height),
            "Display helper returned a different size from {width}x{height}: {geometry:?}"
        );
        Ok(())
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => return false,
                Err(error) => {
                    tracing::warn!(%error, pid = self.child.id(), "Could not reap display helper");
                    return false;
                }
            }
        }
    }

    fn abort(&mut self) {
        self.usable = false;
        self.input.take();
        // Closing input gives a healthy owner an EOF and a chance to restore
        // its saved layout before forced termination is necessary.
        if self.wait_for_exit(SHUTDOWN_GRACE) {
            return;
        }
        if let Err(error) = self.child.kill() {
            tracing::warn!(%error, pid = self.child.id(), "Could not terminate display helper");
        }
        if !self.wait_for_exit(REAP_TIMEOUT) {
            tracing::warn!(
                pid = self.child.id(),
                "Display helper did not exit after termination"
            );
        }
    }
}

fn log_window_status(windows: crate::protocol::WindowStatus) {
    for report in windows.reports {
        tracing::info!(?report, "Remote window session phase completed");
    }
    for warning in windows.warnings {
        tracing::warn!(%warning, "Remote window session warning");
    }
}

impl Drop for DisplayHelper {
    fn drop(&mut self) {
        if self.usable {
            if let Some(input) = &mut self.input {
                if write_message(input, &Request::Shutdown {}).is_ok() {
                    let _ = self.replies.recv_timeout(SHUTDOWN_GRACE);
                }
            }
        }
        self.input.take();
        if !self.wait_for_exit(SHUTDOWN_GRACE) {
            self.abort();
        }
    }
}

fn validate_size(width: u16, height: u16) -> Result<()> {
    ensure!(
        normalize_size(DisplayBackend::Virtual, width, height) == (width, height),
        "Display helper dimensions must be normalized and within supported bounds"
    );
    Ok(())
}

/// Called directly from the executable's main thread, before any CG query.
pub(crate) fn run_child() -> Result<()> {
    transition::install_helper_output();
    // EOF cannot wake a main thread blocked inside an Apple API. A separate
    // thread bounds orphan ownership even if both server and UI processes die.
    crate::parent_watchdog::start()?;
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut output = BufWriter::new(std::io::stdout().lock());
    let mut owner: Option<NativeDisplay> = None;
    // Retain inherited ownership beyond the native call and reply. IdentityLease
    // also pins a duplicate until process exit for WindowServer retirement.
    let mut selector_identity: Option<IdentityLease> = None;
    let mut acknowledge_shutdown = false;
    let outcome = loop {
        let request = match read_message::<Request>(&mut input) {
            Ok(Some(request)) => request,
            Ok(None) => break Ok(()), // Parent death/closed input ends the owner process.
            Err(error) => break Err(error),
        };
        if matches!(request, Request::Shutdown {}) {
            acknowledge_shutdown = true;
            break Ok(());
        }
        let one_shot = matches!(
            request,
            Request::ValidateIdentity { .. }
                | Request::SelectMode { .. }
                | Request::ConfigureLayout { .. }
                | Request::SetDisplayEnabled { .. }
        );
        if let Request::ValidateIdentity { identity } = request {
            let response = (|| {
                ensure!(
                    owner.is_none() && selector_identity.is_none(),
                    "Identity preflight requires a fresh selector process"
                );
                selector_identity = Some(IdentityLease::inherit(&identity)?);
                crate::selector::validate_available(
                    selector_identity.as_ref().expect("inherited identity"),
                )
            })();
            match response {
                Ok(()) => write_message(&mut output, &Reply::IdentityAvailable {})?,
                Err(error) => {
                    let _ = write_message(
                        &mut output,
                        &Reply::Error {
                            message: format!("{error:#}"),
                        },
                    );
                    break Err(error);
                }
            }
            break Ok(());
        }
        if let Request::SetDisplayEnabled {
            display_id,
            identity,
            enabled,
        } = request
        {
            let response = (|| {
                ensure!(
                    owner.is_none() && selector_identity.is_none(),
                    "Display activation requires a fresh selector process"
                );
                selector_identity = Some(IdentityLease::inherit(&identity)?);
                crate::selector::set_display_enabled(
                    display_id,
                    selector_identity.as_ref().expect("inherited identity"),
                    enabled,
                )
            })();
            match response {
                Ok(()) => write_message(
                    &mut output,
                    &Reply::DisplayEnabled {
                        display_id,
                        enabled,
                    },
                )?,
                Err(error) => {
                    let _ = write_message(
                        &mut output,
                        &Reply::Error {
                            message: format!("{error:#}"),
                        },
                    );
                    break Err(error);
                }
            }
            break Ok(());
        }
        if matches!(request, Request::ParkRemoteSession {}) {
            let response = autoreleasepool(|_| {
                owner
                    .as_mut()
                    .context("Display helper is not initialized")?
                    .park_remote_session()
            });
            match response {
                Ok(()) => {
                    let native = owner.as_mut().expect("parked owner");
                    write_message(
                        &mut output,
                        &Reply::Parked {
                            display_id: native.id(),
                            windows: native.take_window_status(),
                        },
                    )?;
                }
                Err(error) => {
                    if let Some(native) = owner.as_mut() {
                        if native.can_retire_after_park_refusal() {
                            write_message(
                                &mut output,
                                &Reply::ParkRefused {
                                    display_id: native.id(),
                                    message: format!("{error:#}"),
                                    windows: native.take_window_status(),
                                },
                            )?;
                            continue;
                        }
                    }
                    let _ = write_message(
                        &mut output,
                        &Reply::Error {
                            message: format!("{error:#}"),
                        },
                    );
                    break Err(error);
                }
            }
            continue;
        }
        let response = autoreleasepool(|_| -> Result<DisplayGeometry> {
            match request {
                Request::Initialize {
                    width,
                    height,
                    identity_slot,
                } => {
                    ensure!(owner.is_none(), "Display helper was already initialized");
                    validate_size(width, height)?;
                    owner = Some(NativeDisplay::new_virtual(width, height, &identity_slot)?);
                    owner.as_ref().expect("initialized owner").geometry()
                }
                Request::Geometry {} => owner
                    .as_ref()
                    .context("Display helper is not initialized")?
                    .geometry(),
                Request::BeginRemoteSession {} => owner
                    .as_mut()
                    .context("Display helper is not initialized")?
                    .begin_remote_session(),
                Request::EndRemoteSession {} => owner
                    .as_mut()
                    .context("Display helper is not initialized")?
                    .end_remote_session(),
                Request::PrepareCapture { width, height } => {
                    validate_size(width, height)?;
                    owner
                        .as_mut()
                        .context("Display helper is not initialized")?
                        .prepare_capture(width, height)
                }
                Request::SelectMode {
                    display_id,
                    identity,
                    width,
                    height,
                } => {
                    ensure!(
                        owner.is_none(),
                        "Mode selection requires a fresh selector process"
                    );
                    validate_size(width, height)?;
                    ensure!(
                        selector_identity.is_none(),
                        "Selector identity was already initialized"
                    );
                    selector_identity = Some(IdentityLease::inherit(&identity)?);
                    crate::selector::select_mode(
                        display_id,
                        selector_identity.as_ref().expect("inherited identity"),
                        width,
                        height,
                    )
                }
                Request::ConfigureLayout {
                    display_id,
                    identity,
                    primary,
                    displays,
                } => {
                    ensure!(
                        owner.is_none(),
                        "Layout configuration requires a fresh selector process"
                    );
                    ensure!(
                        selector_identity.is_none(),
                        "Selector identity was already initialized"
                    );
                    selector_identity = Some(IdentityLease::inherit(&identity)?);
                    crate::selector::configure_layout(
                        display_id,
                        selector_identity.as_ref().expect("inherited identity"),
                        primary,
                        &displays,
                    )
                }
                Request::ValidateIdentity { .. }
                | Request::Shutdown {}
                | Request::ParkRemoteSession {}
                | Request::SetDisplayEnabled { .. } => unreachable!(),
            }
        });
        match response {
            Ok(geometry) => {
                let windows = owner
                    .as_mut()
                    .map(NativeDisplay::take_window_status)
                    .unwrap_or_default();
                if let Err(error) =
                    write_message(&mut output, &Reply::Geometry { geometry, windows })
                {
                    break Err(error);
                }
                if one_shot {
                    break Ok(());
                }
            }
            Err(error) => {
                let _ = write_message(
                    &mut output,
                    &Reply::Error {
                        message: format!("{error:#}"),
                    },
                );
                break Err(error);
            }
        }
    };
    if let Some(owner) = owner {
        let display_id = owner.id();
        let _ = transition::native(Phase::OwnerDrop, Details::display(display_id), || {
            autoreleasepool(|_| drop(owner));
            Ok(())
        });
    }
    if acknowledge_shutdown {
        write_message(&mut output, &Reply::Stopped {})?;
    }
    // Returning is not sufficient: entry points must exit immediately after this
    // hook, because WindowServer may retain a mode-switched display until exit.
    outcome
}
