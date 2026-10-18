//! Capture/session ownership lives here, outside the application and watchdog.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use super::protocol::{self, Command, Event, Initialization, Snapshot};
use crate::{ConnectionEvent, ServerEventHandler};

#[derive(Clone)]
struct Events(mpsc::SyncSender<Event>);

impl ServerEventHandler for Events {
    fn on_connection(&self, value: ConnectionEvent) {
        let _ = self.0.try_send(Event::Connection { value });
    }
}

struct DiagnosticLayer {
    events: mpsc::SyncSender<Event>,
    queued: Arc<AtomicUsize>,
}
impl<S: tracing::Subscriber> Layer<S> for DiagnosticLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if let Ok(bytes) = crate::log_bridge::serialize_event(event) {
            if bytes.len() <= 32 * 1024 {
                if let Ok(record) = String::from_utf8(bytes) {
                    // Leave most pipe capacity for health, settings replies and
                    // terminal events even when debug logging is very busy.
                    if self
                        .queued
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                            (count < 64).then_some(count + 1)
                        })
                        .is_ok()
                    {
                        if self.events.try_send(Event::Diagnostic { record }).is_err() {
                            self.queued.fetch_sub(1, Ordering::AcqRel);
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn run() -> Result<()> {
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
    let writer = std::thread::Builder::new()
        .name("server-worker-output".into())
        .spawn(move || {
            let mut output = BufWriter::new(std::io::stdout());
            while let Ok(event) = events.recv() {
                if matches!(&event, Event::Diagnostic { .. }) {
                    pending_diagnostics.fetch_sub(1, Ordering::AcqRel);
                }
                let terminal = matches!(&event, Event::Stopped | Event::Failed { .. });
                if protocol::write(&event, &mut output).is_err() {
                    break;
                }
                if terminal {
                    written.store(true, Ordering::Release);
                }
            }
        })?;
    let level = initialization.config.log_level.as_deref().unwrap_or("info");
    let filter = tracing_subscriber::EnvFilter::try_new(level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let (filter, reload_filter) = tracing_subscriber::reload::Layer::new(filter);
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(DiagnosticLayer {
            events: sender.clone(),
            queued: diagnostic_count,
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
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    parent_gone: Arc<AtomicBool>,
    reload_filter: tracing_subscriber::reload::Handle<
        tracing_subscriber::EnvFilter,
        tracing_subscriber::Registry,
    >,
) -> Result<()> {
    let callback = Events(events.clone());
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
                        crate::ConfigUpdate::LogLevel(level) => Some(tracing_subscriber::EnvFilter::try_new(level).map_err(|_| "Invalid log level".to_owned())),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_saturation_reserves_capacity_for_health_and_control_replies() {
        let (events, received) = mpsc::sync_channel(256);
        let queued = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(DiagnosticLayer {
            events: events.clone(),
            queued: queued.clone(),
        });
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..1_000 {
                tracing::info!("Diagnostic saturation fixture");
            }
        });
        assert_eq!(queued.load(Ordering::Acquire), 64);
        assert!(events
            .try_send(Event::Snapshot {
                snapshot: Box::default()
            })
            .is_ok());
        for request in 0..191 {
            assert!(events
                .try_send(Event::Updated {
                    request,
                    result: Ok(())
                })
                .is_ok());
        }
        let mut diagnostics = 0;
        let mut health = 0;
        for event in received.try_iter() {
            match event {
                Event::Diagnostic { .. } => diagnostics += 1,
                Event::Snapshot { .. } => health += 1,
                _ => {}
            }
        }
        assert_eq!(diagnostics, 64);
        assert_eq!(health, 1);
    }
}
