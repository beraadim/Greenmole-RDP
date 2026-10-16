//! A blocked native owner must not outlive its parent indefinitely. This thread
//! only observes its own parent and exits its own process; it never signals one.

use anyhow::{Context, Result};
use std::time::{Duration, Instant};

const RESTORATION_GRACE: Duration = Duration::from_secs(6);
const POLL: Duration = Duration::from_millis(100);

extern "C" {
    fn getppid() -> i32;
    fn _exit(status: i32) -> !;
}

pub(crate) fn start() -> Result<()> {
    // PPID changes on parent death and cannot return to the old owner if its
    // PID is reused. An already-orphaned launch must also retire itself.
    let owner = unsafe { getppid() };
    std::thread::Builder::new()
        .name("display-parent-watchdog".into())
        .spawn(move || {
            monitor(
                || owner > 1 && unsafe { getppid() } == owner,
                Instant::now,
                std::thread::sleep,
                // SAFETY: This is the isolated display owner after its parent
                // disappeared. _exit deliberately skips potentially stuck native
                // destructors and atexit handlers; WindowServer releases its ID.
                || unsafe { _exit(70) },
            );
        })
        .context("Could not start the display parent watchdog")?;
    Ok(())
}

fn monitor(
    mut parent_alive: impl FnMut() -> bool,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
    exit: impl FnOnce(),
) {
    let mut parent_lost_at = None;
    loop {
        let current = now();
        if parent_lost_at.is_none() && !parent_alive() {
            parent_lost_at = Some(current);
        }
        if parent_lost_at
            .is_some_and(|at| current.saturating_duration_since(at) >= RESTORATION_GRACE)
        {
            exit();
            return;
        }
        sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn healthy_parent_survives_then_receives_a_full_restoration_grace() {
        let elapsed = Cell::new(Duration::ZERO);
        let start = Instant::now();
        monitor(
            || elapsed.get() < Duration::from_secs(10),
            || start + elapsed.get(),
            |delay| elapsed.set(elapsed.get() + delay),
            || assert_eq!(elapsed.get(), Duration::from_secs(16)),
        );
    }

    #[test]
    fn orphan_watchdog_exits_independently_of_blocked_native_work() {
        let (release, blocked) = std::sync::mpsc::channel();
        let owner =
            std::thread::spawn(move || blocked.recv_timeout(Duration::from_secs(2)).unwrap());
        let elapsed = Cell::new(Duration::ZERO);
        let start = Instant::now();
        monitor(
            || false,
            || start + elapsed.get(),
            |delay| elapsed.set(elapsed.get() + delay),
            || {
                assert!(!owner.is_finished(), "watchdog waited for the native owner");
                assert_eq!(elapsed.get(), RESTORATION_GRACE);
            },
        );
        release.send(()).unwrap();
        owner.join().unwrap();
    }
}
