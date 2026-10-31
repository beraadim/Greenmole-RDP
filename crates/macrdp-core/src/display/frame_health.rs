//! Bounded startup evidence that capture contains visible pixels.
//!
//! The sample is a coarse brightness summary, not proof of a correct desktop.
//! Never retain pixels, their positions, an image fingerprint, or frame data.
//! NV12 remains opaque so diagnostics cannot introduce a plane copy/conversion.

use macrdp_capture::{CapturedFrame, FrameData};
use std::time::{Duration, Instant};

const GRID_EDGE: u32 = 16;
const NEAR_BLACK_LUMA_MAX: u8 = 4;
const STARTUP_WINDOW: Duration = Duration::from_secs(5);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct FrameHealth {
    started_at: Instant,
    last_observed_at: Option<Instant>,
}

impl FrameHealth {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            last_observed_at: None,
        }
    }

    fn take_observation(&mut self, now: Instant) -> Option<(&'static str, u64)> {
        let elapsed = now.saturating_duration_since(self.started_at);
        let phase = if let Some(previous) = self.last_observed_at {
            if elapsed > STARTUP_WINDOW || now.saturating_duration_since(previous) < SAMPLE_INTERVAL
            {
                return None;
            }
            "progress"
        } else {
            // A delayed first frame is still useful evidence. Subsequent
            // observations remain limited to the original startup window.
            "baseline"
        };
        self.last_observed_at = Some(now);
        Some((phase, elapsed.as_millis().min(u128::from(u64::MAX)) as u64))
    }

    pub(super) fn observe(&mut self, frame: &CapturedFrame, display_id: u32, now: Instant) {
        let Some((phase, elapsed_ms)) = self.take_observation(now) else {
            return;
        };
        let (source_format, sample) = match &frame.data {
            FrameData::Raw(pixels) => (
                "bgra",
                sample_bgra(pixels, frame.width, frame.height, frame.stride),
            ),
            FrameData::BgraPixelBuffer(buffer) => (
                "bgra",
                buffer
                    .with_locked_bgra(frame.width, frame.height, |pixels, stride| {
                        sample_bgra(pixels, frame.width, frame.height, stride)
                    })
                    .flatten(),
            ),
            FrameData::PixelBuffer(_) => ("nv12_unsampled", None),
        };
        let summary = LogSummary::from(sample);
        // Keep the persisted record explicitly numeric, with fixed labels.
        // A missing sample must never look like an observed black image.
        tracing::info!(target: "macrdp_transition",
            component = "capture_health", phase, state = "observed",
            worker_pid = std::process::id(), display_id, elapsed_ms,
            width = frame.width, height = frame.height, source_format,
            sample_available = summary.available,
            sampled_pixels = summary.sampled_pixels,
            near_black_pixels = summary.near_black_pixels,
            near_black_luma_max = NEAR_BLACK_LUMA_MAX,
            luma_min = summary.luma_min, luma_max = summary.luma_max,
            luma_mean = summary.luma_mean,
            "Capture startup pixel health"
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    sampled_pixels: u16,
    near_black_pixels: u16,
    luma_min: u8,
    luma_max: u8,
    luma_sum: u32,
}

struct LogSummary {
    available: bool,
    sampled_pixels: u16,
    near_black_pixels: u16,
    luma_min: i16,
    luma_max: i16,
    luma_mean: f64,
}

impl From<Option<Sample>> for LogSummary {
    fn from(sample: Option<Sample>) -> Self {
        match sample {
            Some(sample) => Self {
                available: true,
                sampled_pixels: sample.sampled_pixels,
                near_black_pixels: sample.near_black_pixels,
                luma_min: i16::from(sample.luma_min),
                luma_max: i16::from(sample.luma_max),
                luma_mean: f64::from(sample.luma_sum) / f64::from(sample.sampled_pixels),
            },
            None => Self {
                available: false,
                sampled_pixels: 0,
                near_black_pixels: 0,
                luma_min: -1,
                luma_max: -1,
                luma_mean: -1.0,
            },
        }
    }
}

fn sample_bgra(pixels: &[u8], width: u32, height: u32, stride: usize) -> Option<Sample> {
    let width_usize = usize::try_from(width).ok()?;
    let height_usize = usize::try_from(height).ok()?;
    let row_bytes = width_usize.checked_mul(4)?;
    let required_len = height_usize
        .checked_sub(1)?
        .checked_mul(stride)?
        .checked_add(row_bytes)?;
    if width == 0 || stride < row_bytes || pixels.len() < required_len {
        return None;
    }
    let columns = width.min(GRID_EDGE);
    let rows = height.min(GRID_EDGE);
    let mut sample = Sample {
        sampled_pixels: 0,
        near_black_pixels: 0,
        luma_min: u8::MAX,
        luma_max: 0,
        luma_sum: 0,
    };
    for row in 0..rows {
        // Sample each grid cell's center. u64 arithmetic also covers u32::MAX
        // dimensions without overflow; only the at-most-256 pixels are read.
        let y = ((u64::from(row) * 2 + 1) * u64::from(height)) / (u64::from(rows) * 2);
        for column in 0..columns {
            let x = ((u64::from(column) * 2 + 1) * u64::from(width)) / (u64::from(columns) * 2);
            let offset = usize::try_from(y)
                .ok()?
                .checked_mul(stride)?
                .checked_add(usize::try_from(x).ok()?.checked_mul(4)?)?;
            let pixel = pixels.get(offset..offset.checked_add(4)?)?;
            // Approximate Rec.709 brightness in the original 8-bit range.
            // Alpha is deliberately ignored: desktop BGRA alpha may be unused.
            let luma = ((19 * u32::from(pixel[0])
                + 183 * u32::from(pixel[1])
                + 54 * u32::from(pixel[2])
                + 128)
                >> 8) as u8;
            sample.sampled_pixels += 1;
            sample.near_black_pixels += u16::from(luma <= NEAR_BLACK_LUMA_MAX);
            sample.luma_min = sample.luma_min.min(luma);
            sample.luma_max = sample.luma_max.max(luma);
            sample.luma_sum += u32::from(luma);
        }
    }
    Some(sample)
}

#[cfg(test)]
#[path = "frame_health_tests.rs"]
mod tests;
