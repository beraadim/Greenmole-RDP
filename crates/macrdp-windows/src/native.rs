use crate::accessibility::{self, Element};
use crate::discovery::{application_pids, physical_screens, Screen};
use crate::geometry::Frame;
use crate::{Desired, Shared};
use anyhow::Result;
use core_graphics::display::CGDisplay;
use macrdp_display::{DisplayController, DisplayGeometry};
use std::collections::VecDeque;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const PASS_BUDGET: Duration = Duration::from_millis(800);
// Reserve time in the outer ten-second barrier for in-flight AX calls.
const RESTORE_BUDGET: Duration = Duration::from_secs(6);
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);
const MAX_WINDOWS: usize = 1024;

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

struct Record {
    pid: i32,
    element: Element,
    original: Frame,
    source: Screen,
    last_target: Frame,
    pending_frame: Frame,
    settled: bool,
    closed: bool,
    resizable: bool,
    changed: bool,
    attempts: u8,
}

struct Session {
    target: DisplayGeometry,
    physical: Vec<Screen>,
    fallback_id: u32,
    records: Vec<Record>,
    pids: VecDeque<i32>,
    pending: VecDeque<(i32, Element)>,
    next_discovery: Instant,
    limited: bool,
    last_report: Option<(usize, usize)>,
    prune_cursor: usize,
}

impl Session {
    fn new(target: DisplayGeometry) -> Result<Self> {
        let physical = physical_screens(target.display_id)?;
        let fallback_id = physical[0].id;
        Ok(Self {
            target,
            physical,
            fallback_id,
            records: Vec::new(),
            pids: VecDeque::new(),
            pending: VecDeque::new(),
            next_discovery: Instant::now(),
            limited: false,
            last_report: None,
            prune_cursor: 0,
        })
    }

    fn reconcile(&mut self, shared: &Shared, expected: Desired) {
        let deadline = Instant::now() + PASS_BUDGET;
        let target = Frame::from(self.target);
        for record in &mut self.records {
            if interrupted(shared, expected) || Instant::now() >= deadline {
                return;
            }
            if record.last_target == target && record.settled {
                continue;
            }
            if record.attempts >= 3 {
                continue;
            }
            record.attempts += 1;
            match record.element.movable().and_then(|movable| {
                movable
                    .map(|resizable| record.element.frame().map(|current| (current, resizable)))
                    .transpose()
            }) {
                Ok(Some((current, resizable))) => {
                    if record.last_target != target {
                        record.pending_frame =
                            current.after_resize(record.last_target, target, resizable);
                        record.last_target = target;
                        record.settled = false;
                    }
                    if interrupted(shared, expected) {
                        return;
                    }
                    // Retain the original even on a partially successful AX mutation.
                    record.changed = true;
                    if record
                        .element
                        .set_frame(record.pending_frame, resizable)
                        .is_ok()
                    {
                        record.settled = true;
                        record.resizable = resizable;
                        record.attempts = 0;
                    }
                }
                Ok(None) => {}
                Err(error) if error.closed() => {
                    record.changed = false;
                    record.attempts = 3;
                    record.closed = true;
                }
                Err(_) => {}
            }
        }

        if self.pids.is_empty() && self.pending.is_empty() && Instant::now() >= self.next_discovery
        {
            let pids = application_pids();
            // CG omission can be temporary (Spaces, permission, app state).
            // Only a native invalid-element result can retire a saved snapshot.
            for _ in 0..4 {
                if self.records.is_empty()
                    || Instant::now() >= deadline
                    || interrupted(shared, expected)
                {
                    break;
                }
                self.prune_cursor %= self.records.len();
                let record = &mut self.records[self.prune_cursor];
                if matches!(record.element.frame(), Err(error) if error.closed()) {
                    record.closed = true;
                }
                self.prune_cursor += 1;
            }
            self.records.retain(|record| !record.closed);
            if let Ok(screens) = physical_screens(self.target.display_id) {
                self.physical = screens;
            }
            self.pids = pids.into_iter().collect();
            self.next_discovery = Instant::now() + DISCOVERY_INTERVAL;
        }
        while Instant::now() < deadline && !interrupted(shared, expected) {
            if let Some((pid, element)) = self.pending.pop_front() {
                if self
                    .records
                    .iter()
                    .any(|record| record.pid == pid && record.element.same(&element))
                {
                    continue;
                }
                if self.records.len() >= MAX_WINDOWS {
                    if !self.limited {
                        tracing::warn!(
                            limit = MAX_WINDOWS,
                            "Window relocation snapshot limit reached"
                        );
                        self.limited = true;
                    }
                    self.pending.clear();
                    self.pids.clear();
                    break;
                }
                let Ok(Some(resizable)) = element.movable() else {
                    continue;
                };
                let Ok(frame) = element.frame() else {
                    continue;
                };
                let source = self
                    .physical
                    .iter()
                    .copied()
                    .max_by(|a, b| {
                        frame
                            .intersection(a.bounds)
                            .total_cmp(&frame.intersection(b.bounds))
                    })
                    .unwrap();
                // Windows born on the virtual desktop return to the physical
                // main display. Existing physical windows keep exact originals.
                let on_virtual = frame.intersection(target) > frame.intersection(source.bounds);
                let (source, original, from) = if on_virtual {
                    let fallback = self
                        .physical
                        .iter()
                        .copied()
                        .find(|screen| screen.id == self.fallback_id)
                        .unwrap_or(self.physical[0]);
                    (
                        fallback,
                        frame.relocate(target, fallback.bounds, resizable),
                        target,
                    )
                } else {
                    (source, frame, source.bounds)
                };
                let destination = frame.relocate(from, target, resizable);
                let mut record = Record {
                    pid,
                    element,
                    original,
                    source,
                    last_target: target,
                    pending_frame: destination,
                    settled: false,
                    closed: false,
                    resizable,
                    changed: true,
                    attempts: 1,
                };
                if interrupted(shared, expected) {
                    return;
                }
                if record.element.set_frame(destination, resizable).is_ok() {
                    record.settled = true;
                    record.attempts = 0;
                }
                self.records.push(record);
            } else if let Some(pid) = self.pids.pop_front() {
                if let Ok(windows) = accessibility::windows(pid) {
                    self.pending
                        .extend(windows.into_iter().map(|window| (pid, window)));
                }
            } else {
                break;
            }
        }
    }

    fn report(&mut self) {
        if !self.pending.is_empty() || !self.pids.is_empty() {
            return;
        }
        let moved = self.records.iter().filter(|record| record.settled).count();
        let failed = self
            .records
            .iter()
            .filter(|record| !record.settled && record.attempts >= 3)
            .count();
        if self.last_report != Some((moved, failed)) {
            tracing::info!(
                moved,
                failed,
                tracked = self.records.len(),
                "Remote desktop window placement status"
            );
            self.last_report = Some((moved, failed));
        }
    }

    fn update_target(&mut self, target: DisplayGeometry) {
        if self.target != target {
            for record in &mut self.records {
                record.attempts = 0;
            }
            self.target = target;
        }
    }

    fn restore(self) -> usize {
        let deadline = Instant::now() + RESTORE_BUDGET;
        let screens = physical_screens(self.target.display_id).unwrap_or_default();
        let total = self.records.iter().filter(|record| record.changed).count();
        let mut restored = 0;
        let mut failures = 0;
        let mut closed = 0;
        for record in self.records.into_iter().filter(|record| record.changed) {
            if Instant::now() >= deadline {
                failures += 1;
                continue;
            }
            let destination = screens
                .iter()
                .find(|screen| screen.id == record.source.id)
                .map(|screen| {
                    if screen.bounds == record.source.bounds {
                        record.original
                    } else {
                        record.original.relocate(
                            record.source.bounds,
                            screen.bounds,
                            record.resizable,
                        )
                    }
                })
                .or_else(|| {
                    screens.first().map(|screen| {
                        record.original.relocate(
                            record.source.bounds,
                            screen.bounds,
                            record.resizable,
                        )
                    })
                });
            let Some(destination) = destination else {
                failures += 1;
                continue;
            };
            match record.element.restorable().and_then(|movable| {
                if let Some(resizable) = movable {
                    record
                        .element
                        .set_frame(destination, resizable)
                        .map(|_| true)
                } else {
                    Ok(false)
                }
            }) {
                Ok(true) => restored += 1,
                Err(error) if error.closed() => closed += 1,
                _ => failures += 1,
            }
        }
        if failures == 0 {
            tracing::info!(
                total,
                restored,
                closed,
                "Returned remote-session windows to physical displays"
            );
        } else {
            tracing::warn!(total, restored, closed, failures, "Some remote-session windows could not be restored; fullscreen or unresponsive applications are not forced");
        }
        failures
    }
}

fn interrupted(shared: &Shared, expected: Desired) -> bool {
    let now = shared.desired();
    if now.stopping || now.epoch != expected.epoch || now.target != expected.target {
        return true;
    }
    // Native modes can change before activation posts its new capture geometry.
    expected
        .target
        .map(|target| {
            let bounds = CGDisplay::new(target.display_id).bounds();
            !Frame {
                x: bounds.origin.x,
                y: bounds.origin.y,
                width: bounds.size.width,
                height: bounds.size.height,
            }
            .close_to(Frame::from(target))
        })
        .unwrap_or(true)
}

pub(crate) fn run(
    display: DisplayController,
    shared: Arc<Shared>,
    ready: mpsc::Sender<Result<()>>,
) {
    // The display owner is deliberately released after the final restoration.
    let display_lifetime = display;
    if !accessibility::trusted() {
        let _ = ready.send(Err(anyhow::anyhow!(
            "Accessibility permission is required for automatic window relocation"
        )));
        shared.lock().stopped = true;
        shared.changed.notify_all();
        return;
    }
    if ready.send(Ok(())).is_err() {
        return;
    }
    let mut epoch = 0;
    let mut session: Option<Session> = None;
    loop {
        let desired = shared.desired();
        if desired.stopping || desired.epoch != epoch {
            let failures = session.take().map(Session::restore).unwrap_or(0);
            epoch = desired.epoch;
            shared.complete_restore(epoch, failures);
            if desired.stopping {
                break;
            }
        }
        if let Some(target) = desired.target {
            if session.is_none() {
                match Session::new(target) {
                    Ok(value) => {
                        tracing::info!(
                            display_id = target.display_id,
                            excluded_pid = std::process::id(),
                            "Moving other application windows into the remote virtual desktop; macrdp control windows stay in place"
                        );
                        session = Some(value);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Cannot start automatic window relocation")
                    }
                }
            }
            if let Some(session) = session.as_mut() {
                session.update_target(target);
                session.reconcile(&shared, desired);
                session.report();
            }
        }
        let state = shared.lock();
        // Recheck under the lock to avoid losing a disconnect notification.
        if state.desired.epoch == desired.epoch
            && state.desired.target == desired.target
            && !state.desired.stopping
        {
            let _ = shared
                .changed
                .wait_timeout(state, Duration::from_millis(500));
        }
    }
    // Signal the shutdown barrier only after releasing our display owner. If
    // this is its last handle, helper teardown must finish before a replacement
    // server can create another virtual display.
    drop(display_lifetime);
    shared.lock().stopped = true;
    shared.changed.notify_all();
}
