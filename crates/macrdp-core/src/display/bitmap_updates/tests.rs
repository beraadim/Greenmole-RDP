use super::*;
use macrdp_capture::Rect;

fn frame(width: u32, height: u32, stride: usize) -> CapturedFrame {
    CapturedFrame {
        width,
        height,
        stride,
        data: FrameData::Raw(Bytes::from(vec![7; stride * height as usize])),
        timestamp_us: 0,
        content_rect: None,
        dirty_rects: Vec::new(),
    }
}

fn region(x: u32, y: u32, width: u32, height: u32) -> Region {
    Region {
        bounds: ClampedRect {
            x,
            y,
            width,
            height,
        },
        dirty_rects: Vec::new(),
    }
}

#[test]
fn raw_regions_share_checked_stride_slices_without_copying() {
    let capture = frame(4, 3, 20);
    let FrameData::Raw(source) = &capture.data else {
        unreachable!()
    };
    let result = materialize(region(1, 1, 2, 2), &capture, false).unwrap();
    assert_eq!(result.update.data.as_ptr(), source[24..].as_ptr());
    assert_eq!(result.update.data.len(), 28); // no padding after the last row
    assert_eq!(result.update.stride.get(), 20);
    assert_eq!(result.copied_bytes, 0);
    let mut stats = CopyStats::default();
    stats.record(&result);
    assert_eq!(
        (stats.regions, stats.copied_bytes, stats.shared_bytes),
        (1, 0, 16)
    );
    // Bytes ownership remains valid after the capture and original owner drop.
    drop(capture);
    assert_eq!(&result.update.data[20..28], &[7; 8]);
}

#[test]
fn native_region_copy_omits_padding_and_copies_only_dispatched_pixels() {
    let mut data = vec![99; 60];
    data[24..32].fill(1);
    data[44..52].fill(2);
    let layout = checked_layout(data.len(), 20, 4, 3, region(1, 1, 2, 2).bounds).unwrap();
    let pixels = copy_region(&data, 20, &layout).unwrap();
    assert_eq!(pixels.len(), 16);
    assert_eq!(
        pixels.as_ref(),
        &[1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2]
    );
}

#[test]
fn malformed_strides_bounds_and_truncated_last_rows_are_rejected() {
    let mut capture = frame(4, 3, 20);
    for invalid in [
        region(4, 0, 1, 1),
        region(0, 3, 1, 1),
        region(0, 0, 0, 1),
        region(u32::MAX, 0, 2, 1),
        region(0, 0, u32::MAX, 1),
    ] {
        assert!(materialize(invalid, &capture, false).is_none());
    }
    capture.stride = 12;
    assert!(materialize(region(0, 0, 1, 1), &capture, false).is_none());
    capture.stride = 20;
    capture.data = FrameData::Raw(Bytes::from(vec![7; 51]));
    assert!(materialize(region(1, 1, 2, 2), &capture, false).is_none());
    // Exactly the visible last pixel is enough; trailing padding is optional.
    capture.data = FrameData::Raw(Bytes::from(vec![7; 52]));
    assert!(materialize(region(1, 1, 2, 2), &capture, false).is_some());
}

#[test]
fn planning_is_lazy_and_refresh_uses_newest_pixels_without_old_hints() {
    let mut original = frame(1000, 2, 4000);
    original.dirty_rects = vec![
        Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        },
        Rect {
            x: 998,
            y: 0,
            width: 2,
            height: 1,
        },
    ];
    let (mut queued, full) = plan(&original, false);
    assert!(!full);
    assert_eq!(queued.len(), 2);
    let mut stats = CopyStats::default();
    assert_eq!(stats.regions, 0);
    let first = materialize(queued.pop_front().unwrap(), &original, false).unwrap();
    assert_eq!(first.update.dirty_rects.len(), 1);
    stats.record(&first);

    let mut latest = frame(1000, 2, 4000);
    latest.data = FrameData::Raw(Bytes::from(vec![3; 8000]));
    let newest = materialize(queued.pop_front().unwrap(), &latest, true).unwrap();
    assert_eq!(newest.update.data.as_ref(), &[3; 8]);
    assert!(newest.update.dirty_rects.is_empty());
    stats.record(&newest);
    assert!(queued.is_empty());
    // The old eager+refresh path copied 16 initial bytes plus 8 refreshed bytes.
    // These exact dispatches now share 16 bytes and copy none.
    assert_eq!(
        (stats.regions, stats.copied_bytes, stats.shared_bytes),
        (2, 0, 16)
    );
}

#[test]
fn forced_seed_and_unknown_damage_materialize_the_complete_frame() {
    let mut capture = frame(4, 2, 20);
    capture.dirty_rects.push(Rect {
        x: 1,
        y: 0,
        width: 1,
        height: 1,
    });
    for force in [true, false] {
        if !force {
            capture.dirty_rects.clear();
        }
        let (mut regions, full) = plan(&capture, force);
        assert!(full);
        assert_eq!(regions.len(), 1);
        let update = materialize(regions.pop_front().unwrap(), &capture, false)
            .unwrap()
            .update;
        assert_eq!(
            (update.x, update.y, update.width.get(), update.height.get()),
            (0, 0, 4, 2)
        );
        assert!(update.dirty_rects.is_empty());
        assert_eq!(update.data.len(), 36);
    }
}

/// Isolates pixel-copy work from capture/codec/network scheduling. Wall-clock
/// numbers are descriptive; only bytes copied are deterministic assertions.
#[test]
#[ignore = "optional bounded bitmap-copy microbenchmark"]
fn bitmap_materialization_work_microbenchmark() {
    use std::hint::black_box;
    use std::time::Instant;

    const BATCHES: usize = 100;
    const REGIONS: usize = 12;
    let capture = frame(1280, 720, 5120);
    let FrameData::Raw(data) = &capture.data else {
        unreachable!()
    };
    let layouts: Vec<_> = (0..REGIONS)
        .map(|index| {
            let bounds = region((index as u32 % 4) * 300, (index as u32 / 4) * 200, 64, 48).bounds;
            checked_layout(
                data.len(),
                capture.stride,
                capture.width,
                capture.height,
                bounds,
            )
            .unwrap()
        })
        .collect();

    let mut eager_copied = 0usize;
    let start = Instant::now();
    for _ in 0..BATCHES {
        let mut queued: Vec<_> = layouts
            .iter()
            .map(|layout| {
                let pixels = copy_region(data, capture.stride, layout).unwrap();
                eager_copied += pixels.len();
                pixels
            })
            .collect();
        for (index, layout) in layouts.iter().enumerate().skip(1) {
            queued[index] = copy_region(data, capture.stride, layout).unwrap();
            eager_copied += queued[index].len();
        }
        black_box(queued);
    }
    let eager_us = start.elapsed().as_micros();

    let mut lazy_copied = 0usize;
    let start = Instant::now();
    for _ in 0..BATCHES {
        for layout in &layouts {
            // Same copy routine used while a native BGRA pixel buffer is locked.
            let pixels = copy_region(data, capture.stride, layout).unwrap();
            lazy_copied += pixels.len();
            black_box(pixels);
        }
    }
    let lazy_us = start.elapsed().as_micros();

    let mut raw_stats = CopyStats::default();
    let start = Instant::now();
    for _ in 0..BATCHES {
        for index in 0..REGIONS {
            let pending = region((index as u32 % 4) * 300, (index as u32 / 4) * 200, 64, 48);
            let result = materialize(pending, &capture, true).unwrap();
            raw_stats.record(&result);
            black_box(result);
        }
    }
    let raw_us = start.elapsed().as_micros();
    assert_eq!(eager_copied, BATCHES * (2 * REGIONS - 1) * 64 * 48 * 4);
    assert_eq!(lazy_copied, BATCHES * REGIONS * 64 * 48 * 4);
    assert_eq!(raw_stats.copied_bytes, 0);
    assert_eq!(raw_stats.shared_bytes, lazy_copied as u64);
    println!("bitmap copy microbenchmark: batches={BATCHES} regions={REGIONS} eager_bytes={eager_copied} lazy_native_bytes={lazy_copied} raw_shared_bytes={} eager_us={eager_us} lazy_native_us={lazy_us} raw_us={raw_us}", raw_stats.shared_bytes);
}
