use crate::manual_windows::geometry::Frame;
use crate::window_move::Budget;

pub(super) const MAX_SAVED_WINDOWS: usize = 1024;

/// UUIDs, rather than transient CoreGraphics IDs, keep a reconnected monitor
/// identifiable without ever confusing a new monitor that reuses its old ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DisplayKey(pub [u8; 16]);

#[derive(Clone, Copy, Debug)]
pub(super) struct Screen {
    pub key: DisplayKey,
    pub frame: Frame,
    pub primary: bool,
}

#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct PhaseReport {
    pub phase: String,
    pub elapsed_ms: u64,
    pub inspected: usize,
    pub saved: usize,
    pub moved: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub failed: usize,
    pub app_failures: usize,
    pub fallback: usize,
    pub timed_out: bool,
    pub limit_reached: bool,
}

/// Both phases recheck movability so a window minimized, closed, or placed in
/// fullscreen during the session is not resurrected or pulled out of its Space.
pub(super) trait Window {
    type Error: std::fmt::Display;
    fn same_window(&self, other: &Self) -> bool;
    fn inspect(&mut self, budget: &Budget) -> Result<Option<(Frame, bool)>, Self::Error>;
    fn set_frame(
        &mut self,
        frame: Frame,
        resizable: bool,
        budget: &Budget,
    ) -> Result<(), Self::Error>;
}

struct Saved<W> {
    window: W,
    frame: Frame,
    source: Screen,
}

pub(super) struct Session<W> {
    screens: Vec<Screen>,
    saved: Vec<Saved<W>>,
    gathered: bool,
    restored: bool,
}

impl<W: Window> Session<W> {
    pub fn new(screens: Vec<Screen>) -> Self {
        Self {
            screens,
            saved: Vec::new(),
            gathered: false,
            restored: false,
        }
    }

    pub fn capture_window(&mut self, mut window: W, budget: &Budget, report: &mut PhaseReport) {
        if budget.expired() {
            report.timed_out = true;
            return;
        }
        if self.saved.len() >= MAX_SAVED_WINDOWS {
            report.limit_reached = true;
            return;
        }
        if self
            .saved
            .iter()
            .any(|saved| saved.window.same_window(&window))
        {
            return;
        }
        report.inspected += 1;
        match window.inspect(budget) {
            Ok(Some((frame, _))) if frame.valid() => {
                let source = self
                    .screens
                    .iter()
                    .copied()
                    .max_by(|a, b| {
                        frame
                            .intersection(a.frame)
                            .total_cmp(&frame.intersection(b.frame))
                    })
                    .filter(|screen| frame.intersection(screen.frame) > 0.0);
                if let Some(source) = source {
                    self.saved.push(Saved {
                        window,
                        frame,
                        source,
                    });
                    report.saved += 1;
                } else {
                    // Do not adopt windows on other virtual or disconnected
                    // screens when we have no physical source to restore to.
                    report.skipped += 1;
                }
            }
            Ok(_) => report.skipped += 1,
            Err(_) => report.failed += 1,
        }
        report.timed_out |= budget.expired();
    }

    /// A second prepare/resize can never recapture or move windows again. The
    /// saved pre-session geometry remains authoritative until disconnect.
    pub fn gather(&mut self, target: Frame, budget: &Budget) -> PhaseReport {
        let mut report = PhaseReport::default();
        if self.gathered || self.restored || !target.valid() {
            return report;
        }
        self.gathered = true;
        let target = accessible_bounds(target);
        for saved in &mut self.saved {
            // macOS follows the original primary's windows automatically;
            // retain their snapshots for restoration but never force a move.
            if saved.source.primary {
                report.skipped += 1;
                continue;
            }
            if budget.expired() {
                report.timed_out = true;
                break;
            }
            report.inspected += 1;
            match saved.window.inspect(budget) {
                Ok(Some((actual, resizable))) => {
                    let destination = saved.frame.relocate(saved.source.frame, target, resizable);
                    apply(
                        &mut saved.window,
                        actual,
                        destination,
                        resizable,
                        budget,
                        &mut report,
                    );
                }
                Ok(None) => report.skipped += 1,
                Err(_) => report.failed += 1,
            }
        }
        report.timed_out |= budget.expired();
        report
    }

    /// Call only after restoring physical display origins, before destroying
    /// the virtual display. Every retained handle is released even on timeout.
    pub fn restore(&mut self, live: &[Screen], budget: &Budget) -> PhaseReport {
        let mut report = PhaseReport::default();
        if self.restored {
            return report;
        }
        self.restored = true;
        let fallback = live
            .iter()
            .find(|screen| screen.primary)
            .or_else(|| live.first());
        let saved_windows = std::mem::take(&mut self.saved);
        for mut saved in saved_windows {
            if budget.expired() {
                report.timed_out = true;
                break;
            }
            report.inspected += 1;
            let Some(target) = live
                .iter()
                .find(|screen| screen.key == saved.source.key)
                .or(fallback)
            else {
                report.failed += 1;
                continue;
            };
            match saved.window.inspect(budget) {
                Ok(Some((actual, resizable))) => {
                    let original_available = target.key == saved.source.key;
                    let same_bounds = target.frame.close_to(saved.source.frame);
                    let destination = if original_available && same_bounds {
                        saved.frame
                    } else {
                        if !original_available {
                            report.fallback += 1;
                        }
                        // Preserve placement relative to the monitor if its
                        // origin/mode changed, and fit to a surviving screen
                        // when the original monitor has been unplugged.
                        let bounds = if original_available {
                            target.frame
                        } else {
                            accessible_bounds(target.frame)
                        };
                        saved.frame.relocate(saved.source.frame, bounds, resizable)
                    };
                    apply(
                        &mut saved.window,
                        actual,
                        destination,
                        resizable,
                        budget,
                        &mut report,
                    );
                }
                Ok(None) => report.skipped += 1,
                Err(_) => report.failed += 1,
            }
        }
        report.timed_out |= budget.expired();
        report
    }
}

fn accessible_bounds(target: Frame) -> Frame {
    Frame {
        x: target.x + 8.0,
        y: target.y + 32.0,
        width: (target.width - 16.0).max(1.0),
        height: (target.height - 40.0).max(1.0),
    }
}

fn apply<W: Window>(
    window: &mut W,
    actual: Frame,
    destination: Frame,
    resizable: bool,
    budget: &Budget,
    report: &mut PhaseReport,
) {
    if actual.close_to(destination) {
        report.unchanged += 1;
    } else if budget.expired() {
        report.timed_out = true;
    } else if window.set_frame(destination, resizable, budget).is_ok() {
        report.moved += 1;
    } else {
        // Keep the pre-mutation snapshot even if the AX setter partially
        // succeeded; disconnect must still attempt the original restoration.
        report.failed += 1;
    }
}
