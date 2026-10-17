//! Keep an AVC444 reference chain intact across transient native drops. A
//! missing view cannot be substituted with AVC420 from the same codec session.

use std::time::{Duration, Instant};

use anyhow::Result;
use ironrdp_server::{DisplayUpdate, GfxFrameUpdate};
use macrdp_capture::{CapturedFrame, FrameData};
use macrdp_encode::VideoEncoder;

use super::{CachedFrame, DisplayWake, FrameStats, MacDisplayUpdates};

const FAILURE_BUDGET: Duration = Duration::from_secs(5);
const WARNING_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct Recovery {
    first_failure: Option<Instant>,
    last_warning: Option<Instant>,
    failures: u64,
}

pub(super) struct Pending {
    started_at: Instant,
    deadline: Instant,
    width: u16,
    height: u16,
}

impl Pending {
    pub(super) fn new(started_at: Instant, width: u16, height: u16) -> Self {
        Self {
            started_at,
            deadline: started_at + FAILURE_BUDGET,
            width,
            height,
        }
    }

    fn limit_deadline(&mut self, recovery_deadline: Option<Instant>) {
        if let Some(recovery_deadline) = recovery_deadline {
            self.deadline = self.deadline.min(recovery_deadline);
        }
    }

    fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CompletionWake {
    Ready,
    Cursor,
    Closed,
    Timeout,
}

async fn wait_completion(
    pending: &Pending,
    completion: &mut tokio::sync::watch::Receiver<Option<macrdp_encode::EncodeTiming>>,
    cursor_timeout: Option<Duration>,
) -> CompletionWake {
    let remaining = pending.remaining(Instant::now());
    if remaining.is_zero() {
        return CompletionWake::Timeout;
    }
    match tokio::time::timeout(
        remaining,
        super::event_wait::next_event(
            completion.wait_for(|timing| timing.is_some()),
            None,
            cursor_timeout,
        ),
    )
    .await
    {
        Ok(DisplayWake::Capture(Ok(_))) => CompletionWake::Ready,
        Ok(DisplayWake::Capture(Err(_))) => CompletionWake::Closed,
        Ok(DisplayWake::Cursor | DisplayWake::Idle) => CompletionWake::Cursor,
        Err(_) => CompletionWake::Timeout,
    }
}

pub(super) struct Failure {
    pub failures: u64,
    pub elapsed: Duration,
    pub warn: bool,
    pub fallback: bool,
}

impl Recovery {
    fn deadline(&self) -> Option<Instant> {
        self.first_failure.map(|first| first + FAILURE_BUDGET)
    }

    fn failed(&mut self, started_at: Instant, now: Instant, transient: bool) -> Failure {
        // A slow callback already consumed recovery time before reporting its
        // drop. Later pairs share the first unsent submission's budget.
        let first = *self.first_failure.get_or_insert(started_at.min(now));
        self.failures = self.failures.saturating_add(1);
        let elapsed = now.saturating_duration_since(first);
        let warn = !transient
            && self
                .last_warning
                .is_none_or(|last| now.saturating_duration_since(last) >= WARNING_INTERVAL);
        if warn {
            self.last_warning = Some(now);
        }
        Failure {
            failures: self.failures,
            elapsed,
            warn,
            fallback: elapsed >= FAILURE_BUDGET,
        }
    }

    fn succeeded(&mut self, now: Instant) -> Option<(u64, Duration)> {
        let first = self.first_failure?;
        let result = (self.failures, now.saturating_duration_since(first));
        *self = Self::default();
        Some(result)
    }
}

/// The codec choice is fixed for this call. Validate the complete pair before
/// handing anything to the RDP writer, and repair references after any generic
/// encoder violates that contract. Native typed drops retain their own repair
/// state, including the safe case where neither view was ever encoded.
pub(super) fn encode_bgra(
    encoder: &mut dyn VideoEncoder,
    bgra: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    use_444: bool,
) -> Result<GfxFrameUpdate> {
    let row_bytes = usize::try_from(width)?
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("BGRA row size overflow"))?;
    let required_len = usize::try_from(height)?
        .saturating_sub(1)
        .checked_mul(stride)
        .and_then(|previous_rows| previous_rows.checked_add(row_bytes))
        .ok_or_else(|| anyhow::anyhow!("BGRA frame size overflow"))?;
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= u32::from(u16::MAX)
            && height <= u32::from(u16::MAX)
            && stride >= row_bytes
            && bgra.len() >= required_len,
        "Invalid BGRA frame layout"
    );
    if use_444 {
        anyhow::ensure!(
            encoder.supports_444(),
            "Selected encoder does not support AVC444"
        );
        let result = encoder.encode_bgra_444(bgra, width, height, stride);
        let pair = match result {
            Ok(pair) => pair,
            Err(error) => {
                if !is_transient_drop(&error) && !is_pending(&error) {
                    encoder.force_keyframe();
                }
                return Err(error);
            }
        };
        if pair.main_view.data.is_empty()
            || pair.aux_view.data.is_empty()
            || pair.main_view.width != pair.aux_view.width
            || pair.main_view.height != pair.aux_view.height
            || !valid_encoded_size(pair.main_view.width, pair.main_view.height, width, height)
        {
            encoder.force_keyframe();
            anyhow::bail!("AVC444 encoder returned an incomplete or inconsistent frame pair");
        }
        return Ok(GfxFrameUpdate {
            width: width as u16,
            height: height as u16,
            enc_width: pair.main_view.width as u16,
            enc_height: pair.main_view.height as u16,
            // The auxiliary access unit follows the main IDR in the same H.264
            // reference chain. It may reference that main picture and remain
            // a P-frame while the whole pair is independently decodable.
            is_keyframe: pair.main_view.is_keyframe,
            h264_data: pair.main_view.data,
            h264_aux: Some(pair.aux_view.data),
        });
    }
    let frame = encoder.encode_bgra(bgra, width, height, stride)?;
    anyhow::ensure!(
        !frame.data.is_empty() && valid_encoded_size(frame.width, frame.height, width, height),
        "H.264 encoder returned no usable frame"
    );
    Ok(GfxFrameUpdate {
        width: width as u16,
        height: height as u16,
        enc_width: frame.width as u16,
        enc_height: frame.height as u16,
        is_keyframe: frame.is_keyframe,
        h264_data: frame.data,
        h264_aux: None,
    })
}

fn valid_encoded_size(
    encoded_width: u32,
    encoded_height: u32,
    visible_width: u32,
    visible_height: u32,
) -> bool {
    encoded_width >= visible_width
        && encoded_height >= visible_height
        && encoded_width <= u32::from(u16::MAX)
        && encoded_height <= u32::from(u16::MAX)
}

fn is_pending(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<macrdp_encode::Avc444EncodeError>(),
        Some(macrdp_encode::Avc444EncodeError::Pending)
    )
}

fn encode_cached_444(
    encoder: &mut dyn VideoEncoder,
    cache: &CachedFrame,
) -> Result<GfxFrameUpdate> {
    match cache {
        CachedFrame::Bgra {
            data,
            width,
            height,
            stride,
        } => encode_bgra(encoder, data, *width, *height, *stride, true),
        CachedFrame::BgraPixelBuffer(buffer, width, height, _) => buffer
            .with_locked_bgra(*width, *height, |data, stride| {
                encode_bgra(encoder, data, *width, *height, stride, true)
            })
            .ok_or_else(|| anyhow::anyhow!("Pending AVC444 BGRA buffer could not be locked"))?,
        CachedFrame::PixelBuffer(_, _, _) => anyhow::bail!("Pending AVC444 requires BGRA pixels"),
    }
}

fn is_transient_drop(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<macrdp_encode::Avc444EncodeError>(),
        Some(macrdp_encode::Avc444EncodeError::Dropped)
    )
}

impl MacDisplayUpdates {
    pub(super) fn note_video_encode_failure(
        &mut self,
        error: &anyhow::Error,
        use_444: bool,
        started_at: Instant,
    ) {
        if use_444 && is_pending(error) {
            if self.pending_avc444.is_none() {
                if let Some(cache) = &self.last_frame_cache {
                    let mut pending =
                        Pending::new(started_at, cache.width() as u16, cache.height() as u16);
                    pending.limit_deadline(self.video_encode_recovery.deadline());
                    self.pending_avc444 = Some(pending);
                }
            }
            return;
        }
        let transient = is_transient_drop(error);
        let failure = self
            .video_encode_recovery
            .failed(started_at, Instant::now(), transient);
        self.mark_client_may_be_stale("video-encode-incomplete");
        if failure.fallback {
            self.encoder_failed = true;
            self.retire_current_encoder();
            self.needs_full_gfx_seed = true;
            self.settings_refresh_pending = true;
            self.session_audit
                .note_fallback_reason("persistent-video-encode-failure");
            tracing::warn!(%error, use_444, failures = failure.failures,
                elapsed_ms = failure.elapsed.as_millis() as u64,
                "Video encoder did not recover within five seconds; recovering with image updates");
        } else if failure.warn {
            tracing::warn!(%error, use_444, failures = failure.failures,
                "Video frame unavailable; keeping the previous picture while the encoder repairs its reference chain");
        } else if failure.failures == 1 {
            tracing::debug!(%error, use_444, "Native encoder dropped a frame; preserving the negotiated video codec");
        }
    }

    /// Wait on the pending native view, keeping the first submission's deadline
    /// and visible size through both main and auxiliary callbacks. Controls can
    /// cancel this await without discarding its retained native owner.
    pub(super) async fn wait_for_pending_avc444(&mut self) -> Result<Option<DisplayUpdate>> {
        let Some(pending) = self.pending_avc444.as_ref() else {
            return Ok(None);
        };
        let started_at = pending.started_at;
        let width = pending.width;
        let height = pending.height;
        let completion = self
            .encoder
            .as_ref()
            .and_then(|encoder| encoder.encode_completion());
        let Some(mut completion) = completion else {
            self.abandon_pending_avc444("AVC444 encoder has no completion signal");
            return Ok(None);
        };
        match wait_completion(pending, &mut completion, self.cursor_poll_timeout()).await {
            CompletionWake::Ready => {}
            CompletionWake::Cursor => return Ok(None),
            CompletionWake::Closed => {
                self.abandon_pending_avc444("AVC444 completion channel closed");
                return Ok(None);
            }
            CompletionWake::Timeout => {
                self.abandon_pending_avc444(
                    "AVC444 encoder did not complete both views within five seconds",
                );
                return Ok(None);
            }
        }
        let result = match (&mut self.encoder, &self.last_frame_cache) {
            (Some(encoder), Some(cache)) => encode_cached_444(encoder.as_mut(), cache),
            _ => Err(anyhow::anyhow!(
                "Pending AVC444 encoder or source pixels unavailable"
            )),
        };
        match result {
            Err(error) if is_pending(&error) => {}
            Err(error) => {
                self.pending_avc444 = None;
                self.note_video_encode_failure(&error, true, started_at);
            }
            Ok(mut update) => {
                self.pending_avc444 = None;
                // The native pending pair belongs to the first submitted image,
                // even if capture has coalesced newer pixels while it encoded.
                update.width = width;
                update.height = height;
                let output = self.finish_bgra_update(
                    update,
                    started_at,
                    1,
                    u32::from(width) * u32::from(height),
                );
                if let Some(latest) = self
                    .capturer
                    .as_mut()
                    .and_then(super::NativeCapture::try_latest_image)
                {
                    self.record_capture_delivery();
                    self.deferred_bitmap_frame = Some(latest);
                    self.idle_since = None;
                    self.mark_client_may_be_stale("capture-advanced-during-avc444-encode");
                }
                return Ok(output);
            }
        }
        Ok(None)
    }

    fn abandon_pending_avc444(&mut self, reason: &'static str) {
        self.encoder_failed = true;
        self.retire_current_encoder();
        self.needs_full_gfx_seed = true;
        self.settings_refresh_pending = true;
        self.mark_client_may_be_stale("avc444-encode-watchdog");
        self.session_audit
            .note_fallback_reason("avc444-encode-watchdog");
        tracing::warn!(
            reason,
            "AVC444 unavailable; recovering with image updates while native cleanup remains owned"
        );
    }

    pub(super) fn note_video_encode_success(&mut self) {
        if let Some((failures, elapsed)) = self.video_encode_recovery.succeeded(Instant::now()) {
            tracing::debug!(
                failures,
                elapsed_ms = elapsed.as_millis() as u64,
                "Video encoding recovered without changing the negotiated codec"
            );
        }
    }

    pub(super) fn encode_bgra_frame(
        &mut self,
        frame: &CapturedFrame,
        use_444: bool,
        started_at: Instant,
    ) -> Option<DisplayUpdate> {
        let encoder = self.encoder.as_mut()?;
        let result = match &frame.data {
            FrameData::Raw(bgra) => encode_bgra(
                encoder.as_mut(),
                bgra,
                frame.width,
                frame.height,
                frame.stride,
                use_444,
            ),
            FrameData::BgraPixelBuffer(buffer) => buffer
                .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                    encode_bgra(
                        encoder.as_mut(),
                        bgra,
                        frame.width,
                        frame.height,
                        stride,
                        use_444,
                    )
                })
                .ok_or_else(|| anyhow::anyhow!("BGRA pixel buffer lock failed during encode"))
                .and_then(|result| result),
            FrameData::PixelBuffer(_) => unreachable!("NV12 uses the native pixel-buffer path"),
        };
        let update = match result {
            Ok(update) => update,
            Err(error) => {
                self.note_video_encode_failure(&error, use_444, started_at);
                return None;
            }
        };
        self.finish_bgra_update(
            update,
            started_at,
            frame.dirty_rects.len(),
            frame
                .dirty_rects
                .iter()
                .map(|rect| rect.width * rect.height)
                .sum(),
        )
    }

    /// Completion duration includes native callback latency, not just CPU
    /// preprocessing. Report expensive work without dropping the next fresh
    /// capture: one outstanding encode and the latest-frame mailbox already
    /// bound the queue, while the adaptive controller adjusts sustained load.
    pub(super) fn note_video_encode_duration(&mut self, completion_ms: f64) {
        let frame_interval_ms = 1000.0 / self.capture_config.frame_rate.max(1) as f64;
        if completion_ms > frame_interval_ms * 0.8 {
            self.slow_video_encodes = self.slow_video_encodes.saturating_add(1);
            if self.slow_video_encodes % 60 == 1 {
                tracing::debug!(
                    video_completion_ms = completion_ms,
                    frame_interval_ms,
                    slow_video_encodes = self.slow_video_encodes,
                    "Video completion used most of capture budget; retaining the latest capture"
                );
            }
        }
    }

    fn finish_bgra_update(
        &mut self,
        update: GfxFrameUpdate,
        started_at: Instant,
        dirty_rects: usize,
        dirty_pixels: u32,
    ) -> Option<DisplayUpdate> {
        let use_444 = update.h264_aux.is_some();
        self.note_video_encode_success();
        let encode_ms = started_at.elapsed().as_secs_f64() * 1000.0;
        let total_bytes =
            update.h264_data.len() + update.h264_aux.as_ref().map_or(0, |data| data.len());
        let is_keyframe = update.is_keyframe;
        tracing::debug!(
            display_frame = self.display_frame_count,
            use_444,
            h264_bytes = update.h264_data.len(),
            aux_bytes = update.h264_aux.as_ref().map_or(0, |data| data.len()),
            is_keyframe,
            encode_ms,
            "Display: sending BGRA GFX frame"
        );
        {
            let mut gfx = self.gfx_state.lock().unwrap();
            gfx.record_encode_timing(encode_ms);
            gfx.last_frame_bytes = total_bytes as u32;
        }
        self.bitrate_ctrl.record_frame(FrameStats {
            encode_ms,
            frame_bytes: total_bytes as u32,
            is_keyframe,
        });
        if let Some(stats) = &self.perf_stats {
            stats
                .lock()
                .unwrap()
                .record_frame(encode_ms, total_bytes as u32, is_keyframe);
        }
        let path = if use_444 {
            "gfx-avc444"
        } else {
            "gfx-avc-bgra"
        };
        self.record_avc_audit(
            total_bytes as u32,
            encode_ms,
            dirty_rects,
            dirty_pixels,
            path,
        );
        self.apply_adaptive_decision();
        self.note_video_encode_duration(encode_ms);
        if is_keyframe {
            self.last_idr_time = Instant::now();
            self.needs_full_gfx_seed = false;
        }
        self.clear_client_stale_flag(path);
        Some(DisplayUpdate::GfxFrame(update))
    }
}

#[cfg(test)]
mod tests;
