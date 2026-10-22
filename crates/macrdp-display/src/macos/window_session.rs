//! Session window ownership stays with the native display owner, including EOF.

use crate::protocol::WindowStatus;
use crate::session_windows::SessionWindows;
use crate::DisplayGeometry;

#[derive(Default)]
pub(super) struct SessionState {
    windows: Option<SessionWindows>,
    completed: WindowStatus,
}

impl SessionState {
    pub(super) fn capture(&mut self, primary: u32, virtual_id: u32) {
        // The caller only enters here before its first primary-display change.
        // Never replace the original frames during a resize or repeated Begin.
        if self.windows.is_some() {
            return;
        }
        match SessionWindows::capture(primary, virtual_id) {
            Ok(windows) => self.windows = Some(windows),
            Err(error) => {
                let message = format!("Automatic window snapshot unavailable: {error:#}");
                eprintln!("{message}");
                self.completed.warnings.push(message);
            }
        }
    }

    pub(super) fn gather(&mut self, target: DisplayGeometry) {
        if let Some(windows) = &mut self.windows {
            windows.gather(target);
        }
    }

    pub(super) fn restore(&mut self) {
        if let Some(mut windows) = self.windows.take() {
            windows.restore();
            self.completed.reports.extend(windows.take_reports());
        }
    }

    pub(super) fn restore_for_parking(&mut self, virtual_id: u32) -> anyhow::Result<()> {
        self.restore();
        match SessionWindows::evacuate_for_parking(virtual_id) {
            Ok(report) => {
                self.completed.reports.push(report);
                Ok(())
            }
            Err(error) => {
                self.completed.warnings.push(format!(
                    "Retained display parking will use removal fallback: {error:#}"
                ));
                Err(error)
            }
        }
    }

    pub(super) fn pending(&self) -> bool {
        self.windows.is_some()
    }

    pub(super) fn take_status(&mut self) -> WindowStatus {
        if let Some(windows) = &mut self.windows {
            self.completed.reports.extend(windows.take_reports());
        }
        std::mem::take(&mut self.completed)
    }
}
