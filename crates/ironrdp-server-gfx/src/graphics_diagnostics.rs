//! Bounded, payload-free evidence about the initial client graphics baseline.
//!
//! These events describe prepared commands. The transport's write/ACK counters
//! establish delivery separately; preparing a surface does not prove display.

use super::{GfxFrameUpdate, GfxState};

#[derive(Debug)]
pub(super) struct GraphicsStartupDiagnostics {
    surface_generation: u64,
    first_avc_pending: bool,
}

impl Default for GraphicsStartupDiagnostics {
    fn default() -> Self {
        Self {
            surface_generation: 0,
            first_avc_pending: true,
        }
    }
}

pub(super) struct SurfaceMetadata {
    pub width: u16,
    pub height: u16,
    pub frame_width: u16,
    pub frame_height: u16,
    /// Zero denotes a raw baseline without an encoded picture.
    pub encoded_width: u16,
    pub encoded_height: u16,
    pub graphics_frame_id: u32,
    pub bitmap_dispatches: u64,
    pub avc_frame: bool,
    pub keyframe: bool,
    pub deleted_previous_surface: bool,
}

impl GraphicsStartupDiagnostics {
    pub(super) fn surface_prepared(&mut self, surface: SurfaceMetadata) {
        self.surface_generation = self.surface_generation.saturating_add(1);
        self.first_avc_pending = true;
        tracing::info!(target: "macrdp_transition",
            component = "graphics_surface", phase = "created", state = "prepared",
            worker_pid = std::process::id(), surface_id = 0_u16,
            surface_generation = self.surface_generation,
            width = surface.width, height = surface.height,
            frame_width = surface.frame_width, frame_height = surface.frame_height,
            encoded_width = surface.encoded_width, encoded_height = surface.encoded_height,
            graphics_frame_id = surface.graphics_frame_id,
            bitmap_dispatches = surface.bitmap_dispatches,
            avc_frame = surface.avc_frame, keyframe = surface.keyframe,
            deleted_previous_surface = surface.deleted_previous_surface,
            "GFX surface setup prepared");
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct NalSummary {
    sps: bool,
    pps: bool,
    idr: bool,
}

/// Inspect Annex B headers only. Never retain bytes, hashes, or decoded pixels.
fn summarize_nals(data: &[u8]) -> NalSummary {
    let mut summary = NalSummary::default();
    let mut offset = 0;
    while offset + 3 < data.len() {
        let header = if data[offset..].starts_with(&[0, 0, 1]) {
            Some(offset + 3)
        } else if data[offset..].starts_with(&[0, 0, 0, 1]) {
            Some(offset + 4)
        } else {
            None
        };
        if let Some(header) = header {
            if let Some(nal) = data.get(header) {
                match nal & 0x1f {
                    5 => summary.idr = true,
                    7 => summary.sps = true,
                    8 => summary.pps = true,
                    _ => {}
                }
            }
            offset = header;
        } else {
            offset += 1;
        }
    }
    summary
}

pub(super) fn first_avc_prepared(state: &mut GfxState, frame: &GfxFrameUpdate, frame_id: u32) {
    if !state.startup_diagnostics.first_avc_pending {
        return;
    }
    state.startup_diagnostics.first_avc_pending = false;
    let main = summarize_nals(&frame.h264_data);
    let aux = summarize_nals(frame.h264_aux.as_deref().unwrap_or_default());
    tracing::info!(target: "macrdp_transition",
        component = "graphics_startup", phase = "first_avc", state = "prepared",
        worker_pid = std::process::id(), surface_id = 0_u16,
        surface_generation = state.startup_diagnostics.surface_generation,
        graphics_frame_id = frame_id,
        width = frame.width, height = frame.height,
        encoded_width = frame.enc_width, encoded_height = frame.enc_height,
        main_bytes = frame.h264_data.len(),
        aux_bytes = frame.h264_aux.as_ref().map_or(0, |data| data.len()),
        bitmap_dispatches = state.bitmap_dispatches,
        uncompressed_dispatches = state.uncompressed_dispatches,
        display_updates_sent = state.total_frames_sent,
        avc444 = frame.h264_aux.is_some() && state.use_avc444(),
        keyframe = frame.is_keyframe,
        main_has_sps = main.sps, main_has_pps = main.pps, main_has_idr = main.idr,
        aux_has_sps = aux.sps, aux_has_pps = aux.pps, aux_has_idr = aux.idr,
        "First AVC frame prepared");
}

#[cfg(test)]
#[path = "graphics_diagnostics_tests.rs"]
mod tests;
