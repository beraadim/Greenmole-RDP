//! Lazily encode complete bitmap partitions within the negotiated receive limit.

use anyhow::{anyhow, Result};
use core::num::NonZeroU16;
use ironrdp_graphics::diff::Rect;
use ironrdp_pdu::fast_path::UpdateCode;
use tracing::warn;

use super::{limits, UpdateEncoder, UpdateFragmenter};
use crate::{BitmapUpdate, DisplayUpdate};

#[derive(Debug, Default)]
pub(super) enum State {
    Start(DisplayUpdate),
    BitmapDiffs {
        diffs: Vec<Rect>,
        bitmap: BitmapUpdate,
        pos: usize,
        pending: Vec<Rect>,
    },
    #[default]
    Ended,
}

#[cfg_attr(feature = "__bench", visibility::make(pub))]
pub(crate) struct EncoderIter<'a> {
    pub(super) encoder: &'a mut UpdateEncoder,
    pub(super) state: State,
}

impl EncoderIter<'_> {
    pub(crate) fn begin_bitmap_frame(&mut self) -> Result<Option<UpdateFragmenter>> {
        self.encoder.begin_bitmap_frame()
    }

    pub(crate) fn end_bitmap_frame(&mut self) -> Result<Option<UpdateFragmenter>> {
        self.encoder.end_bitmap_frame()
    }

    /// Encode a cursor update between complete bitmap partitions without
    /// consuming pending damage or committing the framebuffer. The caller must
    /// finish each fragmenter before inserting another update on the wire.
    pub(crate) fn encode_pointer(&self, update: DisplayUpdate) -> Result<Option<UpdateFragmenter>> {
        let output = match update {
            DisplayUpdate::PointerPosition(pos) => UpdateEncoder::pointer_position(pos),
            DisplayUpdate::RGBAPointer(ptr) => UpdateEncoder::rgba_pointer(ptr),
            DisplayUpdate::ColorPointer(ptr) => UpdateEncoder::color_pointer(ptr),
            DisplayUpdate::HidePointer => UpdateEncoder::hide_pointer(),
            DisplayUpdate::DefaultPointer => UpdateEncoder::default_pointer(),
            _ => return Ok(None),
        }?;
        Ok(Some(limit_pointer(
            output,
            self.encoder.limits.max_payload(),
        )))
    }

    #[cfg_attr(feature = "__bench", visibility::make(pub))]
    pub(crate) async fn next(&mut self) -> Option<Result<UpdateFragmenter>> {
        loop {
            let state = core::mem::take(&mut self.state);
            let encoder = &mut self.encoder;

            let res = match state {
                State::Start(update) => match update {
                    DisplayUpdate::Bitmap(bitmap) => {
                        let diffs = encoder.bitmap_diffs(&bitmap);
                        self.state = State::BitmapDiffs {
                            diffs,
                            bitmap,
                            pos: 0,
                            pending: Vec::new(),
                        };
                        continue;
                    }
                    DisplayUpdate::PointerPosition(pos) => UpdateEncoder::pointer_position(pos),
                    DisplayUpdate::RGBAPointer(ptr) => UpdateEncoder::rgba_pointer(ptr),
                    DisplayUpdate::ColorPointer(ptr) => UpdateEncoder::color_pointer(ptr),
                    DisplayUpdate::HidePointer => UpdateEncoder::hide_pointer(),
                    DisplayUpdate::DefaultPointer => UpdateEncoder::default_pointer(),
                    DisplayUpdate::Resize(_) => return None,
                    DisplayUpdate::GfxFrame(_) | DisplayUpdate::GfxUncompressed(_) => {
                        // GFX frames are handled separately in the server loop, not through the encoder
                        return None;
                    }
                },
                State::BitmapDiffs {
                    diffs,
                    bitmap,
                    mut pos,
                    mut pending,
                } => {
                    let rect = if let Some(rect) = pending.pop() {
                        rect
                    } else if let Some(rect) = diffs.get(pos) {
                        pos += 1;
                        rect.clone()
                    } else {
                        encoder.bitmap_update_framebuffer(bitmap, &diffs);
                        self.state = State::Ended;
                        return None;
                    };
                    // Bound work/allocation before encoding. Exact output size is
                    // checked below, since noisy compressed pixels can expand.
                    if encoder.limits.should_partition(&rect) {
                        if let Some((first, second)) = limits::split_rect(&rect) {
                            pending.push(second);
                            pending.push(first);
                            self.state = State::BitmapDiffs {
                                diffs,
                                bitmap,
                                pos,
                                pending,
                            };
                            continue;
                        }
                    }
                    let Rect {
                        x,
                        y,
                        width,
                        height,
                    } = rect;

                    let x = match u16::try_from(x) {
                        Ok(x) => x,
                        Err(_) => {
                            return Some(Err(anyhow!(
                                "invalid `x`: out of range integral conversion"
                            )))
                        }
                    };
                    let y = match u16::try_from(y) {
                        Ok(y) => y,
                        Err(_) => {
                            return Some(Err(anyhow!(
                                "invalid `y`: out of range integral conversion"
                            )))
                        }
                    };
                    let width = match u16::try_from(width) {
                        Ok(width) => match NonZeroU16::new(width) {
                            Some(width) => width,
                            None => return Some(Err(anyhow!("rectangle width cannot be zero"))),
                        },
                        Err(_) => {
                            return Some(Err(anyhow!(
                                "invalid `width`: out of range integral conversion"
                            )))
                        }
                    };
                    let height = match u16::try_from(height) {
                        Ok(height) => match NonZeroU16::new(height) {
                            Some(height) => height,
                            None => return Some(Err(anyhow!("rectangle height cannot be zero"))),
                        },
                        Err(_) => {
                            return Some(Err(anyhow!(
                                "invalid `height`: out of range integral conversion"
                            )))
                        }
                    };

                    let Some(sub) = bitmap.sub(x, y, width, height) else {
                        warn!("Failed to extract bitmap subregion");
                        return None;
                    };
                    match encoder.bitmap(sub).await {
                        Ok(Some(output)) => {
                            self.state = State::BitmapDiffs {
                                diffs,
                                bitmap,
                                pos,
                                pending,
                            };
                            Ok(output)
                        }
                        Ok(None) => {
                            let rect = Rect {
                                x: usize::from(x),
                                y: usize::from(y),
                                width: usize::from(width.get()),
                                height: usize::from(height.get()),
                            };
                            let Some((first, second)) = limits::split_rect(&rect) else {
                                return Some(Err(anyhow!(
                                    "client Fast-Path update limit {} cannot fit one encoded pixel",
                                    encoder.limits.bitmap_budget()
                                )));
                            };
                            pending.push(second);
                            pending.push(first);
                            self.state = State::BitmapDiffs {
                                diffs,
                                bitmap,
                                pos,
                                pending,
                            };
                            continue;
                        }
                        Err(error) => Err(error),
                    }
                }
                State::Ended => return None,
            };

            return Some(res.map(|output| limit_pointer(output, encoder.limits.max_payload())));
        }
    }
}

fn limit_pointer(output: UpdateFragmenter, limit: usize) -> UpdateFragmenter {
    // A pointer cannot be spatially partitioned. Use the client's system
    // pointer when its negotiated receive limit cannot fit our shape.
    if output.data.len() > limit {
        warn!(
            bytes = output.data.len(),
            limit, "Pointer shape exceeds client update limit; using system pointer"
        );
        UpdateFragmenter::new(UpdateCode::DefaultPointer, Vec::new())
    } else {
        output
    }
}
