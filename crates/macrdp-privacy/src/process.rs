//! Parent-side heartbeat and reciprocal failure supervision. Only anonymous
//! pipes carry session routing data; the command line contains a fixed selector.

use crate::protocol::{read_message, write_message, Message};
use anyhow::{ensure, Context, Result};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(25);
const HEARTBEAT: Duration = Duration::from_secs(1);
const LEASE: Duration = Duration::from_secs(5);
const READY_TIMEOUT: Duration = Duration::from_secs(12);
const FINISH_TIMEOUT: Duration = Duration::from_secs(12);

enum Control {
    Finish {
        force_lock: bool,
        reply: mpsc::SyncSender<Result<()>>,
    },
}

struct ReapOnDrop(Option<Child>);
impl std::ops::Deref for ReapOnDrop {
    type Target = Child;
    fn deref(&self) -> &Child {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for ReapOnDrop {
    fn deref_mut(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }
}
impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            // Never kill an admission-time helper: it may already own barriers
            // and be waiting for lock verification. Reap without delaying the
            // caller, including repeated permission/capability refusals.
            let _ = std::thread::Builder::new()
                .name("private-session-helper-reaper".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

pub(crate) struct ParentGuard {
    commands: Option<mpsc::SyncSender<Control>>,
    healthy: Arc<AtomicBool>,
    finished: bool,
}

impl ParentGuard {
    pub(crate) fn start(remote_pid: u32, tag: i64) -> Result<Self> {
        let executable =
            std::env::current_exe().context("Cannot locate private-session helper executable")?;
        let child = Command::new(executable)
            .arg(crate::HELPER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // This is deliberately outside the server worker's process group.
            // Supervisor kill-group recovery must not remove local barriers.
            .process_group(0)
            .spawn()
            .context("Cannot launch private-session guard helper")?;
        let mut child = ReapOnDrop(Some(child));
        let mut input = child
            .stdin
            .take()
            .context("Missing private-session helper input pipe")?;
        let output = child
            .stdout
            .take()
            .context("Missing private-session helper output pipe")?;
        let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0
                && unsafe {
                    libc::fcntl(input.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                } == 0,
            "Could not bound private-session control writes"
        );
        write_message(&mut input, Message::Start { remote_pid, tag })?;
        let (reply_tx, replies) = mpsc::sync_channel(8);
        std::thread::Builder::new()
            .name("private-session-helper-replies".into())
            .spawn(move || {
                let mut output = output;
                loop {
                    let message = read_message(&mut output).and_then(|message| {
                        message.context("Private-session helper closed its reply pipe")
                    });
                    let failed = message.is_err();
                    if reply_tx.send(message).is_err() || failed {
                        break;
                    }
                }
            })
            .context("Cannot start private-session reply reader")?;
        let (command_tx, commands) = mpsc::sync_channel(1);
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let healthy = Arc::new(AtomicBool::new(false));
        let monitor_health = Arc::clone(&healthy);
        std::thread::Builder::new()
            .name("private-session-parent-watchdog".into())
            .spawn(move || {
                monitor(
                    child,
                    input,
                    replies,
                    commands,
                    ready_tx,
                    monitor_health,
                    remote_pid,
                    fatal,
                );
            })
            .context("Cannot start reciprocal private-session watchdog")?;
        ready
            .recv_timeout(READY_TIMEOUT)
            .context("Private-session guard did not become ready within 12 seconds")??;
        Ok(Self {
            commands: Some(command_tx),
            healthy,
            finished: false,
        })
    }

    pub(crate) fn is_healthy(&self) -> bool {
        !self.finished && self.healthy.load(Ordering::Acquire)
    }

    pub(crate) fn finish(&mut self, force_lock: bool) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let (reply, result) = mpsc::sync_channel(1);
        let commands = self
            .commands
            .as_ref()
            .context("Private-session recovery already lost its control lease")?;
        commands
            .try_send(Control::Finish { force_lock, reply })
            .context("Private-session watchdog is unavailable or already finishing")?;
        match result.recv_timeout(FINISH_TIMEOUT) {
            Ok(Ok(())) => {
                self.finished = true;
                self.commands.take();
                self.healthy.store(false, Ordering::Release);
                Ok(())
            }
            Ok(Err(error)) => Err(error),
            Err(error) => {
                // Stop renewal. The independent helper keeps its panels/tap
                // and generation lease until positive lock verification.
                self.commands.take();
                self.healthy.store(false, Ordering::Release);
                Err(error).context("Private-session recovery is still pending; local barriers must not be reported released")
            }
        }
    }
}

impl Drop for ParentGuard {
    fn drop(&mut self) {
        if !self.finished && self.commands.is_some() {
            if let Err(error) = self.finish(true) {
                tracing::error!(%error, "Private-session guard left independent recovery active");
            }
        }
    }
}

fn monitor(
    mut child: ReapOnDrop,
    mut input: ChildStdin,
    replies: mpsc::Receiver<Result<Message>>,
    commands: mpsc::Receiver<Control>,
    ready: mpsc::SyncSender<Result<()>>,
    healthy: Arc<AtomicBool>,
    remote_pid: u32,
    failure: fn(u32, anyhow::Error) -> !,
) {
    let mut admitted = false;
    let mut must_restore_lock = true;
    let mut last_ack = Instant::now();
    let mut last_write = Instant::now() - HEARTBEAT;
    let mut finishing: Option<mpsc::SyncSender<Result<()>>> = None;
    let mut child_exited_at = None;
    loop {
        match replies.recv_timeout(POLL) {
            Ok(Ok(Message::Ready { initial })) if !admitted => {
                admitted = true;
                must_restore_lock = initial != crate::policy::LockState::Unlocked;
                healthy.store(true, Ordering::Release);
                last_ack = Instant::now();
                tracing::info!(helper_pid = child.id(), initial_lock_state = ?initial, "Private-session local barriers are ready");
                if ready.send(Ok(())).is_err() {
                    break;
                }
            }
            Ok(Ok(Message::Alive)) if admitted => {
                last_ack = Instant::now();
            }
            Ok(Ok(Message::Finished { locked })) if admitted && finishing.is_some() => {
                healthy.store(false, Ordering::Release);
                if must_restore_lock && !locked {
                    failure(remote_pid, anyhow::anyhow!("Private-session helper reported release without required lock verification"));
                }
                tracing::info!(
                    locked,
                    "Private-session lock restoration and local barrier release completed"
                );
                let _ = finishing.take().unwrap().send(Ok(()));
                // Finished is emitted after native barrier and lease release.
                // Reap asynchronously if process teardown takes another turn.
                drop(input);
                let _ = child.wait();
                return;
            }
            Ok(Ok(Message::Fault { code })) => {
                healthy.store(false, Ordering::Release);
                let error = anyhow::anyhow!(
                    "Private-session helper entered fail-closed recovery (reason {code})"
                );
                if admitted {
                    failure(remote_pid, error);
                }
                let _ = ready.send(Err(error));
                return;
            }
            Ok(Err(error)) => {
                healthy.store(false, Ordering::Release);
                if admitted {
                    failure(remote_pid, error);
                }
                let _ = ready.send(Err(error.context("Private-session admission refused; check guard helper diagnostics and permissions")));
                return;
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                healthy.store(false, Ordering::Release);
                let error =
                    anyhow::anyhow!("Invalid or unavailable private-session helper replies");
                if admitted {
                    failure(remote_pid, error);
                }
                let _ = ready.send(Err(error));
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        match commands.try_recv() {
            Ok(Control::Finish { force_lock, reply }) => {
                must_restore_lock |= force_lock;
                if finishing.is_some() {
                    let _ = reply.send(Err(anyhow::anyhow!(
                        "Private-session cleanup is already pending"
                    )));
                } else if write_message(&mut input, Message::Finish { force_lock }).is_ok() {
                    finishing = Some(reply);
                } else {
                    healthy.store(false, Ordering::Release);
                    if admitted {
                        failure(
                            remote_pid,
                            anyhow::anyhow!("Private-session finish pipe failed"),
                        );
                    }
                    let _ = reply.send(Err(anyhow::anyhow!("Private-session finish pipe failed")));
                    return;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Dropping the lease pipe makes the surviving guard recover.
                healthy.store(false, Ordering::Release);
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if last_write.elapsed() >= HEARTBEAT {
            if write_message(&mut input, Message::Heartbeat).is_err() {
                healthy.store(false, Ordering::Release);
                if admitted {
                    failure(
                        remote_pid,
                        anyhow::anyhow!("Private-session heartbeat pipe failed"),
                    );
                }
                let _ = ready.send(Err(anyhow::anyhow!(
                    "Private-session helper did not accept its lease"
                )));
                return;
            }
            last_write = Instant::now();
        }
        // A child may exit just before its reader decodes the final Finished
        // frame. Drain that bounded pipe before treating process exit as a
        // failure; never confuse normal teardown with a guard crash.
        if matches!(child.try_wait(), Ok(Some(_))) {
            healthy.store(false, Ordering::Release);
            child_exited_at.get_or_insert_with(Instant::now);
        }
        if last_ack.elapsed() >= LEASE
            || child_exited_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(500))
        {
            healthy.store(false, Ordering::Release);
            if admitted {
                failure(
                    remote_pid,
                    anyhow::anyhow!("Private-session helper heartbeat expired or helper exited"),
                );
            }
            let _ = ready.send(Err(anyhow::anyhow!(
                "Private-session helper readiness lease expired"
            )));
            return;
        }
    }
}

fn fatal(remote_pid: u32, error: anyhow::Error) -> ! {
    tracing::error!(%error, "Private-session guard lost; requesting macOS lock and terminating remote worker");
    if let Err(error) = crate::lock_session_and_verify() {
        tracing::error!(%error, "macOS lock could not be verified during guard failure");
    }
    // The helper owns reciprocal recovery if it survives us. The worker cannot
    // continue injecting/capturing after losing its local guard, including idle.
    if remote_pid == std::process::id() {
        unsafe {
            libc::kill(remote_pid as i32, libc::SIGKILL);
        }
    }
    std::process::abort()
}

#[cfg(test)]
mod tests;
