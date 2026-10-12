use super::*;
use bytes::Bytes;

fn frame() -> CapturedFrame {
    CapturedFrame {
        width: 4,
        height: 2,
        stride: 20,
        data: FrameData::Raw(Bytes::from(vec![7; 40])),
        timestamp_us: 0,
        content_rect: None,
        dirty_rects: vec![Rect {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        }],
    }
}

fn baseline() -> CachedFrame {
    CachedFrame::Bgra {
        width: 4,
        height: 2,
        stride: 20,
        data: Bytes::from(vec![7; 40]),
    }
}

#[test]
fn missing_damage_is_unknown_but_equal_valid_pixels_are_unchanged() {
    let previous = baseline();
    let mut capture = frame();
    capture.dirty_rects.clear();
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Unknown
    );
    assert!(!FrameDamage::Unknown.can_skip(false));

    let mut capture = frame();
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Unchanged
    );
    assert!(capture.dirty_rects.is_empty());
    assert!(FrameDamage::Unchanged.can_skip(false));
    // A reconnect, resize, recovery, or transport change still owes a seed.
    assert!(!FrameDamage::Unchanged.can_skip(true));
    assert!(!FrameDamage::Regions.can_skip(false));
}

#[test]
fn no_baseline_or_different_geometry_cannot_prove_unchanged() {
    let mut capture = frame();
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, None, false),
        FrameDamage::Regions
    );
    let mut previous = baseline();
    if let CachedFrame::Bgra { width, .. } = &mut previous {
        *width = 3;
    }
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Regions
    );
    assert!(!capture.dirty_rects.is_empty());
}

#[test]
fn padding_is_ignored_but_a_changed_visible_pixel_is_preserved() {
    let previous = baseline();
    let mut capture = frame();
    let mut data = vec![7; 40];
    data[16..20].fill(99);
    data[36..40].fill(99);
    capture.data = FrameData::Raw(Bytes::from(data.clone()));
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Unchanged
    );
    capture = frame();
    data[24] = 42;
    capture.data = FrameData::Raw(Bytes::from(data));
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Regions
    );
    assert_eq!(capture.dirty_rects.len(), 1);
}

#[test]
fn invalid_hints_or_buffers_never_become_a_no_change_proof() {
    let previous = baseline();
    for rect in [
        Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 1,
        },
        Rect {
            x: 4,
            y: 0,
            width: 1,
            height: 1,
        },
        Rect {
            x: u32::MAX,
            y: 0,
            width: 2,
            height: 1,
        },
    ] {
        let mut capture = frame();
        capture.dirty_rects = vec![rect];
        assert_eq!(
            refine_capture_dirty_rects(&mut capture, Some(&previous), false),
            FrameDamage::Regions
        );
    }
    let mut capture = frame();
    capture.stride = 4;
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Regions
    );
    capture = frame();
    capture.data = FrameData::Raw(Bytes::from_static(&[7; 8]));
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Regions
    );
}

#[test]
fn accumulated_partial_delivery_damage_survives_even_if_pixels_revert() {
    let previous = baseline();
    let mut capture = frame();
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), true),
        FrameDamage::Regions
    );
    assert_eq!(capture.dirty_rects.len(), 1);
}

#[test]
fn edge_crossing_damage_is_clipped_to_visible_pixels_before_comparison() {
    let previous = baseline();
    let mut capture = frame();
    capture.dirty_rects = vec![Rect {
        x: 2,
        y: 1,
        width: 10,
        height: 10,
    }];
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Unchanged
    );
    capture.dirty_rects = vec![Rect {
        x: 2,
        y: 1,
        width: 10,
        height: 10,
    }];
    let mut bytes = vec![7; 40];
    bytes[28] = 42;
    capture.data = FrameData::Raw(Bytes::from(bytes));
    assert_eq!(
        refine_capture_dirty_rects(&mut capture, Some(&previous), false),
        FrameDamage::Regions
    );
    let rect = &capture.dirty_rects[0];
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (2, 1, 2, 1));
}
