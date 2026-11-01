//! Checked zero-copy crops preserve explicit delivery intent without retaining
//! stale damage hints or accepting malformed backing layouts.

use super::*;

fn bitmap() -> BitmapUpdate {
    BitmapUpdate {
        x: 7,
        y: 9,
        width: NonZeroU16::new(4).unwrap(),
        height: NonZeroU16::new(3).unwrap(),
        format: PixelFormat::BgrA32,
        data: Bytes::from((0..56u8).collect::<Vec<_>>()),
        stride: NonZeroUsize::new(20).unwrap(),
        dirty_rects: vec![Rect::new(7, 9, 1, 1)],
        force_refresh: false,
    }
}

fn crop(source: &BitmapUpdate, x: u16, y: u16, width: u16, height: u16) -> Option<BitmapUpdate> {
    source.sub(
        x,
        y,
        NonZeroU16::new(width).unwrap(),
        NonZeroU16::new(height).unwrap(),
    )
}

#[test]
fn crops_share_padded_rows_and_preserve_only_explicit_refresh_intent() {
    for force in [false, true] {
        let mut source = bitmap();
        source.force_refresh = force;
        let region = crop(&source, 1, 1, 2, 2).unwrap();
        assert_eq!(
            (region.x, region.y, region.width.get(), region.height.get()),
            (8, 10, 2, 2)
        );
        assert_eq!(region.data.as_ptr(), source.data[24..].as_ptr());
        assert_eq!(region.data.as_ref(), &source.data[24..52]);
        assert_eq!(region.stride, source.stride);
        assert!(region.dirty_rects.is_empty());
        assert_eq!(region.force_refresh, force);
        drop(source);
        assert_eq!(&region.data[20..], &[44, 45, 46, 47, 48, 49, 50, 51]);
    }
}

#[test]
fn out_of_bounds_wrapped_and_truncated_crops_return_none() {
    let mut source = bitmap();
    for (x, y, width, height) in [
        (4, 0, 1, 1),
        (0, 3, 1, 1),
        (u16::MAX, 0, 2, 1),
        (0, u16::MAX, 1, 2),
        (1, 1, u16::MAX, 1),
    ] {
        assert!(crop(&source, x, y, width, height).is_none());
    }
    source.x = u16::MAX;
    assert!(crop(&source, 0, 0, 1, 1).is_none());
    source = bitmap();
    source.y = u16::MAX;
    assert!(crop(&source, 0, 0, 1, 1).is_none());
    source = bitmap();
    source.stride = NonZeroUsize::new(12).unwrap();
    assert!(crop(&source, 0, 0, 1, 1).is_none());
    source.stride = NonZeroUsize::new(usize::MAX).unwrap();
    assert!(crop(&source, 0, 0, 1, 2).is_none());
    source = bitmap();
    source.data = source.data.slice(..55);
    assert!(crop(&source, 0, 0, 4, 3).is_none());
    assert!(
        crop(&source, 0, 0, 1, 1).is_none(),
        "the source layout must be complete"
    );
}

#[test]
fn single_row_does_not_require_trailing_stride_and_maximum_desktop_edge_is_valid() {
    let mut source = bitmap();
    source.x = u16::MAX - 1;
    source.y = u16::MAX - 1;
    source.width = NonZeroU16::new(1).unwrap();
    source.height = NonZeroU16::new(1).unwrap();
    source.stride = NonZeroUsize::new(usize::MAX).unwrap();
    source.data = Bytes::from_static(&[1, 2, 3, 4]);
    let region = crop(&source, 0, 0, 1, 1).unwrap();
    assert_eq!(region.data.as_ref(), &[1, 2, 3, 4]);
    assert_eq!((region.x, region.y), (u16::MAX - 1, u16::MAX - 1));
}
