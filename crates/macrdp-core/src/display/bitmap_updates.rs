//! Queue geometry, then materialize each region from the newest available image.
//!
//! Raw captures retain their `Bytes` allocation with a checked stride slice.
//! Native pixel buffers are locked only for the region being dispatched; queued
//! regions neither copy pixels eagerly nor keep an older copy to refresh later.

use std::collections::VecDeque;
use std::num::{NonZeroU16, NonZeroUsize};

use bytes::Bytes;
use ironrdp_server::{BitmapUpdate, DiffRect, PixelFormat};
use macrdp_capture::{CapturedFrame, FrameData};

use super::{prepare_bitmap_dirty_regions, ClampedRect};

pub(super) struct Region {
    pub(super) bounds: ClampedRect,
    pub(super) dirty_rects: Vec<DiffRect>,
}

pub(super) struct Materialized {
    pub(super) update: BitmapUpdate,
    copied_bytes: u64,
}

/// Scalar, saturating counters keep diagnosis bounded for long sessions.
#[derive(Default)]
pub(super) struct CopyStats {
    regions: u64,
    copied_bytes: u64,
    shared_bytes: u64,
}

impl CopyStats {
    pub(super) fn record(&mut self, materialized: &Materialized) {
        self.regions = self.regions.saturating_add(1);
        self.copied_bytes = self.copied_bytes.saturating_add(materialized.copied_bytes);
        if materialized.copied_bytes == 0 {
            let update = &materialized.update;
            self.shared_bytes = self
                .shared_bytes
                .saturating_add(u64::from(update.width.get()) * u64::from(update.height.get()) * 4);
        }
    }

    pub(super) fn log_summary(&self) {
        tracing::info!(
            regions = self.regions,
            copied_bytes = self.copied_bytes,
            shared_bytes = self.shared_bytes,
            "Bitmap pixel materialization summary"
        );
    }
}

/// Plans at most the bounded capture-damage list; this does not read pixels.
pub(super) fn plan(frame: &CapturedFrame, force_full: bool) -> (VecDeque<Region>, bool) {
    let regions = if force_full {
        Vec::new()
    } else {
        prepare_bitmap_dirty_regions(frame.width, frame.height, &frame.dirty_rects)
    };
    if regions.is_empty() {
        return (
            VecDeque::from([Region {
                bounds: ClampedRect {
                    x: 0,
                    y: 0,
                    width: frame.width,
                    height: frame.height,
                },
                dirty_rects: Vec::new(),
            }]),
            true,
        );
    }
    let planned = regions
        .into_iter()
        .map(|bounds| {
            let dirty_rects = frame
                .dirty_rects
                .iter()
                .filter_map(|rect| {
                    let left = rect.x.max(bounds.x);
                    let top = rect.y.max(bounds.y);
                    let right = rect
                        .x
                        .saturating_add(rect.width)
                        .min(bounds.x + bounds.width);
                    let bottom = rect
                        .y
                        .saturating_add(rect.height)
                        .min(bounds.y + bounds.height);
                    (right > left && bottom > top).then(|| DiffRect {
                        x: left as usize,
                        y: top as usize,
                        width: (right - left) as usize,
                        height: (bottom - top) as usize,
                    })
                })
                .collect();
            Region {
                bounds,
                dirty_rects,
            }
        })
        .collect();
    (planned, false)
}

struct PixelLayout {
    start: usize,
    end: usize,
    row_bytes: usize,
    height: usize,
}

fn checked_layout(
    data_len: usize,
    stride: usize,
    frame_width: u32,
    frame_height: u32,
    bounds: ClampedRect,
) -> Option<PixelLayout> {
    if bounds.width == 0
        || bounds.height == 0
        || bounds.x.checked_add(bounds.width)? > frame_width
        || bounds.y.checked_add(bounds.height)? > frame_height
        || stride < usize::try_from(frame_width).ok()?.checked_mul(4)?
    {
        return None;
    }
    let x = usize::try_from(bounds.x).ok()?;
    let y = usize::try_from(bounds.y).ok()?;
    let row_bytes = usize::try_from(bounds.width).ok()?.checked_mul(4)?;
    let height = usize::try_from(bounds.height).ok()?;
    let start = y.checked_mul(stride)?.checked_add(x.checked_mul(4)?)?;
    let end = start
        .checked_add((height - 1).checked_mul(stride)?)?
        .checked_add(row_bytes)?;
    (end <= data_len).then_some(PixelLayout {
        start,
        end,
        row_bytes,
        height,
    })
}

fn copy_region(data: &[u8], stride: usize, layout: &PixelLayout) -> Option<Bytes> {
    let mut pixels = Vec::with_capacity(layout.row_bytes.checked_mul(layout.height)?);
    for row in 0..layout.height {
        let start = layout.start + row * stride;
        pixels.extend_from_slice(&data[start..start + layout.row_bytes]);
    }
    Some(Bytes::from(pixels))
}

pub(super) fn materialize(
    region: Region,
    frame: &CapturedFrame,
    refreshed: bool,
) -> Option<Materialized> {
    let bounds = region.bounds;
    let width = NonZeroU16::new(u16::try_from(bounds.width).ok()?)?;
    let height = NonZeroU16::new(u16::try_from(bounds.height).ok()?)?;
    let (data, stride, copied_bytes) = match &frame.data {
        FrameData::Raw(data) => {
            let layout =
                checked_layout(data.len(), frame.stride, frame.width, frame.height, bounds)?;
            (data.slice(layout.start..layout.end), frame.stride, 0)
        }
        FrameData::BgraPixelBuffer(buffer) => buffer
            .with_locked_bgra(frame.width, frame.height, |data, stride| {
                let layout = checked_layout(data.len(), stride, frame.width, frame.height, bounds)?;
                let pixels = copy_region(data, stride, &layout)?;
                let copied_bytes = pixels.len() as u64;
                Some((pixels, layout.row_bytes, copied_bytes))
            })
            .flatten()?,
        FrameData::PixelBuffer(_) => return None,
    };
    Some(Materialized {
        update: BitmapUpdate {
            x: u16::try_from(bounds.x).ok()?,
            y: u16::try_from(bounds.y).ok()?,
            width,
            height,
            format: PixelFormat::BgrA32,
            data,
            stride: NonZeroUsize::new(stride)?,
            // The newest image may have changed outside the original hints.
            // The wire encoder's framebuffer must compare the entire region.
            dirty_rects: if refreshed {
                Vec::new()
            } else {
                region.dirty_rects
            },
        },
        copied_bytes,
    })
}

#[cfg(test)]
mod tests;
