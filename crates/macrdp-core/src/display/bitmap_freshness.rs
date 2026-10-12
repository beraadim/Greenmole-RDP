//! Refresh pending regions without discarding the newer frame's damage.

use macrdp_capture::CapturedFrame;

const MAX_DEFERRED_DAMAGE_RECTS: usize = 128;

pub(super) fn accumulate_frame(latest: &mut CapturedFrame, previous: &CapturedFrame) {
    if latest.width != previous.width
        || latest.height != previous.height
        || latest.dirty_rects.is_empty()
        || previous.dirty_rects.is_empty()
        || latest.dirty_rects.len() + previous.dirty_rects.len() > MAX_DEFERRED_DAMAGE_RECTS
    {
        latest.dirty_rects.clear();
    } else {
        latest
            .dirty_rects
            .extend(previous.dirty_rects.iter().cloned());
    }
}

#[cfg(test)]
mod tests {
    use super::super::{bitmap_updates, damage, ClampedRect};
    use super::*;
    use bytes::Bytes;
    use ironrdp_server::BitmapUpdate;
    use macrdp_capture::FrameData;

    fn refresh_region(bitmap: &mut BitmapUpdate, frame: &CapturedFrame) -> bool {
        let region = bitmap_updates::Region {
            bounds: ClampedRect {
                x: u32::from(bitmap.x),
                y: u32::from(bitmap.y),
                width: u32::from(bitmap.width.get()),
                height: u32::from(bitmap.height.get()),
            },
            dirty_rects: bitmap.dirty_rects.clone(),
        };
        let Some(materialized) = bitmap_updates::materialize(region, frame, true) else {
            return false;
        };
        *bitmap = materialized.update;
        true
    }
    use ironrdp_server::{DiffRect, PixelFormat};
    use macrdp_capture::Rect;
    use std::num::{NonZeroU16, NonZeroUsize};

    fn frame(value: u8, x: u32) -> CapturedFrame {
        CapturedFrame {
            width: 4,
            height: 2,
            stride: 20,
            data: FrameData::Raw(Bytes::from(vec![value; 40])),
            timestamp_us: 0,
            dirty_rects: vec![Rect {
                x,
                y: 0,
                width: 1,
                height: 1,
            }],
            content_rect: None,
        }
    }

    #[test]
    fn refreshes_queued_pixels_and_retains_all_intervening_damage() {
        let previous = frame(1, 3);
        let mut latest = frame(2, 0);
        accumulate_frame(&mut latest, &previous);
        let mut queued = BitmapUpdate {
            x: 2,
            y: 0,
            width: NonZeroU16::new(2).unwrap(),
            height: NonZeroU16::new(2).unwrap(),
            format: PixelFormat::BgrA32,
            data: Bytes::from(vec![1; 16]),
            stride: NonZeroUsize::new(8).unwrap(),
            dirty_rects: vec![DiffRect {
                x: 2,
                y: 0,
                width: 1,
                height: 1,
            }],
        };
        assert!(refresh_region(&mut queued, &latest));
        assert_eq!(&queued.data[..8], &[2; 8]);
        assert_eq!(&queued.data[20..28], &[2; 8]);
        assert_eq!(queued.stride.get(), 20);
        assert!(queued.dirty_rects.is_empty());
        assert_eq!(
            latest.dirty_rects.iter().map(|r| r.x).collect::<Vec<_>>(),
            vec![0, 3]
        );
        latest.width = 2;
        assert!(!refresh_region(&mut queued, &latest));
    }

    #[test]
    fn long_stalls_unknown_damage_and_resizes_require_full_refresh() {
        let mut latest = frame(2, 0);
        let mut previous = frame(1, 1);
        previous.dirty_rects = vec![previous.dirty_rects[0].clone(); 128];
        accumulate_frame(&mut latest, &previous);
        assert!(latest.dirty_rects.is_empty());
        let mut latest = frame(2, 0);
        previous = frame(1, 1);
        previous.width = 5;
        accumulate_frame(&mut latest, &previous);
        assert!(latest.dirty_rects.is_empty());
    }

    #[test]
    fn completing_old_regions_then_accumulated_damage_converges_to_latest_pixels() {
        let make_frame = |pixels: &[u8; 6], changed: &[u32]| CapturedFrame {
            width: 6,
            height: 1,
            stride: 24,
            data: FrameData::Raw(Bytes::from(
                pixels.iter().flat_map(|v| [*v; 4]).collect::<Vec<_>>(),
            )),
            timestamp_us: 0,
            content_rect: None,
            dirty_rects: changed
                .iter()
                .map(|&x| Rect {
                    x,
                    y: 0,
                    width: 1,
                    height: 1,
                })
                .collect(),
        };
        let mut client = vec![1u8; 24];
        let mut queued = BitmapUpdate {
            x: 2,
            y: 0,
            width: NonZeroU16::new(2).unwrap(),
            height: NonZeroU16::new(1).unwrap(),
            format: PixelFormat::BgrA32,
            data: Bytes::from(vec![1; 8]),
            stride: NonZeroUsize::new(8).unwrap(),
            dirty_rects: vec![],
        };
        let first = make_frame(&[2, 1, 3, 1, 1, 1], &[0, 2]);
        assert!(refresh_region(&mut queued, &first));
        client[8..16].copy_from_slice(&queued.data);
        let mut latest = make_frame(&[2, 4, 5, 1, 1, 6], &[1, 2, 5]);
        accumulate_frame(&mut latest, &first);
        queued.x = 4;
        assert!(refresh_region(&mut queued, &latest));
        client[16..24].copy_from_slice(&queued.data);
        let FrameData::Raw(expected) = &latest.data else {
            unreachable!()
        };
        for rect in &latest.dirty_rects {
            let start = rect.x as usize * 4;
            client[start..start + 4].copy_from_slice(&expected[start..start + 4]);
        }
        assert_eq!(client, expected.as_ref());
    }

    #[test]
    fn intermediate_pixels_that_revert_are_not_removed_by_capture_refinement() {
        use super::super::CachedFrame;
        let make_frame = |pixels: &[u8; 6], changed: &[u32]| CapturedFrame {
            width: 6,
            height: 1,
            stride: 24,
            data: FrameData::Raw(Bytes::from(
                pixels.iter().flat_map(|v| [*v; 4]).collect::<Vec<_>>(),
            )),
            timestamp_us: 0,
            content_rect: None,
            dirty_rects: changed
                .iter()
                .map(|&x| Rect {
                    x,
                    y: 0,
                    width: 1,
                    height: 1,
                })
                .collect(),
        };
        // The cache remains A while the client receives region X from B.
        let baseline = CachedFrame::Bgra {
            data: Bytes::from(vec![1; 24]),
            width: 6,
            height: 1,
            stride: 24,
        };
        let mut client = vec![1u8; 24];
        let intermediate = make_frame(&[1, 1, 3, 1, 1, 1], &[2]);
        let mut queued = BitmapUpdate {
            x: 2,
            y: 0,
            width: NonZeroU16::new(2).unwrap(),
            height: NonZeroU16::new(1).unwrap(),
            format: PixelFormat::BgrA32,
            data: Bytes::from(vec![1; 8]),
            stride: NonZeroUsize::new(8).unwrap(),
            dirty_rects: vec![],
        };
        assert!(refresh_region(&mut queued, &intermediate));
        client[8..16].copy_from_slice(&queued.data);
        // C restores X to A, but independently changes Y. Comparing C with A
        // must not erase X from the accumulated damage still owed to the client.
        let mut latest = make_frame(&[1, 1, 1, 1, 5, 1], &[2, 4]);
        accumulate_frame(&mut latest, &intermediate);
        damage::refine_capture_dirty_rects(&mut latest, Some(&baseline), true);
        assert!(latest.dirty_rects.iter().any(|rect| rect.x == 2));
        let FrameData::Raw(expected) = &latest.data else {
            unreachable!()
        };
        for rect in &latest.dirty_rects {
            let start = rect.x as usize * 4;
            client[start..start + 4].copy_from_slice(&expected[start..start + 4]);
        }
        assert_eq!(client, expected.as_ref());
    }
}
