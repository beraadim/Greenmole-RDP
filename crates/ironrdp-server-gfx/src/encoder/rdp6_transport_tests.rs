//! Exercise complete legacy bitmap delivery with a receiver independent of the
//! RDP6 encoder/decoder implementation. Geometry and compression-header units
//! follow MS-RDPBCGR 2.2.9.1.1.3.1.2.2; plane RLE follows MS-RDPEGDI 3.1.9.2.

use super::*;
use bytes::Bytes;
use core::num::NonZeroUsize;
use ironrdp_core::{decode_cursor, ReadCursor};
use ironrdp_pdu::bitmap::{BitmapData, BitmapUpdateData};
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdatePdu, Fragmentation};

fn reassemble(mut output: UpdateFragmenter, fragmented: &mut bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut packet = vec![0; output.size_hint()];
    let mut started = false;
    let mut complete = false;
    while let Some(len) = output.next(&mut packet) {
        assert!(!complete);
        let mut cursor = ReadCursor::new(&packet[..len]);
        let header: FastPathHeader = decode_cursor(&mut cursor).unwrap();
        let header_len = cursor.pos();
        let update: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        assert_eq!(header_len + header.data_length, len);
        assert_eq!(update.update_code, UpdateCode::Bitmap);
        match update.fragmentation {
            Fragmentation::Single => {
                assert!(!started);
                complete = true;
            }
            Fragmentation::First => {
                assert!(!started);
                *fragmented = true;
            }
            Fragmentation::Next => assert!(started),
            Fragmentation::Last => {
                assert!(started);
                complete = true;
            }
        }
        started = true;
        bytes.extend_from_slice(update.data);
    }
    assert!(complete);
    bytes
}

pub(crate) fn fullscreen_pixels() -> Vec<u8> {
    // Flat 1920-pixel runs exercise long RLE segments. Paired scanlines
    // produce zero deltas alongside positive, negative and wrapping deltas.
    let mut pixels: Vec<u8> = (0..1200)
        .flat_map(|y| {
            let row = (y / 2) as u8;
            let blue = if row & 64 == 0 { row } else { 255 - row };
            [
                blue.wrapping_add(23),
                47u8.wrapping_sub(row),
                row.wrapping_mul(3),
                0, // Captured XRGB alpha may be unused; wire pixels stay opaque.
            ]
            .repeat(1920)
        })
        .collect();
    // Incompressible content makes a logical rectangle cross Fast-Path packet
    // boundaries even though Windows compatibility requires one rectangle/PDU.
    let mut random = 0x39a5_0c71u32;
    for pixel in pixels[128 * 1920 * 4..192 * 1920 * 4].chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *channel = random as u8;
        }
    }
    pixels
}

/// Strict test-only receiver: parse row segments and resolve deltas without
/// calling ironrdp-graphics' RDP6 decoder, so shared codec errors cannot cancel.
fn decode_plane(data: &[u8], offset: &mut usize, width: usize, height: usize) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        let mut last = 0;
        let mut decoded = Vec::with_capacity(width);
        while decoded.len() < width {
            let control = data[*offset];
            *offset += 1;
            assert_ne!(control, 0);
            let raw = usize::from(control >> 4);
            let run = usize::from(control & 15);
            let (raw, run) = match run {
                1 => (0, 16 + raw),
                2 => (0, 32 + raw),
                _ => (raw, run),
            };
            assert!(
                decoded.len() + raw + run <= width,
                "RLE segment crosses a row"
            );
            let literals = &data[*offset..*offset + raw];
            *offset += raw;
            decoded.extend_from_slice(literals);
            if let Some(value) = literals.last() {
                last = *value;
            }
            decoded.resize(decoded.len() + run, last);
        }
        if row != 0 {
            for (column, value) in decoded.iter_mut().enumerate() {
                let encoded = *value;
                let delta = if encoded & 1 == 0 {
                    i16::from(encoded / 2)
                } else {
                    -(i16::from(encoded) + 1) / 2
                };
                *value = (i16::from(pixels[(row - 1) * width + column]) + delta) as u8;
            }
        }
        pixels.extend_from_slice(&decoded);
    }
    pixels
}

pub(crate) fn decode_rectangle(rect: &BitmapData<'_>) -> Vec<u8> {
    assert_eq!(rect.bits_per_pixel, 32);
    let header = rect.compressed_data_header.as_ref().unwrap();
    assert_eq!(usize::from(header.main_body_size), rect.bitmap_data.len());
    // Match the byte-stride convention used by FreeRDP's shadow server.
    // The Microsoft TS_CD_HEADER prose calls this pixels; that discrepancy
    // is not resolved by this test and must not be treated as a codec failure.
    assert_eq!(header.scan_width, rect.width * 4);
    assert_eq!(
        usize::from(header.uncompressed_size),
        usize::from(rect.width) * usize::from(rect.height) * 4
    );
    let format = rect.bitmap_data[0];
    assert_eq!(format & !0x30, 0, "ARGB/RLE receiver only");
    assert_ne!(format & 0x10, 0);
    let width = usize::from(rect.width);
    let height = usize::from(rect.height);
    let mut offset = 1;
    if format & 0x20 == 0 {
        let alpha = decode_plane(rect.bitmap_data, &mut offset, width, height);
        assert!(alpha.iter().all(|value| *value == 255));
    }
    let red = decode_plane(rect.bitmap_data, &mut offset, width, height);
    let green = decode_plane(rect.bitmap_data, &mut offset, width, height);
    let blue = decode_plane(rect.bitmap_data, &mut offset, width, height);
    assert_eq!(
        offset,
        rect.bitmap_data.len(),
        "all color planes must consume exactly the stream"
    );
    (0..width * height)
        .flat_map(|i| [blue[i], green[i], red[i], 255])
        .collect()
}

async fn receive(
    encoder: &mut UpdateEncoder,
    bitmap: BitmapUpdate,
    canvas: &mut [u8],
    covered: &mut [u8],
    fragmented: &mut bool,
) -> usize {
    let mut iter = encoder.update(DisplayUpdate::Bitmap(bitmap));
    let mut rectangles = 0;
    while let Some(output) = iter.next().await {
        let bytes = reassemble(output.unwrap(), fragmented);
        let update: BitmapUpdateData<'_> = ironrdp_core::decode(&bytes).unwrap();
        assert_eq!(
            update.rectangles.len(),
            1,
            "one RDP6 rectangle per logical update"
        );
        assert!(!update.rectangles.is_empty());
        for rect in &update.rectangles {
            rectangles += 1;
            let pixels = decode_rectangle(rect);
            assert_eq!(rect.rectangle.right - rect.rectangle.left + 1, rect.width);
            assert_eq!(rect.rectangle.bottom - rect.rectangle.top + 1, rect.height);
            for y in rect.rectangle.top..=rect.rectangle.bottom {
                for x in rect.rectangle.left..=rect.rectangle.right {
                    let dst = usize::from(y) * 1920 + usize::from(x);
                    let src = usize::from(rect.rectangle.bottom - y) * usize::from(rect.width)
                        + usize::from(x - rect.rectangle.left);
                    covered[dst] = covered[dst].checked_add(1).unwrap();
                    canvas[dst * 4..dst * 4 + 4].copy_from_slice(&pixels[src * 4..src * 4 + 4]);
                }
            }
        }
    }
    rectangles
}

#[tokio::test]
async fn fullscreen_rdp6_baseline_and_later_damage_cover_and_decode_every_pixel() {
    for budget in [65_536, 131_072, 524_288] {
        for skip_alpha in [false, true] {
            let mut pixels = fullscreen_pixels();
            let mut expected = pixels.clone();
            for pixel in expected.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
            let source = BitmapUpdate {
                x: 0,
                y: 0,
                width: NonZeroU16::new(1920).unwrap(),
                height: NonZeroU16::new(1200).unwrap(),
                format: ironrdp_graphics::image_processing::PixelFormat::BgrA32,
                data: Bytes::copy_from_slice(&pixels),
                stride: NonZeroUsize::new(1920 * 4).unwrap(),
                dirty_rects: Vec::new(),
            };
            let mut codecs = UpdateEncoderCodecs::new();
            codecs.set_allow_skip_alpha(skip_alpha);
            codecs.set_max_request_size(16_777_215);
            let mut encoder = UpdateEncoder::new(
                DesktopSize {
                    width: 1920,
                    height: 1200,
                },
                CmdFlags::empty(),
                codecs,
            )
            .unwrap();
            assert_eq!(encoder.bitmap_codec(), "rdp6-rle");
            assert_eq!(encoder.set_bitmap_working_budget(budget), budget);
            let mut canvas = vec![0; pixels.len()];
            let mut covered = vec![0; 1920 * 1200];
            let mut fragmented = false;
            assert!(
                receive(
                    &mut encoder,
                    source,
                    &mut canvas,
                    &mut covered,
                    &mut fragmented
                )
                .await
                    > 0
            );
            if budget >= 131_072 {
                assert!(fragmented, "noisy rectangles must span Fast-Path packets");
            }
            assert!(covered.iter().all(|count| *count == 1));
            assert_eq!(canvas, expected);

            let mut patch_pixels = vec![0x5a; 160 * 23];
            for row in patch_pixels.chunks_exact_mut(160) {
                for pixel in row[..35 * 4].chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[97, 61, 29, 0]);
                }
            }
            // The last row has visible pixels but no trailing stride padding.
            patch_pixels.truncate(22 * 160 + 35 * 4);
            let patch = BitmapUpdate {
                x: 137,
                y: 91,
                width: NonZeroU16::new(35).unwrap(),
                height: NonZeroU16::new(23).unwrap(),
                format: ironrdp_graphics::image_processing::PixelFormat::BgrA32,
                data: Bytes::from(patch_pixels),
                stride: NonZeroUsize::new(160).unwrap(),
                dirty_rects: Vec::new(),
            };
            for y in 91..114 {
                for x in 137..172 {
                    let offset = (y * 1920 + x) * 4;
                    pixels[offset..offset + 4].copy_from_slice(&[97, 61, 29, 0]);
                    expected[offset..offset + 4].copy_from_slice(&[97, 61, 29, 255]);
                }
            }
            covered.fill(0);
            assert!(
                receive(
                    &mut encoder,
                    patch,
                    &mut canvas,
                    &mut covered,
                    &mut fragmented
                )
                .await
                    > 0
            );
            assert_eq!(canvas, expected);
            assert_eq!(covered.iter().filter(|count| **count != 0).count(), 35 * 23);
            let unchanged = BitmapUpdate {
                x: 0,
                y: 0,
                width: NonZeroU16::new(1920).unwrap(),
                height: NonZeroU16::new(1200).unwrap(),
                format: ironrdp_graphics::image_processing::PixelFormat::BgrA32,
                data: Bytes::copy_from_slice(&pixels),
                stride: NonZeroUsize::new(1920 * 4).unwrap(),
                dirty_rects: Vec::new(),
            };
            covered.fill(0);
            assert_eq!(
                receive(
                    &mut encoder,
                    unchanged,
                    &mut canvas,
                    &mut covered,
                    &mut fragmented
                )
                .await,
                0
            );
            assert!(covered.iter().all(|count| *count == 0));
        }
    }
}
