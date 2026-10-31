//! Bounded startup evidence that capture contains changing, visible pixels.
//!
//! At most 256 BGR samples from the previous observation stay in private memory
//! for comparison. They are replaced on each sample and cleared on expiry or an
//! unavailable/changed source. No pixels, positions, hashes or raw timestamps
//! enter diagnostics. Sparse changes are evidence, not proof of a correct desktop.
//! NV12 remains opaque: diagnostics never copy or convert its planes.

use macrdp_capture::{CapturedFrame, FrameData};
use std::time::{Duration, Instant};

const GRID_EDGE: u32 = 16;
const MAX_SAMPLE_POINTS: usize = (GRID_EDGE * GRID_EDGE) as usize;
const NEAR_BLACK_LUMA_MAX: u8 = 4;
const STARTUP_WINDOW: Duration = Duration::from_secs(60);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const MAX_OBSERVATIONS: u8 = 61;

pub(super) struct FrameHealth {
    started_at: Instant,
    last_observed_at: Option<Instant>,
    observations: u8,
    previous_sample: Option<Sample>,
    previous_timestamp_us: Option<u64>,
    previous_context: Option<SampleContext>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameSource {
    Raw,
    BgraBuffer,
    Nv12Buffer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SampleContext {
    display_id: u32,
    width: u32,
    height: u32,
    source: FrameSource,
}

impl FrameHealth {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            last_observed_at: None,
            observations: 0,
            previous_sample: None,
            previous_timestamp_us: None,
            previous_context: None,
        }
    }

    fn clear_comparison(&mut self) {
        self.previous_sample = None;
        self.previous_timestamp_us = None;
        self.previous_context = None;
    }

    fn take_observation(&mut self, now: Instant) -> Option<(&'static str, u64)> {
        let elapsed = now.saturating_duration_since(self.started_at);
        if elapsed > STARTUP_WINDOW || self.observations >= MAX_OBSERVATIONS {
            self.clear_comparison();
            if self.last_observed_at.is_some() {
                return None;
            }
        }
        let phase = if let Some(previous) = self.last_observed_at {
            if now.saturating_duration_since(previous) < SAMPLE_INTERVAL {
                return None;
            }
            "progress"
        } else {
            // A late first frame still gives one baseline; it does not reopen
            // the original sixty-second observation window.
            "baseline"
        };
        self.last_observed_at = Some(now);
        self.observations += 1;
        Some((phase, elapsed.as_millis().min(u128::from(u64::MAX)) as u64))
    }

    pub(super) fn observe(&mut self, frame: &CapturedFrame, display_id: u32, now: Instant) {
        let Some((phase, elapsed_ms)) = self.take_observation(now) else {
            return;
        };
        let (source, source_format, sample) = match &frame.data {
            FrameData::Raw(pixels) => (
                FrameSource::Raw,
                "bgra",
                sample_bgra(pixels, frame.width, frame.height, frame.stride),
            ),
            FrameData::BgraPixelBuffer(buffer) => (
                FrameSource::BgraBuffer,
                "bgra",
                buffer
                    .with_locked_bgra(frame.width, frame.height, |pixels, stride| {
                        sample_bgra(pixels, frame.width, frame.height, stride)
                    })
                    .flatten(),
            ),
            FrameData::PixelBuffer(_) => (FrameSource::Nv12Buffer, "nv12_unsampled", None),
        };
        let context = SampleContext {
            display_id,
            width: frame.width,
            height: frame.height,
            source,
        };
        let (comparison, timestamp) = self.compare_observation(context, sample, frame.timestamp_us);
        let summary = LogSummary::from(self.previous_sample.as_ref());
        // A missing/opaque sample must never look like an observed black or
        // unchanged image. Timestamp progress is independent of pixel access.
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
            comparison_available = comparison.available,
            compared_sample_points = comparison.compared,
            changed_sample_points = comparison.changed,
            timestamp_available = timestamp.available,
            timestamp_baseline_available = timestamp.baseline_available,
            timestamp_advanced = timestamp.advanced,
            timestamp_duplicate = timestamp.duplicate,
            timestamp_regressed = timestamp.regressed,
            "Capture startup pixel health"
        );
        if elapsed_ms >= STARTUP_WINDOW.as_millis() as u64 || self.observations >= MAX_OBSERVATIONS
        {
            self.clear_comparison();
        }
    }

    fn compare_observation(
        &mut self,
        context: SampleContext,
        sample: Option<Sample>,
        timestamp_us: u64,
    ) -> (Comparison, TimestampProgress) {
        // Raw fallback capture and native buffers use different clock domains.
        // A source/display/size change always establishes a fresh baseline.
        if self.previous_context != Some(context) {
            self.clear_comparison();
        }
        let comparison = compare_samples(self.previous_sample.as_ref(), sample.as_ref());
        let timestamp = TimestampProgress::new(self.previous_timestamp_us, timestamp_us);
        self.previous_context = Some(context);
        self.previous_sample = sample;
        self.previous_timestamp_us = (timestamp_us != 0).then_some(timestamp_us);
        (comparison, timestamp)
    }
}

// No Debug implementation: private comparison colors must never be formatted.
struct Sample {
    width: u32,
    height: u32,
    colors: [[u8; 3]; MAX_SAMPLE_POINTS],
    sampled_pixels: u16,
    near_black_pixels: u16,
    luma_min: u8,
    luma_max: u8,
    luma_sum: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct Comparison {
    available: bool,
    compared: u16,
    changed: i16,
}

fn compare_samples(previous: Option<&Sample>, current: Option<&Sample>) -> Comparison {
    if let (Some(previous), Some(current)) = (previous, current) {
        if previous.width == current.width
            && previous.height == current.height
            && previous.sampled_pixels == current.sampled_pixels
        {
            let count = usize::from(current.sampled_pixels);
            return Comparison {
                available: true,
                compared: current.sampled_pixels,
                changed: previous.colors[..count]
                    .iter()
                    .zip(&current.colors[..count])
                    .filter(|(before, after)| before != after)
                    .count() as i16,
            };
        }
    }
    Comparison {
        available: false,
        compared: 0,
        changed: -1,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TimestampProgress {
    available: bool,
    baseline_available: bool,
    advanced: bool,
    duplicate: bool,
    regressed: bool,
}

impl TimestampProgress {
    fn new(previous: Option<u64>, current: u64) -> Self {
        let available = current != 0;
        let baseline_available = available && previous.is_some();
        Self {
            available,
            baseline_available,
            advanced: baseline_available && previous.is_some_and(|before| current > before),
            duplicate: baseline_available && previous == Some(current),
            regressed: baseline_available && previous.is_some_and(|before| current < before),
        }
    }
}

struct LogSummary {
    available: bool,
    sampled_pixels: u16,
    near_black_pixels: u16,
    luma_min: i16,
    luma_max: i16,
    luma_mean: f64,
}

impl From<Option<&Sample>> for LogSummary {
    fn from(sample: Option<&Sample>) -> Self {
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
        width,
        height,
        colors: [[0; 3]; MAX_SAMPLE_POINTS],
        sampled_pixels: 0,
        near_black_pixels: 0,
        luma_min: u8::MAX,
        luma_max: 0,
        luma_sum: 0,
    };
    for row in 0..rows {
        // Sample each grid cell's center. u64 arithmetic covers u32::MAX;
        // only the at-most-256 pixels are read, independent of image size.
        let y = ((u64::from(row) * 2 + 1) * u64::from(height)) / (u64::from(rows) * 2);
        for column in 0..columns {
            let x = ((u64::from(column) * 2 + 1) * u64::from(width)) / (u64::from(columns) * 2);
            let offset = usize::try_from(y)
                .ok()?
                .checked_mul(stride)?
                .checked_add(usize::try_from(x).ok()?.checked_mul(4)?)?;
            let pixel = pixels.get(offset..offset.checked_add(4)?)?;
            // Approximate Rec.709 brightness; alpha may be unused. Keep BGR
            // separately so equal-brightness color changes are still counted.
            let luma = ((19 * u32::from(pixel[0])
                + 183 * u32::from(pixel[1])
                + 54 * u32::from(pixel[2])
                + 128)
                >> 8) as u8;
            sample.colors[usize::from(sample.sampled_pixels)].copy_from_slice(&pixel[..3]);
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
