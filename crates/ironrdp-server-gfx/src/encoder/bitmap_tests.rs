use super::*;
use bytes::Bytes;
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use std::num::NonZeroU16;

#[test]
fn single_row_with_large_trailing_stride_encodes_without_arithmetic_overflow() {
    let mut bitmap = BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(1).unwrap(),
        height: NonZeroU16::new(1).unwrap(),
        format: PixelFormat::BgrA32,
        data: Bytes::from_static(&[10, 20, 30, 255]),
        stride: NonZeroUsize::new(1usize << (usize::BITS - 1)).unwrap(),
        dirty_rects: Vec::new(),
    };
    let mut output = vec![0; 1024];
    let len = BitmapEncoder::new().encode(&bitmap, &mut output).unwrap();
    let decoded: BitmapUpdateData<'_> = ironrdp_core::decode(&output[..len]).unwrap();
    assert_eq!(decoded.rectangles.len(), 1);
    assert_eq!(decoded.rectangles[0].height, 1);
    bitmap.height = NonZeroU16::new(2).unwrap();
    assert!(BitmapEncoder::new().encode(&bitmap, &mut output).is_err());
}

#[test]
fn odd_widths_and_partial_final_stride_roundtrip_without_losing_pixels() {
    for width in [1u16, 3, 417] {
        let row_bytes = usize::from(width) * 4;
        let stride = row_bytes + 12;
        let mut pixels = vec![0u8; stride + row_bytes];
        for y in 0..2 {
            for x in 0..usize::from(width) {
                let offset = y * stride + x * 4;
                pixels[offset..offset + 4].copy_from_slice(&[10 + y as u8, 20, 30, 255]);
            }
        }
        let bitmap = BitmapUpdate {
            x: 7,
            y: 9,
            width: NonZeroU16::new(width).unwrap(),
            height: NonZeroU16::new(2).unwrap(),
            format: PixelFormat::BgrA32,
            data: Bytes::from(pixels),
            stride: NonZeroUsize::new(stride).unwrap(),
            dirty_rects: Vec::new(),
        };
        let mut encoded = vec![0u8; 65536];
        let length = BitmapEncoder::new().encode(&bitmap, &mut encoded).unwrap();
        let update: BitmapUpdateData<'_> = ironrdp_core::decode(&encoded[..length]).unwrap();
        assert_eq!(update.rectangles.len(), 1);
        let rectangle = &update.rectangles[0];
        assert_eq!(rectangle.width, width);
        assert_eq!(rectangle.height, 2);
        assert_eq!(rectangle.rectangle.bottom, 10);
        assert_eq!(
            rectangle
                .compressed_data_header
                .as_ref()
                .unwrap()
                .scan_width,
            width * 4
        );
        let mut decoded = Vec::new();
        BitmapStreamDecoder::default()
            .decode_bitmap_stream_to_rgb24(
                rectangle.bitmap_data,
                &mut decoded,
                usize::from(width),
                2,
            )
            .unwrap();
        assert_eq!(decoded.len(), usize::from(width) * 2 * 3);
        for (row, blue) in decoded.chunks_exact(usize::from(width) * 3).zip([11u8, 10]) {
            assert!(row.chunks_exact(3).all(|pixel| pixel == [30, 20, blue]));
        }
    }
}

#[test]
fn multiple_rectangle_candidate_is_rejected_before_writing_output() {
    let mut encoder = BitmapEncoder::new();
    let mut bitmap = BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(1920).unwrap(),
        height: NonZeroU16::new(9).unwrap(),
        format: PixelFormat::BgrA32,
        data: Bytes::from(vec![255; 1920 * 9 * 4]),
        stride: NonZeroUsize::new(1920 * 4).unwrap(),
        dirty_rects: Vec::new(),
    };
    let rows = encoder.max_rows(&bitmap).unwrap();
    assert_eq!(rows, 8);
    let mut output = vec![0xa5; 100_000];
    assert!(encoder.encode(&bitmap, &mut output).is_err());
    assert!(output.iter().all(|byte| *byte == 0xa5));

    bitmap.height = NonZeroU16::new(rows).unwrap();
    let length = encoder.encode(&bitmap, &mut output).unwrap();
    let update: BitmapUpdateData<'_> = ironrdp_core::decode(&output[..length]).unwrap();
    assert_eq!(update.rectangles.len(), 1);
    assert_eq!(update.rectangles[0].height, rows);
}

#[test]
fn negotiated_compression_header_omission_changes_only_wire_framing() {
    let width = 7usize;
    let height = 3usize;
    let stride = width * 4 + 12;
    let mut pixels = vec![0; stride * (height - 1) + width * 4];
    for row in 0..height {
        for column in 0..width {
            let offset = row * stride + column * 4;
            pixels[offset..offset + 4].copy_from_slice(&[
                u8::try_from(row * 17 + column).unwrap(),
                u8::try_from(column * 29).unwrap(),
                u8::try_from(row * 53 + column * 7).unwrap(),
                255,
            ]);
        }
    }
    let bitmap = BitmapUpdate {
        x: 13,
        y: 19,
        width: NonZeroU16::new(u16::try_from(width).unwrap()).unwrap(),
        height: NonZeroU16::new(u16::try_from(height).unwrap()).unwrap(),
        format: PixelFormat::BgrA32,
        data: Bytes::from(pixels),
        stride: NonZeroUsize::new(stride).unwrap(),
        dirty_rects: Vec::new(),
    };
    for skip_alpha in [false, true] {
        let mut with_header = BitmapEncoder::new();
        with_header.set_skip_alpha(skip_alpha);
        assert!(with_header.compression_header_present());
        let mut without_header = with_header.clone();
        without_header.set_omit_compression_header(true);
        assert!(!without_header.compression_header_present());
        assert_eq!(
            with_header.max_rows(&bitmap).unwrap(),
            without_header.max_rows(&bitmap).unwrap()
        );

        let mut included = vec![0; 4096];
        let mut omitted = vec![0; 4096];
        let included_len = with_header.encode(&bitmap, &mut included).unwrap();
        let omitted_len = without_header.encode(&bitmap, &mut omitted).unwrap();
        included.truncate(included_len);
        omitted.truncate(omitted_len);

        let included_update: BitmapUpdateData<'_> = ironrdp_core::decode(&included).unwrap();
        let omitted_update: BitmapUpdateData<'_> = ironrdp_core::decode(&omitted).unwrap();
        assert_eq!(included_update.rectangles.len(), 1);
        assert_eq!(omitted_update.rectangles.len(), 1);
        let included_rect = &included_update.rectangles[0];
        let omitted_rect = &omitted_update.rectangles[0];
        assert_eq!(
            included_rect.compression_flags,
            Compression::BITMAP_COMPRESSION
        );
        assert_eq!(
            omitted_rect.compression_flags,
            Compression::BITMAP_COMPRESSION | Compression::NO_BITMAP_COMPRESSION_HDR
        );
        assert!(included_rect.compressed_data_header.is_some());
        assert!(omitted_rect.compressed_data_header.is_none());
        assert_eq!(included_rect.bitmap_data, omitted_rect.bitmap_data);

        // Verify the actual little-endian framing independently of the PDU
        // decoder: flags and bitmapLength are followed by TS_CD_HEADER only
        // in the conservative form; the compressed pixel stream is identical.
        assert_eq!(&included[..18], &omitted[..18]);
        assert_eq!(u16::from_le_bytes([included[18], included[19]]), 0x0001);
        assert_eq!(u16::from_le_bytes([omitted[18], omitted[19]]), 0x0401);
        let included_body = usize::from(u16::from_le_bytes([included[20], included[21]]));
        let omitted_body = usize::from(u16::from_le_bytes([omitted[20], omitted[21]]));
        assert_eq!(included_body, omitted_body + 8);
        assert_eq!(included.len(), omitted.len() + 8);
        assert_eq!(included_body, included.len() - 22);
        assert_eq!(omitted_body, omitted.len() - 22);
        assert_eq!(&included[30..], &omitted[22..]);

        let mut decoded = Vec::new();
        BitmapStreamDecoder::default()
            .decode_bitmap_stream_to_rgb24(omitted_rect.bitmap_data, &mut decoded, width, height)
            .unwrap();
        for row in 0..height {
            for column in 0..width {
                let source = (height - row - 1) * stride + column * 4;
                let target = (row * width + column) * 3;
                assert_eq!(
                    &decoded[target..target + 3],
                    &[
                        bitmap.data[source + 2],
                        bitmap.data[source + 1],
                        bitmap.data[source],
                    ]
                );
            }
        }
    }
}
