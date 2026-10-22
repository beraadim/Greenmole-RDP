//! Apply user graphics choices without mutating negotiated capabilities or
//! automatic failure flags. Native codec retirement owns any late callbacks.

use std::time::{Duration, Instant};

use macrdp_encode::{EncoderPreference, Quality};

use super::{encoder_lifecycle, encoder_startup, MacDisplayUpdates};

const REBUILD_BUDGET: Duration = Duration::from_secs(5);

fn rebuild_deadline(
    previous: Option<Instant>,
    has_encoder: bool,
    eligible: bool,
    now: Instant,
) -> Option<Instant> {
    ((has_encoder || previous.is_some()) && eligible)
        .then(|| previous.unwrap_or(now + REBUILD_BUDGET))
}

fn abandon_video(
    encoder: &mut Option<Box<dyn macrdp_encode::VideoEncoder>>,
    startup: &mut Option<encoder_startup::Initializer<Box<dyn macrdp_encode::VideoEncoder>>>,
    pending: &mut bool,
    visible_size: &mut Option<(u16, u16)>,
    submitted_at: &mut Option<Instant>,
    avc444: &mut Option<super::video_encode::Pending>,
) {
    *startup = None;
    *encoder = None;
    *pending = false;
    *visible_size = None;
    *submitted_at = None;
    *avc444 = None;
}

/// A new complete baseline supersedes every unsent descriptor. Preserve the
/// newest source pixels, including updates beyond an older batch's geometry.
pub(super) fn discard_bitmap_batch(
    regions: &mut std::collections::VecDeque<super::bitmap_updates::Region>,
    source: &mut Option<macrdp_capture::CapturedFrame>,
    deferred: &mut Option<macrdp_capture::CapturedFrame>,
) -> Option<macrdp_capture::CapturedFrame> {
    regions.clear();
    let latest = deferred.take().or_else(|| source.take());
    *source = None;
    latest
}

pub(super) fn quality_from_setting(value: Option<&str>, default: Quality) -> Quality {
    match value {
        Some("low_latency") => Quality::LowLatency,
        Some("balanced") => Quality::Balanced,
        Some("high_quality") => Quality::HighQuality,
        Some("lossless") => Quality::Lossless,
        _ => default,
    }
}

impl MacDisplayUpdates {
    pub(super) fn sync_live_graphics_settings(&mut self) {
        let (quality, preference, fps, bitrate, eligible) = {
            let gfx = self.gfx_state.lock().unwrap();
            let quality = quality_from_setting(gfx.target_quality.as_deref(), self.quality);
            let preference = gfx
                .encoder_pref
                .as_deref()
                .map_or(self.encoder_pref, |preference| {
                    EncoderPreference::from_str_opt(Some(preference))
                });
            let fps = gfx
                .target_frame_rate
                .unwrap_or(self.default_frame_rate)
                .clamp(1, 240);
            let bitrate = gfx
                .target_bitrate
                .or(self.bitrate_override)
                .unwrap_or_else(|| {
                    macrdp_encode::screen_bitrate(
                        self.capture_config.width,
                        self.capture_config.height,
                        fps as f32,
                        quality,
                    )
                });
            (
                quality,
                preference,
                fps,
                bitrate.clamp(1, macrdp_encode::MAX_BITRATE_BPS),
                !self.encoder_failed && encoder_lifecycle::accepts_avc(&gfx, quality, preference),
            )
        };
        if quality == self.quality && preference == self.encoder_pref {
            self.sync_runtime_encoder_overrides();
            return;
        }

        // Take the newest pixels before dropping unsent region descriptors.
        // The following baseline covers their full accumulated damage, so stale
        // geometry cannot paint over a later mode's complete desktop.
        if let Some(frame) = discard_bitmap_batch(
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
        let deadline = rebuild_deadline(
            self.encoder_rebuild_deadline,
            self.encoder.is_some(),
            eligible,
            Instant::now(),
        );
        let hold_video = deadline.is_some();
        // Abandonment closes only the bounded initializer's result channel. A
        // late encoder stays on that worker through native destruction, and its
        // global admission lease prevents rapid toggles accumulating workers.
        self.retire_current_encoder();
        self.encoder_startup_attempted = false;
        self.encoder_rebuild_deadline = deadline;

        tracing::info!(old_quality = ?self.quality, new_quality = ?quality,
            old_encoder = ?self.encoder_pref, new_encoder = ?preference,
            hold_video, avc_eligible = eligible,
            "Applying live desktop image settings; requesting a complete baseline");
        self.quality = quality;
        self.encoder_pref = preference;
        // Quantizer bounds belong to codec creation, not merely bitrate. Reset
        // stale congestion windows from the previous policy at the same time.
        self.bitrate_ctrl.set_target_bitrate(bitrate);
        self.bitrate_ctrl.set_target_fps(fps);
        self.bitrate_ctrl.on_idle_recovery();
        self.apply_capture_fps(fps, "image-settings-hot-update");
        self.request_full_bitmap_updates(1);
        self.needs_full_gfx_seed = true;
        self.settings_refresh_pending = true;
        self.mark_client_may_be_stale("image-settings-hot-update");
        self.logged_bitmap_mode = false;
        self.logged_gfx_avc_mode = false;
        self.logged_gfx_uncompressed_mode = false;
    }

    pub(super) fn retire_current_encoder(&mut self) {
        self.video_encode_recovery = Default::default();
        self.startup_mode_444 = None;
        // VideoToolbox Drop transfers its native session, input buffers and
        // callbacks to its bounded retirement owner. Never collect this old
        // submission using the newly configured codec.
        abandon_video(
            &mut self.encoder,
            &mut self.encoder_startup,
            &mut self.has_pending_submit,
            &mut self.pending_visible_size,
            &mut self.pending_frame_time,
            &mut self.pending_avc444,
        );
        let mut gfx = self.gfx_state.lock().unwrap();
        gfx.active_encoder = None;
        gfx.applied_encoder_bitrate = None;
        gfx.requested_encoder_bitrate = None;
    }

    pub(super) fn waiting_for_encoder_rebuild(&mut self) -> bool {
        let Some(deadline) = self.encoder_rebuild_deadline else {
            return false;
        };
        if self.encoder.is_some() {
            self.encoder_rebuild_deadline = None;
            self.settings_refresh_pending = true;
            return false;
        }
        let eligible = !self.encoder_failed
            && encoder_lifecycle::accepts_avc(
                &self.gfx_state.lock().unwrap(),
                self.quality,
                self.encoder_pref,
            );
        if !eligible
            || (self.encoder_startup.is_none() && self.encoder_startup_attempted)
            || Instant::now() >= deadline
        {
            self.encoder_startup = None;
            self.startup_mode_444 = None;
            self.encoder_startup_attempted = true;
            self.encoder_failed |= eligible;
            self.encoder_rebuild_deadline = None;
            self.settings_refresh_pending = true;
            tracing::warn!(
                avc_eligible = eligible,
                "Live video rebuild unavailable within its budget; recovering with images"
            );
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests;
