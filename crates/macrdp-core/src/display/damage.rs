//! Keep missing capture damage distinct from pixels verified unchanged.

use super::{CachedFrame, ClampedRect, DIRTY_RECT_VALIDATION_TILE};
use macrdp_capture::{CapturedFrame, FrameData, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FrameDamage {
    /// Capture did not provide reliable damage information.
    Unknown,
    /// Pixel comparison proved every supplied damaged region unchanged.
    Unchanged,
    /// Damage hints still need to be delivered, including accumulated changes.
    Regions,
}

impl FrameDamage {
    pub(super) fn can_skip(self, refresh_required: bool) -> bool {
        self == Self::Unchanged && !refresh_required
    }
}

pub(super) fn refine_capture_dirty_rects(
    frame: &mut CapturedFrame,
    previous: Option<&CachedFrame>,
    preserve_accumulated_damage: bool,
) -> FrameDamage {
    // Refreshed queued regions may already contain an intermediate image.
    // Comparing their accumulated damage with the older capture cache can
    // suppress a pixel that changed and reverted. Only the wire encoder's
    // framebuffer tracks those partial deliveries accurately.
    if frame.dirty_rects.is_empty() {
        return FrameDamage::Unknown;
    }
    if preserve_accumulated_damage {
        return FrameDamage::Regions;
    }

    let previous = match previous {
        Some(previous) => previous,
        None => return FrameDamage::Regions,
    };

    let original_count = frame.dirty_rects.len();

    let refined = match (&frame.data, previous) {
        (
            FrameData::Raw(current),
            CachedFrame::Bgra {
                data: prev,
                stride,
                width,
                height,
            },
        ) if *width == frame.width && *height == frame.height => {
            validate_dirty_rects_against_previous_bgra(
                frame.width,
                frame.height,
                &frame.dirty_rects,
                current,
                frame.stride,
                prev,
                *stride,
            )
        }
        (FrameData::Raw(current), CachedFrame::BgraPixelBuffer(prev, width, height, _))
            if *width == frame.width && *height == frame.height =>
        {
            prev.with_locked_bgra(frame.width, frame.height, |prev_bgra, prev_stride| {
                validate_dirty_rects_against_previous_bgra(
                    frame.width,
                    frame.height,
                    &frame.dirty_rects,
                    current,
                    frame.stride,
                    prev_bgra,
                    prev_stride,
                )
            })
            .unwrap_or_else(|| frame.dirty_rects.clone())
        }
        (
            FrameData::BgraPixelBuffer(current),
            CachedFrame::Bgra {
                data: prev,
                stride,
                width,
                height,
            },
        ) if *width == frame.width && *height == frame.height => current
            .with_locked_bgra(frame.width, frame.height, |current_bgra, current_stride| {
                validate_dirty_rects_against_previous_bgra(
                    frame.width,
                    frame.height,
                    &frame.dirty_rects,
                    current_bgra,
                    current_stride,
                    prev,
                    *stride,
                )
            })
            .unwrap_or_else(|| frame.dirty_rects.clone()),
        (
            FrameData::BgraPixelBuffer(current),
            CachedFrame::BgraPixelBuffer(prev, width, height, _),
        ) if *width == frame.width && *height == frame.height => current
            .with_locked_bgra(frame.width, frame.height, |current_bgra, current_stride| {
                prev.with_locked_bgra(frame.width, frame.height, |prev_bgra, prev_stride| {
                    validate_dirty_rects_against_previous_bgra(
                        frame.width,
                        frame.height,
                        &frame.dirty_rects,
                        current_bgra,
                        current_stride,
                        prev_bgra,
                        prev_stride,
                    )
                })
                .unwrap_or_else(|| frame.dirty_rects.clone())
            })
            .unwrap_or_else(|| frame.dirty_rects.clone()),
        _ => return FrameDamage::Regions,
    };

    if refined.len() != original_count {
        tracing::debug!(
            original_dirty_rects = original_count,
            refined_dirty_rects = refined.len(),
            "Refined capture dirty rects against previous frame"
        );
    }

    frame.dirty_rects = refined;
    if frame.dirty_rects.is_empty() {
        FrameDamage::Unchanged
    } else {
        FrameDamage::Regions
    }
}

fn bgra_rect_changed(
    current: &[u8],
    current_stride: usize,
    previous: &[u8],
    previous_stride: usize,
    rect: ClampedRect,
) -> bool {
    let row_bytes = rect.width as usize * 4;

    for row in 0..rect.height as usize {
        let y = rect.y as usize + row;
        let current_offset = y * current_stride + rect.x as usize * 4;
        let previous_offset = y * previous_stride + rect.x as usize * 4;
        let current_end = current_offset + row_bytes;
        let previous_end = previous_offset + row_bytes;

        if current_end > current.len() || previous_end > previous.len() {
            return true;
        }

        if current[current_offset..current_end] != previous[previous_offset..previous_end] {
            return true;
        }
    }

    false
}

pub(super) fn validate_dirty_rects_against_previous_bgra(
    frame_width: u32,
    frame_height: u32,
    dirty_rects: &[Rect],
    current: &[u8],
    current_stride: usize,
    previous: &[u8],
    previous_stride: usize,
) -> Vec<Rect> {
    // An invalid layout or hint cannot establish that pixels are unchanged.
    // Keep its conservative damage semantics instead of producing an empty
    // proof from out-of-bounds/zero-area rectangles.
    let complete_layout = |data: &[u8], stride: usize| {
        let Some(row_bytes) = (frame_width as usize).checked_mul(4) else {
            return false;
        };
        frame_width != 0
            && frame_height != 0
            && stride >= row_bytes
            && (frame_height as usize - 1)
                .checked_mul(stride)
                .and_then(|offset| offset.checked_add(row_bytes))
                .is_some_and(|end| end <= data.len())
    };
    if !complete_layout(current, current_stride)
        || !complete_layout(previous, previous_stride)
        || dirty_rects.iter().any(|rect| {
            rect.width == 0 || rect.height == 0 || rect.x >= frame_width || rect.y >= frame_height
        })
    {
        return dirty_rects.to_vec();
    }
    let mut refined = Vec::new();

    for rect in dirty_rects.iter().filter(|r| r.width > 0 && r.height > 0) {
        let x = rect.x.min(frame_width.saturating_sub(1));
        let y = rect.y.min(frame_height.saturating_sub(1));
        let width = rect.width.min(frame_width - x);
        let height = rect.height.min(frame_height - y);
        if width == 0 || height == 0 {
            continue;
        }

        let clamped = ClampedRect {
            x,
            y,
            width,
            height,
        };

        if width <= DIRTY_RECT_VALIDATION_TILE && height <= DIRTY_RECT_VALIDATION_TILE {
            if bgra_rect_changed(current, current_stride, previous, previous_stride, clamped) {
                refined.push(Rect {
                    x,
                    y,
                    width,
                    height,
                });
            }
            continue;
        }

        for tile_y in (y..y + height).step_by(DIRTY_RECT_VALIDATION_TILE as usize) {
            let tile_height = DIRTY_RECT_VALIDATION_TILE.min(y + height - tile_y);
            for tile_x in (x..x + width).step_by(DIRTY_RECT_VALIDATION_TILE as usize) {
                let tile_width = DIRTY_RECT_VALIDATION_TILE.min(x + width - tile_x);
                let tile = ClampedRect {
                    x: tile_x,
                    y: tile_y,
                    width: tile_width,
                    height: tile_height,
                };

                if bgra_rect_changed(current, current_stride, previous, previous_stride, tile) {
                    refined.push(Rect {
                        x: tile_x,
                        y: tile_y,
                        width: tile_width,
                        height: tile_height,
                    });
                }
            }
        }
    }

    refined
}

#[cfg(test)]
mod tests;
