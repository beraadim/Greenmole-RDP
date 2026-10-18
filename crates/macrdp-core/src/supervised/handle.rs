//! Application-side control and cancellation-safe construction.
use super::runtime::supervise;
use super::{
    protocol::Snapshot, ParentCommand, Shared, StartupCancellation, SupervisorLease, WindowRequest,
    SUPERVISOR_OWNED,
};
use crate::{callbacks::*, config::ServerConfig, tls::TlsCertificateInfo, StopOutcome};
use anyhow::{Context, Result};
use macrdp_display::{DisplayBackend, DisplayGeometry, WindowMoveReport};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

#[derive(Clone, serde::Serialize)]
pub struct SupervisionDiagnostics {
    pub enabled: bool,
    pub phase: &'static str,
    pub worker_pid: Option<u32>,
    pub native_operation: Option<String>,
    pub native_operation_elapsed_ms: u64,
    pub restart_count: u32,
    pub recovery_result: &'static str,
}

/// Application-side handle: every native capture/encoder/display owner lives in
/// a separately supervised process, so Stop never depends on its Rust destructor.
pub struct SupervisedServerHandle {
    shared: Arc<Shared>,
    certificate: TlsCertificateInfo,
    backend: DisplayBackend,
    port: u16,
    next_request: AtomicU64,
}

pub struct SupervisedWindowMovePreparation {
    result: mpsc::Receiver<Result<WindowMoveReport>>,
    cancelled: Arc<AtomicBool>,
}
impl SupervisedWindowMovePreparation {
    pub fn wait(self) -> Result<WindowMoveReport> {
        self.result
            .recv_timeout(Duration::from_secs(9))
            .context("Timed out moving windows to remote display")?
    }
}
impl Drop for SupervisedWindowMovePreparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl SupervisedServerHandle {
    pub fn display_backend(&self) -> DisplayBackend {
        self.backend
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn tls_certificate(&self) -> &TlsCertificateInfo {
        &self.certificate
    }
    pub fn status(&self) -> ServerStatus {
        self.shared.snapshot.lock().unwrap().status.clone()
    }
    pub fn metrics(&self) -> Metrics {
        self.shared.snapshot.lock().unwrap().metrics.clone()
    }
    pub fn bytes_sent(&self) -> u64 {
        self.metrics().bytes_sent
    }
    pub fn input_debug_state(&self) -> InputDebugState {
        self.shared.snapshot.lock().unwrap().input.clone()
    }
    pub fn active_connection(&self) -> Option<ConnectionInfo> {
        self.shared.snapshot.lock().unwrap().connection.clone()
    }
    pub fn native_watchdog_snapshot(&self) -> crate::native_watchdog::Snapshot {
        self.shared.snapshot.lock().unwrap().watchdog.clone()
    }
    pub fn diagnostics(&self) -> SupervisionDiagnostics {
        let snapshot = self.shared.snapshot.lock().unwrap();
        let phase = match snapshot.status.state.as_str() {
            "starting" => "starting",
            "running" => "running",
            "recovering" | "cleanup_pending" => "recovering",
            "stopping" => "stopping",
            "stopped" => "stopped",
            _ => "error",
        };
        let pid = self.shared.worker_pid.load(Ordering::Acquire);
        SupervisionDiagnostics {
            enabled: true,
            phase,
            worker_pid: (pid != 0).then_some(pid),
            native_operation: snapshot.watchdog.operation.clone(),
            native_operation_elapsed_ms: snapshot.watchdog.elapsed_ms,
            restart_count: self.shared.restart_count.load(Ordering::Acquire),
            recovery_result: *self.shared.recovery_result.lock().unwrap(),
        }
    }
    pub fn cleanup_complete(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }
    pub fn window_move_unavailable_reason(&self) -> Option<&'static str> {
        if self.shared.stopping.load(Ordering::Acquire) || !self.status().running {
            Some("Start the server and connect to the remote desktop first.")
        } else if self.backend != DisplayBackend::Virtual {
            Some("Select the virtual display in Settings and restart the server.")
        } else if self.active_connection().is_none() {
            Some("Connect to the remote desktop first.")
        } else {
            None
        }
    }
    pub fn queue_move_windows_to_remote_with(
        &self,
        own_windows: impl FnOnce(DisplayGeometry) -> Result<usize> + Send + 'static,
    ) -> Result<SupervisedWindowMovePreparation> {
        if let Some(reason) = self.window_move_unavailable_reason() {
            anyhow::bail!("{reason}");
        }
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (sender, result) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.shared
            .commands
            .try_send(ParentCommand::Move {
                id,
                request: WindowRequest {
                    own_windows: Some(Box::new(own_windows)),
                    result: sender,
                    cancelled: cancelled.clone(),
                },
            })
            .map_err(|_| anyhow::anyhow!("Server control queue is full or unavailable"))?;
        Ok(SupervisedWindowMovePreparation { result, cancelled })
    }
    pub async fn update_config(&self, update: ConfigUpdate) -> Result<()> {
        update.validate()?;
        anyhow::ensure!(
            !self.shared.stopping.load(Ordering::Acquire),
            "Server is stopping"
        );
        let (result, receiver) = tokio::sync::oneshot::channel();
        self.shared
            .commands
            .try_send(ParentCommand::Update { update, result })
            .map_err(|_| anyhow::anyhow!("Server control queue is full or unavailable"))?;
        tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .context("Server settings acknowledgement timed out")?
            .context("Server stopped before acknowledging settings")?
    }
    pub async fn stop(&self) -> Result<StopOutcome> {
        self.shared.stopping.store(true, Ordering::Release);
        let _ = self.shared.commands.try_send(ParentCommand::Stop);
        // Keep the UI responsive while the supervisor retains cleanup ownership.
        let until = Instant::now() + Duration::from_secs(5);
        while !self.cleanup_complete() && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.cleanup_complete() {
            Ok(StopOutcome::Complete)
        } else {
            Ok(StopOutcome::Pending {
                reason: "Recovering the native worker and returning the remote display to this Mac",
            })
        }
    }
    /// Emergency Stop is owned by the healthy supervisor, not by capture cleanup.
    pub async fn recover_local_desktop(&self) -> Result<()> {
        self.shared.stopping.store(true, Ordering::Release);
        let _ = self.shared.commands.try_send(ParentCommand::Stop);
        tokio::time::timeout(Duration::from_secs(50), async {
            while !self.cleanup_complete() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(())
        })
        .await
        .context("Local desktop recovery is still pending")?
    }
    pub async fn wait_until_stopped(&self) -> Result<()> {
        while !self.cleanup_complete() {
            if let Some(error) = self.shared.terminal_error.lock().unwrap().clone() {
                anyhow::bail!("{error}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        match self.shared.terminal_error.lock().unwrap().clone() {
            Some(error) => Err(anyhow::Error::msg(error)),
            None => Ok(()),
        }
    }
}
impl Drop for SupervisedServerHandle {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        let _ = self.shared.commands.try_send(ParentCommand::Stop);
    }
}

pub async fn start_supervised_server(
    config: ServerConfig,
    handler: impl ServerEventHandler,
) -> Result<Arc<SupervisedServerHandle>> {
    start_supervised_server_with_perf(config, handler, false).await
}

pub async fn start_supervised_server_with_perf(
    config: ServerConfig,
    handler: impl ServerEventHandler,
    perf_enabled: bool,
) -> Result<Arc<SupervisedServerHandle>> {
    config.validate()?;
    anyhow::ensure!(
        SUPERVISOR_OWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        "Previous supervised server recovery is still pending"
    );
    let lease = SupervisorLease {
        safe_to_release: false,
    };
    let backend = config.display_backend;
    let (commands, receiver) = tokio::sync::mpsc::channel(32);
    let shared = Arc::new(Shared {
        snapshot: Mutex::new(Snapshot {
            status: ServerStatus {
                state: "starting".into(),
                ..Default::default()
            },
            ..Default::default()
        }),
        terminal_error: Mutex::new(None),
        finished: AtomicBool::new(false),
        stopping: AtomicBool::new(false),
        worker_pid: AtomicU32::new(0),
        restart_count: AtomicU32::new(0),
        recovery_result: Mutex::new("none"),
        commands,
    });
    let (ready, result) = tokio::sync::oneshot::channel();
    let mut cancellation = StartupCancellation {
        shared: shared.clone(),
        armed: true,
    };
    tokio::spawn(supervise(
        config,
        Arc::new(handler),
        shared.clone(),
        receiver,
        ready,
        lease,
        perf_enabled,
    ));
    let (port, certificate) = result
        .await
        .context("Server supervisor stopped during startup")??;
    cancellation.armed = false;
    Ok(Arc::new(SupervisedServerHandle {
        shared,
        certificate,
        backend,
        port,
        next_request: AtomicU64::new(1),
    }))
}
