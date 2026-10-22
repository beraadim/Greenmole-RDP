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
                tracing::info!(target: "macrdp_transition",
                    component = "encoder_startup", phase = "startup", state = "cancelled",
                    "AVC no longer selected; abandoning pending encoder initialization");
            }
            self.startup_mode_444 = None;
            if self.encoder.is_some() {
                self.retire_current_encoder();
            }
            return;
        }
        if self.encoder_startup.is_some() && self.startup_mode_444 != Some(mode_444) {
            // A codec prepared for the old reference-chain format cannot be
            // adopted after the negotiated AVC mode changes. The old worker
            // retains its capacity until it has retired that result.
            self.encoder_startup = None;
            self.startup_mode_444 = None;
            self.encoder_startup_attempted = false;
            tracing::info!(target: "macrdp_transition",
                component = "encoder_startup", phase = "startup", state = "cancelled",
                reason = "negotiated_chroma_changed",
                "Discarding encoder prepared for a previous AVC mode");
        }
        if let Some(startup) = &mut self.encoder_startup {
            match startup.poll(Instant::now()) {
                encoder_startup::Poll::Pending => return,
                encoder_startup::Poll::SlowStart => {
                    tracing::info!(target: "macrdp_transition",
                        component = "encoder_startup", phase = "startup", state = "slow",
                        notice_secs = encoder_startup::SLOW_START_NOTICE.as_secs(),
                        timeout_secs = encoder_startup::STARTUP_TIMEOUT.as_secs(),
                        "Encoder startup is slow; keeping compressed image updates while the bounded native worker continues");
                    return;
                }
                encoder_startup::Poll::Ready(Ok(mut encoder)) => {
                    self.prepare_video_handoff();
                    encoder.force_keyframe();
                    {
                        let mut gfx = self.gfx_state.lock().unwrap();
                        gfx.active_encoder = Some(encoder.backend_name());
                        gfx.applied_encoder_bitrate = encoder.applied_bitrate();
                    }
                    tracing::info!(target: "macrdp_transition",
                        component = "encoder_startup", phase = "startup", state = "ready",
                        backend = encoder.backend_name(),
                        "AVC encoder ready; replacing compressed image fallback with a fresh video baseline"
                    );
                    self.encoder = Some(encoder);
                    self.sync_bitmap_transport();
                    // Configuration/adaptation may have changed while startup
                    // was pending; do not silently retain its older target.
                    self.apply_encoder_bitrate(self.bitrate_ctrl.current_bitrate());
                    self.apply_capture_fps(self.bitrate_ctrl.current_fps(), "video-startup-ready");
                    self.needs_full_gfx_seed = true;
                    self.client_may_be_stale = true;
                    self.settings_refresh_pending = true;
                }
                encoder_startup::Poll::Ready(Err(error)) => {
                    self.encoder_failed = true;
                    tracing::warn!(%error, "Encoder initialization failed; continuing compressed image fallback");
                }
                encoder_startup::Poll::TimedOut => {
                    self.encoder_failed = true;
                    tracing::warn!(timeout_secs = encoder_startup::STARTUP_TIMEOUT.as_secs(),
                        "Encoder initialization timed out; continuing compressed image fallback while native cleanup remains owned");
                }
            }
            self.encoder_startup = None;
            self.startup_mode_444 = None;
            return;
        }
        if self.encoder.is_some() || self.encoder_startup_attempted {
            return;
        }
        // A resized/reconnected capture can arrive before the previous worker
        // finishes retiring. Admission denial is not a failed native attempt:
        // keep compressed images and retry only after the global slot is free.
        if encoder_startup::pending() || macrdp_encode::native_cleanup_pending() {
            return;
        }
        // One attempt per capture. Repeated frames, toggles, and reconnects must
        // not accumulate workers behind a stuck native initialization.
        self.encoder_startup_attempted = true;
        let width = self.capture_config.width;
        let height = self.capture_config.height;
        // Image fallback may temporarily pace capture below the intended video
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
                tracing::info!(target: "macrdp_transition",
                    component = "encoder_startup", phase = "startup", state = "begin",
                    timeout_secs = encoder_startup::STARTUP_TIMEOUT.as_secs(),
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
                self.startup_mode_444 = Some(mode_444);
            }
            Err(error) => {
                self.encoder_failed = true;
                tracing::warn!(%error, "Encoder startup unavailable; continuing compressed image fallback")
            }
        }
    }

    fn prepare_video_handoff(&mut self) {
        // A compressed bitmap can be split over multiple writes. Preserve its
        // newest source, then discard every unsent tile before an AVC baseline
        // supersedes it. Otherwise old RGB tiles could repaint after video.
        if let Some(frame) = super::live_controls::discard_bitmap_batch(
            &mut self.pending_bitmap_updates,
            &mut self.pending_bitmap_frame,
            &mut self.deferred_bitmap_frame,
        ) {
            self.cache_latest_frame(&frame);
        }
        if let Some(frame) = self
            .capturer
            .as_mut()
            .and_then(super::NativeCapture::try_latest_image)
        {
            self.record_capture_delivery();
            self.cache_latest_frame(&frame);
        }
        let gfx = self.gfx_state.lock().unwrap();
        self.bitmap_pacing.reset(super::bitmap_pacing::BitmapCosts {
            dispatches: gfx.bitmap_dispatches,
            encode_ms: gfx.bitmap_encode_ms_total,
            write_ms: gfx.bitmap_write_ms_total,
        });
        self.uncompressed_pacing
            .reset(super::uncompressed_pacing::UncompressedCosts {
                dispatches: gfx.uncompressed_dispatches,
                prepare_ms: gfx.uncompressed_prepare_ms_total,
                write_ms: gfx.uncompressed_write_ms_total,
            });
        drop(gfx);
        self.needs_full_bitmap_seed = false;
        self.forced_full_bitmap_updates_remaining = 0;
        self.logged_bitmap_mode = false;
        self.logged_gfx_avc_mode = false;
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
