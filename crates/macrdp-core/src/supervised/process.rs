//! Owned child/group retirement. Never searches by executable or process name.

use std::collections::HashSet;
use std::io::{BufReader, BufWriter};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::protocol::{self, Command, Event, Initialization};

pub(super) struct Worker {
    pub child: Child,
    pub commands: mpsc::SyncSender<Command>,
    pub events: mpsc::Receiver<Result<Event>>,
    pub baseline: HashSet<u32>,
    pub owned_displays: HashSet<u32>,
    retired: bool,
    inspect_displays: bool,
    private_session: bool,
}

impl Worker {
    pub fn spawn(initialization: Initialization) -> Result<Self> {
        let mut command = ProcessCommand::new(std::env::current_exe()?);
        command
            .arg(super::HELPER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        Self::spawn_command(initialization, command, true)
    }

    #[cfg(test)]
    pub fn spawn_test_worker(
        initialization: Initialization,
        command: ProcessCommand,
    ) -> Result<Self> {
        Self::spawn_command(initialization, command, false)
    }

    fn spawn_command(
        initialization: Initialization,
        mut command: ProcessCommand,
        inspect_displays: bool,
    ) -> Result<Self> {
        let baseline = initialization
            .baseline_display_ids
            .iter()
            .copied()
            .collect();
        let private_session = initialization.config.private_session;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        isolate_group(&mut command);
        let mut child = command
            .spawn()
            .context("Cannot launch isolated server worker")?;
        let mut input = BufWriter::new(child.stdin.take().context("Missing server worker input")?);
        let output = child
            .stdout
            .take()
            .context("Missing server worker output")?;
        let (sender, receiver) = mpsc::sync_channel::<Command>(32);
        let writer = std::thread::Builder::new()
            .name("server-worker-commands".into())
            .spawn(move || {
                if protocol::write(&initialization, &mut input).is_err() {
                    return;
                }
                while let Ok(command) = receiver.recv() {
                    if protocol::write(&command, &mut input).is_err() {
                        break;
                    }
                }
            });
        if let Err(error) = writer {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        let (events, event_receiver) = mpsc::sync_channel(256);
        if let Err(error) = std::thread::Builder::new()
            .name("server-worker-events".into())
            .spawn(move || {
                let mut output = BufReader::new(output);
                loop {
                    let event = protocol::read(&mut output)
                        .and_then(|value| value.context("Server worker closed its event pipe"));
                    let failed = event.is_err();
                    if events.send(event).is_err() || failed {
                        break;
                    }
                }
            })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
        tracing::info!(
            worker_pid = child.id(),
            "Isolated RDP server worker launched"
        );
        Ok(Self {
            child,
            commands: sender,
            events: event_receiver,
            baseline,
            owned_displays: HashSet::new(),
            retired: false,
            inspect_displays,
            private_session,
        })
    }

    pub fn discover_owned_displays(&mut self) -> Result<()> {
        if !self.inspect_displays {
            return Ok(());
        }
        for id in macrdp_display::list_owned_virtual_displays()? {
            if !self.baseline.contains(&id) {
                self.owned_displays.insert(id);
            }
        }
        Ok(())
    }

    /// Stop native work normally first. Killing the server closes the separate
    /// display owner's pipe, giving that owner time to restore the saved layout.
    /// Only this worker's isolated process group is a last-resort kill target.
    #[cfg(test)]
    pub fn retire(self, graceful: Duration) -> Result<()> {
        self.retire_with_handler(graceful, None)
    }

    pub fn retire_with_handler(
        mut self,
        graceful: Duration,
        handler: Option<std::sync::Arc<dyn crate::ServerEventHandler>>,
    ) -> Result<()> {
        let pid = self.child.id();
        let _ = self.commands.try_send(Command::Stop);
        let until = Instant::now() + graceful;
        let mut output_closed = false;
        while Instant::now() < until {
            let _ = self.discover_owned_displays();
            // Keep the worker unreaped until group termination. Its PID cannot
            // be reused for an unrelated group while it remains our child.
            loop {
                match self.events.try_recv() {
                    Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                        output_closed = true;
                        break;
                    }
                    Ok(Ok(Event::Stopped)) => {
                        output_closed = true;
                        break;
                    }
                    Ok(Ok(Event::Diagnostic { record })) => {
                        crate::log_bridge::record_forwarded_record(record.into_bytes());
                    }
                    Ok(Ok(Event::PerformanceSummary { report })) => {
                        if let Some(handler) = &handler {
                            handler.on_performance_summary(report);
                        }
                    }
                    Ok(Ok(_)) => {}
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if output_closed {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let discovery_error = self.discover_owned_displays().err();
        if !output_closed {
            tracing::warn!(
                worker_pid = pid,
                "Native worker did not stop; retiring isolated process"
            );
        }
        // Even an EOF can race final process exit. Killing this exact owned
        // child is harmless if already a zombie and never targets a reused PID.
        let _ = self.child.kill();
        let ids: Vec<_> = self.owned_displays.iter().copied().collect();
        if self.inspect_displays {
            let _ = macrdp_display::wait_for_owned_virtual_display_removal(
                &ids,
                // Parent death gives the helper 16 seconds to finish its owned
                // window/layout work; never kill the group before that grace.
                Duration::from_secs(18),
            );
        }
        // The leader has deliberately not been reaped: group identity cannot
        // be reused here, including when no display was created before a hang.
        kill_group(pid);
        let reap_deadline = Instant::now() + Duration::from_secs(2);
        while self.child.try_wait()?.is_none() {
            anyhow::ensure!(
                Instant::now() < reap_deadline,
                "Isolated server worker has not exited"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        self.retired = true;
        if self.inspect_displays {
            macrdp_display::wait_for_owned_virtual_display_removal(&ids, Duration::from_secs(5))
                .context("Recovery blocked: an owned remote display is still present")?;
            if self.private_session {
                anyhow::ensure!(
                    !macrdp_privacy::guard_recovery_pending()?,
                    "Private session lock recovery remains pending; local protection is retained"
                );
            }
        }
        if let Some(error) = discovery_error {
            return Err(error).context("Cannot verify all owned remote displays were removed");
        }
        tracing::info!(
            worker_pid = pid,
            display_count = ids.len(),
            "Isolated worker retired; remote displays removed"
        );
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if !self.retired {
            let _ = self.child.kill();
            kill_group(self.child.id());
            let _ = self.child.wait();
        }
    }
}

#[cfg(unix)]
fn isolate_group(command: &mut ProcessCommand) {
    use std::os::unix::process::CommandExt;
    // Executed in the child between fork/exec; setpgid is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn isolate_group(_: &mut ProcessCommand) {}

#[cfg(unix)]
fn kill_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}
#[cfg(not(unix))]
fn kill_group(_: u32) {}
