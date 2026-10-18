use super::engine::{DisplayKey, PhaseReport, Screen, Session, Window};
use crate::manual_windows::{accessibility, discovery, geometry::Frame};
use crate::window_move::Budget;
use anyhow::{ensure, Result};
use core_foundation::base::TCFType;
use core_foundation::uuid::CFUUID;
use core_foundation_sys::uuid::{CFUUIDGetUUIDBytes, CFUUIDRef};
use core_graphics::display::CGDisplay;
use std::time::{Duration, Instant};

const PHASE_BUDGET: Duration = Duration::from_secs(3);

#[link(name = "ColorSync", kind = "framework")]
extern "C" {
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> CFUUIDRef;
}

/// This value is neither Send nor Sync because it retains AX handles. It must
/// live with NativeDisplay on the dedicated helper's main thread, rather than
/// being serialized to the UI process or queried by a background watchdog.
pub(crate) struct SessionWindows {
    windows: Session<accessibility::Element>,
    virtual_id: u32,
    reports: Vec<PhaseReport>,
    gather_attempted: bool,
    restore_attempted: bool,
}

impl SessionWindows {
    pub(crate) fn capture(original_primary: u32, virtual_id: u32) -> Result<Self> {
        let started = Instant::now();
        let budget = Budget::for_duration(PHASE_BUDGET);
        ensure!(
            accessibility::trusted(),
            "Accessibility access is unavailable for automatic session window placement"
        );
        let screens = physical_screens(virtual_id, original_primary)?;
        ensure!(
            !screens.is_empty(),
            "No physical display is available for session window placement"
        );
        ensure!(
            screens.iter().any(|screen| screen.primary),
            "The original physical primary display could not be identified safely"
        );
        let mut windows = Session::new(screens);
        let mut report = PhaseReport::default();
        // Window lists and inspection are all charged to the same deadline.
        // An unresponsive application cannot consume a fresh budget per window.
        for pid in discovery::application_pids()? {
            if budget.expired() {
                report.timed_out = true;
                break;
            }
            let application_windows = match accessibility::windows(pid, &budget) {
                Ok(windows) => windows,
                Err(_) => {
                    report.app_failures += 1;
                    continue;
                }
            };
            for window in application_windows {
                windows.capture_window(window, &budget, &mut report);
                if report.timed_out || report.limit_reached {
                    break;
                }
            }
            if report.timed_out || report.limit_reached {
                break;
            }
        }
        report.timed_out |= budget.expired();
        finish_phase("snapshot", &mut report, started);
        Ok(Self {
            windows,
            virtual_id,
            reports: vec![report],
            gather_attempted: false,
            restore_attempted: false,
        })
    }

    pub(crate) fn gather(&mut self, target: crate::DisplayGeometry) -> PhaseReport {
        if self.gather_attempted {
            return PhaseReport::default();
        }
        self.gather_attempted = true;
        let started = Instant::now();
        let budget = Budget::for_duration(PHASE_BUDGET);
        // Refresh Quartz after promotion/mode settlement. Never move windows
        // toward a stale origin, an unrelated monitor, or a removed display.
        let target_frame = (|| -> Result<Frame> {
            let active = CGDisplay::active_displays()
                .map_err(|error| anyhow::anyhow!("Could not refresh remote display: {error}"))?;
            ensure!(
                target.display_id == self.virtual_id && active.contains(&self.virtual_id),
                "Remote display is no longer active"
            );
            let display = CGDisplay::new(self.virtual_id);
            ensure!(!display.is_in_mirror_set(), "Remote display is mirrored");
            let rect = display.bounds();
            let frame = Frame {
                x: rect.origin.x,
                y: rect.origin.y,
                width: rect.size.width,
                height: rect.size.height,
            };
            ensure!(
                frame.valid() && frame.close_to(Frame::from(target)),
                "Remote display geometry changed before gathering windows"
            );
            Ok(frame)
        })();
        let mut report = match target_frame {
            Ok(frame) => self.windows.gather(frame, &budget),
            Err(error) => {
                eprintln!("Automatic window gather skipped: {error}");
                PhaseReport {
                    failed: 1,
                    ..PhaseReport::default()
                }
            }
        };
        finish_phase("gather", &mut report, started);
        self.reports.push(report.clone());
        report
    }

    pub(crate) fn restore(&mut self) -> PhaseReport {
        if self.restore_attempted {
            return PhaseReport::default();
        }
        self.restore_attempted = true;
        let started = Instant::now();
        let budget = Budget::for_duration(PHASE_BUDGET);
        let live = match physical_screens(self.virtual_id, CGDisplay::main().id) {
            Ok(live) => live,
            Err(error) => {
                eprintln!("Automatic window restore could not refresh physical displays: {error}");
                Vec::new()
            }
        };
        let mut report = self.windows.restore(&live, &budget);
        finish_phase("restore", &mut report, started);
        self.reports.push(report.clone());
        report
    }

    pub(crate) fn take_reports(&mut self) -> Vec<PhaseReport> {
        std::mem::take(&mut self.reports)
    }
}

impl Window for accessibility::Element {
    type Error = accessibility::AxError;

    fn same_window(&self, other: &Self) -> bool {
        accessibility::Element::same_window(self, other)
    }

    fn inspect(&mut self, budget: &Budget) -> Result<Option<(Frame, bool)>, Self::Error> {
        self.renew_budget(budget)?;
        let Some(resizable) = self.movable()? else {
            return Ok(None);
        };
        Ok(Some((self.frame()?, resizable)))
    }

    fn set_frame(
        &mut self,
        frame: Frame,
        resizable: bool,
        budget: &Budget,
    ) -> Result<(), Self::Error> {
        self.renew_budget(budget)?;
        accessibility::Element::set_frame(self, frame, resizable)
    }
}

fn physical_screens(virtual_id: u32, primary: u32) -> Result<Vec<Screen>> {
    CGDisplay::active_displays()
        .map_err(|error| anyhow::anyhow!("Could not refresh session displays: {error}"))?;
    let mut screens = Vec::new();
    for id in crate::macos::online_display_ids()? {
        let display = CGDisplay::new(id);
        if id == virtual_id
            || (display.vendor_number() == crate::virtual_display::VENDOR_ID
                && display.model_number() == crate::virtual_display::PRODUCT_ID)
        {
            continue;
        }
        let rect = display.bounds();
        let frame = Frame {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        };
        if !frame.valid() {
            continue;
        }
        let Some(key) = display_key(id) else {
            // Skipping an unidentifiable monitor is safer than restoring to a
            // different display that later reuses this transient numeric ID.
            eprintln!(
                "Automatic window placement skipped display {id}: stable identity unavailable"
            );
            continue;
        };
        ensure!(
            !screens.iter().any(|screen: &Screen| screen.key == key),
            "Display UUIDs are not unique"
        );
        screens.push(Screen {
            key,
            frame,
            primary: id == primary,
        });
    }
    Ok(screens)
}

fn display_key(id: u32) -> Option<DisplayKey> {
    // The create rule owns this reference; CFUUID releases it after copying
    // the UUID bytes. The UUID is kept only in memory for this connection.
    let raw = unsafe { CGDisplayCreateUUIDFromDisplayID(id) };
    if raw.is_null() {
        return None;
    }
    let uuid = unsafe { CFUUID::wrap_under_create_rule(raw) };
    let b = unsafe { CFUUIDGetUUIDBytes(uuid.as_concrete_TypeRef()) };
    Some(DisplayKey([
        b.byte0, b.byte1, b.byte2, b.byte3, b.byte4, b.byte5, b.byte6, b.byte7, b.byte8, b.byte9,
        b.byte10, b.byte11, b.byte12, b.byte13, b.byte14, b.byte15,
    ]))
}

fn finish_phase(phase: &str, report: &mut PhaseReport, started: Instant) {
    report.phase = phase.to_owned();
    report.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // The early display helper has no tracing subscriber; its stderr is
    // inherited by the application's diagnostic log. Never log window titles.
    eprintln!("Automatic window {phase}: inspected={} saved={} moved={} unchanged={} skipped={} failed={} app_failures={} fallback={} timed_out={} limit_reached={} elapsed_ms={}", report.inspected, report.saved, report.moved, report.unchanged, report.skipped, report.failed, report.app_failures, report.fallback, report.timed_out, report.limit_reached, report.elapsed_ms);
}
