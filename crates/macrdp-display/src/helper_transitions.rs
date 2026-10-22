//! Parent operation boundaries. Topology sampling is queued without delaying
//! native requests; helper protocols and operation deadlines stay unchanged.

use super::*;

impl DisplayHelper {
    pub(crate) fn new(width: u16, height: u16, identity_slot: &str) -> Result<Self> {
        transition::observed(
            Phase::OwnerCreate,
            Details::default().size(width, height),
            || Self::initialize(width, height, identity_slot),
        )
    }

    pub(crate) fn begin_remote_session(&mut self) -> Result<DisplayGeometry> {
        transition::observed(Phase::RemoteBegin, Details::display(self.id()), || {
            self.begin_remote_session_inner()
        })
    }

    pub(crate) fn end_remote_session(&mut self) -> Result<DisplayGeometry> {
        transition::observed(Phase::RemoteEnd, Details::display(self.id()), || {
            self.end_remote_session_inner()
        })
    }

    pub(crate) fn prepare_capture(&mut self, width: u16, height: u16) -> Result<DisplayGeometry> {
        transition::observed(
            Phase::CapturePrepare,
            Details::display(self.id()).size(width, height),
            || self.prepare_capture_inner(width, height),
        )
    }
}
