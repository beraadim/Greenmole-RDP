//! Start video only after the client confirms AVC. Native initialization cannot
//! stall capture delivery, connection cancellation, or the server control loop.

use super::{encoder_startup, MacDisplayUpdates};
use ironrdp_server::gfx::GfxState;
use macrdp_encode::{EncoderPreference, Quality};
use std::time::{Duration, Instant};

pub(super) fn accepts_avc(gfx: &GfxState, quality: Quality, preference: EncoderPreference) -> bool {
    quality != Quality::Lossless
        && preference != EncoderPreference::Bitmap
        && gfx.session_uses_gfx
        && gfx.has_gfx_channel()
        && gfx.avc420_supported
        && !gfx.bitmap_mode()
        && !gfx.avc_fallback_to_uncompressed
        && !gfx.gfx_validation_mode
}

impl MacDisplayUpdates {
    pub(super) fn sync_encoder_startup(&mut self) {
        let (eligible, mode_444) = {
            let gfx = self.gfx_state.lock().unwrap();
            (
                !self.encoder_failed && accepts_avc(&gfx, self.quality, self.encoder_pref),
                self.mode_444 && gfx.use_avc444(),
            )
        };
        if !eligible {
            if self.encoder_startup.take().is_some() {
                tracing::info!("AVC no longer selected; abandoning pending encoder initialization");
            }
            if self.encoder.is_some() {
                self.retire_current_encoder();
            }
            return;
        }
        if let Some(startup) = &mut self.encoder_startup {
            match startup.poll(Instant::now()) {
                encoder_startup::Poll::Pending => return,
                encoder_startup::Poll::Ready(Ok(mut encoder)) => {
                    encoder.force_keyframe();
                    {
                        let mut gfx = self.gfx_state.lock().unwrap();
                        gfx.active_encoder = Some(encoder.backend_name());
                        gfx.applied_encoder_bitrate = encoder.applied_bitrate();
                    }
                    tracing::info!(
                        backend = encoder.backend_name(),
                        "AVC encoder ready; switching from uncompressed graphics"
                    );
                    self.encoder = Some(encoder);
                    // Configuration/adaptation may have changed while startup
                    // was pending; do not silently retain its older target.
                    self.apply_encoder_bitrate(self.bitrate_ctrl.current_bitrate());
                    self.needs_full_gfx_seed = true;
                    self.client_may_be_stale = true;
                    self.settings_refresh_pending = true;
                }
                encoder_startup::Poll::Ready(Err(error)) => {
                    self.encoder_failed = true;
                    tracing::warn!(%error, "Encoder initialization failed; continuing uncompressed graphics");
                }
                encoder_startup::Poll::TimedOut => {
                    self.encoder_failed = true;
                    tracing::warn!(timeout_secs = 5,
                        "Encoder initialization timed out; continuing uncompressed graphics while native cleanup remains owned");
                }
            }
            self.encoder_startup = None;
            return;
        }
        if self.encoder.is_some() || self.encoder_startup_attempted {
            return;
        }
        // A resized/reconnected capture can arrive before the previous worker
        // finishes retiring. Admission denial is not a failed native attempt:
        // stay uncompressed and retry only after the one global slot is free.
        if encoder_startup::pending() || macrdp_encode::native_cleanup_pending() {
            return;
        }
        // One attempt per capture. Repeated frames, toggles, and reconnects must
        // not accumulate workers behind a stuck native initialization.
        self.encoder_startup_attempted = true;
        let width = self.capture_config.width;
        let height = self.capture_config.height;
        // Raw fallback may temporarily pace capture below the intended video
        // rate while a previous initializer retires. Initialize AVC for the
        // video controller's rate, which is restored when it becomes ready.
        let fps = self.bitrate_ctrl.current_fps() as f32;
        let bitrate = self.bitrate_ctrl.current_bitrate();
        let quality = self.quality;
        let preference = self.encoder_pref;
        match encoder_startup::Initializer::start(move || {
            let mut encoder = macrdp_encode::create_encoder(
                width, height, fps, quality, preference, mode_444, bitrate,
            )?;
            encoder.prepare()?;
            Ok(encoder)
        }) {
            Ok(startup) => {
                tracing::info!(
                    width,
                    height,
                    fps,
                    bitrate,
                    ?quality,
                    ?preference,
                    mode_444,
                    "Client accepted AVC; starting encoder on bounded native worker"
                );
                self.gfx_state.lock().unwrap().requested_encoder_bitrate = Some(bitrate);
                self.encoder_startup = Some(startup);
            }
            Err(error) => {
                self.encoder_failed = true;
                tracing::warn!(%error, "Encoder startup unavailable; continuing uncompressed graphics")
            }
        }
    }

    pub(super) fn encoder_control_timeout(&self) -> Option<Duration> {
        let gfx = self.gfx_state.lock().unwrap();
        // Burned-in cursors disable normal pointer polling. Continue observing
        // capability negotiation and completion even on an unchanged desktop.
        (self.encoder_startup.is_some()
            || (!self.encoder_startup_attempted
                && !self.encoder_failed
                && self.quality != Quality::Lossless
                && self.encoder_pref != EncoderPreference::Bitmap
                && gfx.session_uses_gfx
                && !gfx.bitmap_mode()
                && (!gfx.caps_confirmed || accepts_avc(&gfx, self.quality, self.encoder_pref))))
        .then_some(Duration::from_millis(100))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_waits_for_explicit_avc_acceptance_and_never_starts_for_bitmap_or_lossless() {
        let mut gfx = GfxState::new(1920, 1080, false, false);
        assert!(!accepts_avc(
            &gfx,
            Quality::HighQuality,
            EncoderPreference::Hardware
        ));
        gfx.session_uses_gfx = true;
        assert!(!accepts_avc(
            &gfx,
            Quality::HighQuality,
            EncoderPreference::Hardware
        ));
        gfx.channel_id = Some(1);
        gfx.avc420_supported = true;
        assert!(!accepts_avc(
            &gfx,
            Quality::HighQuality,
            EncoderPreference::Hardware
        ));
        gfx.caps_confirmed = true;
        assert!(accepts_avc(
            &gfx,
            Quality::HighQuality,
            EncoderPreference::Hardware
        ));
        assert!(!accepts_avc(
            &gfx,
            Quality::Lossless,
            EncoderPreference::Hardware
        ));
        gfx.bitmap_fallback_mode = true;
        assert!(!accepts_avc(
            &gfx,
            Quality::HighQuality,
            EncoderPreference::Hardware
        ));
        gfx.bitmap_fallback_mode = false;
        gfx.gfx_validation_mode = true;
        assert!(!accepts_avc(
            &gfx,
            Quality::Balanced,
            EncoderPreference::Hardware
        ));
        gfx.gfx_validation_mode = false;
        gfx.avc_fallback_to_uncompressed = true;
        assert!(!accepts_avc(
            &gfx,
            Quality::Balanced,
            EncoderPreference::Hardware
        ));
        gfx.avc_fallback_to_uncompressed = false;
        gfx.avc420_supported = false;
        assert!(!accepts_avc(
            &gfx,
            Quality::Balanced,
            EncoderPreference::Hardware
        ));
    }
}
