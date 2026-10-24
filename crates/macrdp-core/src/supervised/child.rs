//! Capture/session ownership lives here, outside the application and watchdog.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use super::protocol::{self, Command, Event, Initialization, Snapshot};
use crate::{ConnectionEvent, ServerEventHandler};

#[path = "child_diagnostics.rs"]
mod diagnostics;
use diagnostics::{record_loss, write_event_with_diagnostic_loss, DiagnosticLayer};

#[derive(Clone)]
struct Events {
    events: mpsc::SyncSender<Event>,
    dropped: Arc<AtomicU64>,
}

impl ServerEventHandler for Events {
    fn on_connection(&self, value: ConnectionEvent) {
        if self.events.try_send(Event::Connection { value }).is_err() {
            record_loss(&self.dropped);
        }
    }
}

pub(super) fn run() -> Result<()> {
    // This helper is dispatched before AppKit/Tauri startup and never starts a
    // UI event loop. Carbon layout reads may therefore be serialized on workers.
    macrdp_input::host_layout::configure_for_headless_process()?;
    let mut input = BufReader::new(std::io::stdin());
    let mut initialization: Initialization =
        protocol::read(&mut input)?.context("Server worker initialization missing")?;
    initialization.config.managed_tls_directory = initialization.managed_tls_directory.take();
    initialization.config.validate()?;
    let (sender, events) = mpsc::sync_channel(256);
    let terminal_written = Arc::new(AtomicBool::new(false));
    let written = terminal_written.clone();
    let diagnostic_count = Arc::new(AtomicUsize::new(0));
    let pending_diagnostics = diagnostic_count.clone();
    let dropped_diagnostics = Arc::new(AtomicU64::new(0));
    let writer_drops = dropped_diagnostics.clone();
    let writer = std::thread::Builder::new()
        .name("server-worker-output".into())
        .spawn(move || {
            let mut output = BufWriter::new(std::io::stdout());
            let mut reported_drops = 0;
            while let Ok(event) = events.recv() {
                if matches!(&event, Event::Diagnostic { .. }) {
                    pending_diagnostics.fetch_sub(1, Ordering::AcqRel);
                }
                let terminal = matches!(&event, Event::Stopped | Event::Failed { .. });
                if write_event_with_diagnostic_loss(
                    &event,
                    &mut output,
                    &writer_drops,
                    &mut reported_drops,
                )
                .is_err()
                {
                    break;
                }
                if terminal {
                    written.store(true, Ordering::Release);
                }
            }
        })?;
    let level = initialization.config.log_level.as_deref().unwrap_or("info");
    let filter = crate::with_transition_diagnostics(
        tracing_subscriber::EnvFilter::try_new(level)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
    );
    let (filter, reload_filter) = tracing_subscriber::reload::Layer::new(filter);
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(DiagnosticLayer {
            events: sender.clone(),
            queued: diagnostic_count,
            dropped: dropped_diagnostics.clone(),
        })
        .try_init()?;
    let (commands, command_receiver) = tokio::sync::mpsc::channel(32);
    let parent_gone = Arc::new(AtomicBool::new(false));
    let disconnected = parent_gone.clone();
    std::thread::Builder::new()
        .name("server-worker-input".into())
        .spawn(move || {
            loop {
                match protocol::read::<Command>(&mut input) {
                    Ok(Some(command)) => {
                        if commands.try_send(command).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            disconnected.store(true, Ordering::Release);
            drop(commands);
            // A native thread may prevent even a Tokio runtime's destructor from
            // returning. Parent EOF therefore has an independent process deadline.
            // Healthy display owners now also restore saved window geometry.
            std::thread::sleep(Duration::from_secs(20));
            terminate_owned_group_after_parent_exit();
            std::process::exit(70);
        })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(serve(
        initialization,
        sender.clone(),
        dropped_diagnostics,
        command_receiver,
        parent_gone,
        reload_filter,
    ));
    if let Err(error) = &result {
        let _ = sender.send(Event::Failed {
            error: format!("{error:#}"),
        });
    }
    // Never wait for leaked native/blocking tasks in Runtime::drop. The process
    // owns them, and the display helper has its own EOF restoration path.
    runtime.shutdown_timeout(Duration::from_millis(100));
    let flush_deadline = std::time::Instant::now() + Duration::from_millis(500);
    while !terminal_written.load(Ordering::Acquire) && std::time::Instant::now() < flush_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(sender);
    let _ = writer; // stdout delivery is flushed by its owner while we unwind.
    result
}

fn terminate_owned_group_after_parent_exit() {
    #[cfg(unix)]
    {
        let pid = std::process::id() as i32;
        // The supervisor creates a new group whose leader is this worker.
        // Never signal a caller's shell/application group if a private helper
        // was accidentally invoked without that isolation.
        if unsafe { libc::getpgrp() } == pid {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

type OwnReplies = Arc<Mutex<HashMap<u64, mpsc::SyncSender<std::result::Result<usize, String>>>>>;

async fn serve(
    initialization: Initialization,
    events: mpsc::SyncSender<Event>,
    dropped: Arc<AtomicU64>,
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    parent_gone: Arc<AtomicBool>,
    reload_filter: tracing_subscriber::reload::Handle<
        tracing_subscriber::EnvFilter,
        tracing_subscriber::Registry,
    >,
) -> Result<()> {
    let callback = Events {
        events: events.clone(),
        dropped,
    };
    // Keep reading parent EOF while native startup is in flight. The dedicated
    // input thread's process deadline does not depend on this future yielding.
    let perf = initialization
        .perf_enabled
        .then(|| crate::perf_stats::new_shared(true));
    let server =
        crate::server::start_server_with_perf(initialization.config, callback, perf.clone())
            .await?;
    events.send(Event::Ready {
        port: server.port(),
        certificate: server.tls_certificate().clone(),
    })?;
    let baseline = initialization.baseline_display_ids;
    let own_replies: OwnReplies = Arc::new(Mutex::new(HashMap::new()));
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    let mut stopping = false;
    let mut stop_task = None;
    let mut warned_operation = None;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Update { request, update }) => {
                    let new_filter = match &update {
                        crate::ConfigUpdate::LogLevel(level) => Some(tracing_subscriber::EnvFilter::try_new(level).map(crate::with_transition_diagnostics).map_err(|_| "Invalid log level".to_owned())),
                        _ => None,
                    };
                    let result = if stopping { Err("Server is stopping".into()) }
                        else if new_filter.as_ref().is_some_and(|filter| filter.is_err()) { Err("Invalid log level".into()) }
                        else {
                            server.update_config(update).map_err(|error| format!("{error:#}")).and_then(|_| {
                                if let Some(Ok(filter)) = new_filter { reload_filter.reload(filter).map_err(|_| "Cannot reload server log level".to_owned())?; }
                                Ok(())
                            })
                        };
                    let _ = events.send(Event::Updated { request, result });
                }
                Some(Command::Stop) | None => {
                    if !stopping {
                        stopping = true;
                        let owner = server.clone();
                        stop_task = Some(tokio::spawn(async move { owner.stop().await }));
                    }
                }
                Some(Command::OwnWindows { request, result }) => {
                    if let Some(reply) = own_replies.lock().unwrap().remove(&request) { let _ = reply.try_send(result); }
                }
                Some(Command::MoveWindows { request }) if !stopping => {
                    if !own_replies.lock().unwrap().is_empty() {
                        let _ = events.try_send(Event::MoveResult { request, result: Err("A window move is already pending".into()) });
                        continue;
                    }
                    let (reply, receiver) = mpsc::sync_channel(1);
                    own_replies.lock().unwrap().insert(request, reply);
                    let target_events = events.clone();
                    let action = server.queue_move_windows_to_remote_with(move |target| {
                        target_events.try_send(Event::MoveTarget { request, target }).context("Window target could not reach application")?;
                        receiver.recv_timeout(Duration::from_secs(3)).context("Application window move timed out")?
                            .map_err(anyhow::Error::msg)
                    });
                    let completed = events.clone();
                    let pending = own_replies.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = action.and_then(|action| action.wait()).map_err(|error| format!("{error:#}"));
                        pending.lock().unwrap().remove(&request);
                        let _ = completed.try_send(Event::MoveResult { request, result });
                    });
                }
                Some(_) => {}
            },
            _ = interval.tick() => {
                let owned_display_ids = macrdp_display::list_owned_virtual_displays().unwrap_or_default()
                    .into_iter().filter(|id| !baseline.contains(id)).collect();
                let watchdog = crate::native_watchdog::snapshot();
                if watchdog.pending && watchdog.elapsed_ms >= 3_000 && warned_operation != Some(watchdog.operation_id) {
                    warned_operation = Some(watchdog.operation_id);
                    tracing::warn!(worker_pid = watchdog.owner_pid, operation = watchdog.operation.as_deref().unwrap_or("none"), operation_id = watchdog.operation_id, operation_elapsed_ms = watchdog.elapsed_ms, stop_requested = watchdog.stop_requested, cleanup_pending = watchdog.cleanup_pending, "Native operation remains pending; supervisor recovery deadline is active");
                }
                let snapshot = Snapshot { status: server.status(), metrics: server.metrics(), input: server.input_debug_state(), connection: server.active_connection(), watchdog, owned_display_ids };
                let complete = server.cleanup_complete();
                let failed = !stopping && !snapshot.status.running;
                let _ = events.send(Event::Snapshot { snapshot: Box::new(snapshot) });
                if stopping && complete {
                    if let Some(perf) = &perf { events.send(Event::PerformanceSummary { report: perf.lock().unwrap().summary() })?; }
                    events.send(Event::Stopped)?;
                    return Ok(());
                }
                if failed { anyhow::bail!("RDP server stopped unexpectedly"); }
                if parent_gone.load(Ordering::Acquire) && !stopping {
                    stopping = true;
                    let owner = server.clone();
                    stop_task = Some(tokio::spawn(async move { owner.stop().await }));
                }
                if stop_task.as_ref().is_some_and(|task| task.is_finished()) { stop_task.take(); }
            }
        }
    }
}
