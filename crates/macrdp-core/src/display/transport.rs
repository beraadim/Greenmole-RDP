//! An accepted video codec may be slow or unavailable. Keep that interval on
//! the negotiated bitmap encoder rather than sending raw full-desktop graphics.

use ironrdp_server::gfx::GfxState;
use macrdp_encode::{EncoderPreference, Quality};

use super::MacDisplayUpdates;

fn compressed_avc_fallback(
    gfx: &GfxState,
    encoder_available: bool,
    quality: Quality,
    preference: EncoderPreference,
) -> bool {
    !encoder_available
        && gfx.session_uses_gfx
        && gfx.has_gfx_channel()
        && gfx.avc420_supported
        && !gfx.bitmap_mode()
        && quality != Quality::Lossless
        && preference != EncoderPreference::Bitmap
        && !gfx.gfx_validation_mode
        && !gfx.avc_fallback_to_uncompressed
        && !super::DEFAULT_FORCE_FULL_FRAME_UNCOMPRESSED_GFX
}

impl MacDisplayUpdates {
    pub(super) fn uses_bitmap_transport(&self, gfx: &GfxState) -> bool {
        gfx.bitmap_mode()
            || compressed_avc_fallback(gfx, self.encoder.is_some(), self.quality, self.encoder_pref)
    }

    /// Report the actual temporary transport without changing AVC negotiation
    /// or latching a permanent failure. A completed initializer remains eligible
    /// for adoption, unlike the existing no-ACK/client rejection fallback.
    pub(super) fn sync_bitmap_transport(&mut self) {
        let fallback = {
            let mut gfx = self.gfx_state.lock().unwrap();
            let fallback = compressed_avc_fallback(
                &gfx,
                self.encoder.is_some(),
                self.quality,
                self.encoder_pref,
            );
            if fallback == gfx.encoder_bitmap_fallback {
                return;
            }
            gfx.encoder_bitmap_fallback = fallback;
            fallback
        };
        if fallback {
            // Classic bitmap diff state may predate the intervening video
            // pictures. Cover the entire desktop before accepting new deltas.
            self.request_full_bitmap_updates(1);
            self.mark_client_may_be_stale("video-unavailable-bitmap-baseline");
            self.logged_bitmap_mode = false;
            self.session_audit
                .note_fallback_reason("video-unavailable-compressed-bitmap");
        }
        tracing::info!(target: "macrdp_transition",
            component = "graphics_transport", worker_pid = std::process::id(),
            phase = "compressed_bitmap_fallback",
            state = if fallback { "begin" } else { "end" },
            encoder_failed = self.encoder_failed,
            encoder_pending = self.encoder_startup.is_some(),
            "Video availability changed the effective graphics transport");
    }
}
