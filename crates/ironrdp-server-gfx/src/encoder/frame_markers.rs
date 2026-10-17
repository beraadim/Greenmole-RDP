//! Surface Commands frame boundaries span every encoded region of one capture.

use anyhow::Result;
use ironrdp_pdu::encode_vec;
use ironrdp_pdu::fast_path::UpdateCode;
use ironrdp_pdu::surface_commands::{FrameAction, FrameMarkerPdu, SurfaceCommand};

use super::UpdateFragmenter;

pub(super) struct FrameMarkers {
    supported: bool,
    next_id: u32,
    open_id: Option<u32>,
}

impl FrameMarkers {
    pub(super) fn new(supported: bool) -> Self {
        Self {
            supported,
            next_id: 1,
            open_id: None,
        }
    }

    pub(super) fn supported(&self) -> bool {
        self.supported
    }

    pub(super) fn set_supported(&mut self, supported: bool) {
        assert!(
            self.open_id.is_none(),
            "close the current frame before changing codecs"
        );
        self.supported = supported;
    }

    pub(super) fn begin(&mut self) -> Result<Option<UpdateFragmenter>> {
        if !self.supported || self.open_id.is_some() {
            return Ok(None);
        }
        let id = self.next_id;
        let output = marker(FrameAction::Begin, id)?;
        self.next_id = self.next_id.wrapping_add(1);
        self.open_id = Some(id);
        Ok(Some(output))
    }

    pub(super) fn end(&mut self) -> Result<Option<UpdateFragmenter>> {
        let Some(id) = self.open_id else {
            return Ok(None);
        };
        let output = marker(FrameAction::End, id)?;
        self.open_id = None;
        Ok(Some(output))
    }
}

fn marker(action: FrameAction, frame_id: u32) -> Result<UpdateFragmenter> {
    Ok(UpdateFragmenter::new(
        UpdateCode::SurfaceCommands,
        encode_vec(&SurfaceCommand::FrameMarker(FrameMarkerPdu {
            frame_action: action,
            frame_id: Some(frame_id),
        }))?,
    ))
}
