//! Retained native display transitions. The active-session snapshot never
//! survives a successful disconnect; reconnect captures the user's new layout.

use super::*;

impl NativeDisplay {
    pub(crate) fn can_retire_after_park_refusal(&self) -> bool {
        !self.parked
            && self.session_layout.is_none()
            && !self.windows.pending()
            && self.geometry().is_ok()
            && CGDisplay::main().id != self.display_id
    }

    pub(super) fn begin_retained_session(&mut self) -> Result<DisplayGeometry> {
        ensure!(
            self.parked && self.session_layout.is_none(),
            "Retained display is not idle"
        );
        let virtual_display = self
            .virtual_display
            .as_ref()
            .context("Missing retained native display")?;
        virtual_display.validate_owned_id()?;
        // Enabling a remembered display can itself move origins/change main.
        // Capture physical windows and layout BEFORE that native transition.
        let physical = crate::layout::Snapshot {
            primary: CGDisplay::main().id,
            displays: self.live_layout()?,
        };
        ensure!(
            physical.primary != self.display_id && !physical.displays.is_empty(),
            "No local desktop is available before retained display activation"
        );
        self.windows.capture(physical.primary, self.display_id);
        self.session_layout = Some(physical.clone());
        // Arm restoration before the first native mutation, including timeout
        // paths where enable succeeds but its response cannot be verified.
        self.parked = false;
        crate::helper::DisplayHelper::set_display_enabled(
            self.display_id,
            virtual_display.identity(),
            true,
        )?;
        let live = self.live_layout()?;
        ensure!(
            physical
                .displays
                .iter()
                .all(|saved| live.iter().any(|current| current.id == saved.id
                    && current.width == saved.width
                    && current.height == saved.height)),
            "Physical display modes/topology changed while enabling the retained screen"
        );
        // Use current physical origins as the baseline, rather than whatever
        // macOS remembered for the re-enabled display arrangement.
        let (primary, displays) = physical.restored(&live, self.display_id)?;
        let snapshot = crate::layout::Snapshot { primary, displays };
        let promoted = snapshot.promoted(self.display_id)?;
        self.session_layout = Some(snapshot);
        let geometry = crate::helper::DisplayHelper::configure_layout(
            self.display_id,
            virtual_display.identity(),
            self.display_id,
            promoted,
        )?;
        self.fresh_mode_required = true;
        eprintln!(
            "macrdp retained display enabled: display_id={}",
            self.display_id
        );
        Ok(geometry)
    }

    pub(crate) fn park_remote_session(&mut self) -> Result<()> {
        if self.parked {
            return Ok(());
        }
        self.end_remote_session()?;
        // Original snapshots cover pre-existing windows. New windows opened
        // remotely must also reach a physical desktop before it goes offline.
        self.windows.restore_for_parking(self.display_id)?;
        let virtual_display = self
            .virtual_display
            .as_ref()
            .context("Only an owned virtual screen can be parked")?;
        virtual_display.validate_owned_id()?;
        ensure!(
            self.session_layout.is_none(),
            "Cannot park an unrestored remote desktop"
        );
        crate::helper::DisplayHelper::set_display_enabled(
            self.display_id,
            virtual_display.identity(),
            false,
        )?;
        self.parked = true;
        eprintln!(
            "macrdp retained display disabled: display_id={}",
            self.display_id
        );
        Ok(())
    }
}
