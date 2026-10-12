use super::*;
use bytes::Bytes;
use core::num::NonZeroUsize;
use ironrdp_graphics::image_processing::PixelFormat;

fn fixture(width: u16, height: u16, padding: usize, noise: bool) -> BitmapUpdate {
    let row_bytes = usize::from(width) * 4;
    let stride = row_bytes + padding;
    let mut data = vec![0; stride * usize::from(height - 1) + row_bytes];
    let mut seed = 0x1234_5678u32;
    for row in data.chunks_mut(stride) {
        for pixel in row[..row_bytes].chunks_exact_mut(4) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            pixel.copy_from_slice(&if noise {
                seed.to_le_bytes()
            } else {
                [12, 34, 56, 255]
            });
        }
    }
    BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(width).unwrap(),
        height: NonZeroU16::new(height).unwrap(),
        format: PixelFormat::BgrA32,
        data: Bytes::from(data),
        stride: NonZeroUsize::new(stride).unwrap(),
        dirty_rects: Vec::new(),
    }
}

// Keep the old sizing strategy here for byte-for-byte regression comparisons
// and the opt-in microbenchmark; production has only one encoding path.
fn legacy_encode(bitmap: &BitmapUpdate) -> Vec<u8> {
    let mut encoder = BitmapEncoder::new();
    legacy_encode_with(&mut encoder, bitmap)
}

fn legacy_encode_with(encoder: &mut BitmapEncoder, bitmap: &BitmapUpdate) -> Vec<u8> {
    let mut data = vec![0; bitmap.data.len() * 2];
    loop {
        match encoder.encode(bitmap, &mut data) {
            Ok(len) => {
                data.truncate(len);
                return data;
            }
            Err(BitmapEncodeError::Encode(err))
                if matches!(
                    err.kind(),
                    ironrdp_core::EncodeErrorKind::NotEnoughBytes { .. }
                ) =>
            {
                data.resize(data.len() * 2, 0);
            }
            Err(err) => panic!("fixture encode failed: {err}"),
        }
    }
}

#[test]
fn tighter_output_preserves_wire_bytes_for_small_noisy_and_padded_regions() {
    for bitmap in [
        fixture(1, 1, 0, false),
        fixture(3, 17, 8192, true),
        fixture(417, 123, 12, true),
        fixture(1920, 1080, 0, false),
    ] {
        let expected = legacy_encode(&bitmap);
        let actual = BitmapHandler::new().handle(&bitmap).unwrap();
        assert_eq!(actual.data, expected);
        let decoded: ironrdp_pdu::bitmap::BitmapUpdateData<'_> =
            ironrdp_core::decode(&actual.data).unwrap();
        assert_eq!(
            decoded
                .rectangles
                .iter()
                .map(|rect| u32::from(rect.height))
                .sum::<u32>(),
            u32::from(bitmap.height.get())
        );
    }
}

#[test]
fn allocation_tracks_visible_pixels_instead_of_padded_backing_storage() {
    let crop = fixture(16, 10, 16384, false);
    assert_eq!(
        BitmapEncoder::initial_output_size(&crop).unwrap(),
        16 * 10 * 4 + 30
    );
    let output = BitmapHandler::new().handle(&crop).unwrap();
    assert!(output.data.capacity() < crop.data.len() / 100);
    let desktop = fixture(3840, 2160, 0, false);
    assert!(BitmapEncoder::initial_output_size(&desktop).unwrap() < desktop.data.len() * 11 / 10);
}

#[test]
fn oversized_bitmap_rows_are_rejected_before_allocating_output() {
    let bitmap = fixture(u16::MAX, 1, 0, false);
    assert!(BitmapEncoder::initial_output_size(&bitmap).is_err());
}

#[test]
#[ignore = "manual release microbenchmark; timings depend on machine and load"]
fn bitmap_allocation_benchmark() {
    for (name, bitmap) in [
        ("solid-1080p", fixture(1920, 1080, 0, false)),
        ("noise-720p", fixture(1280, 720, 0, true)),
        ("narrow-dirty-region", fixture(32, 720, 7552, true)),
    ] {
        let mut handler = BitmapHandler::new();
        let mut legacy_encoder = BitmapEncoder::new();
        let start = std::time::Instant::now();
        let mut legacy_capacity = 0;
        for _ in 0..20 {
            let data = legacy_encode_with(&mut legacy_encoder, std::hint::black_box(&bitmap));
            legacy_capacity = data.capacity();
            std::hint::black_box(data);
        }
        let legacy_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = std::time::Instant::now();
        let mut capacity = 0;
        for _ in 0..20 {
            let frame = handler.handle(std::hint::black_box(&bitmap)).unwrap();
            capacity = frame.data.capacity();
            std::hint::black_box(frame);
        }
        println!("{name}: legacy_ms={legacy_ms:.2} current_ms={:.2} legacy_output_capacity={legacy_capacity} current_output_capacity={capacity} iterations=20", start.elapsed().as_secs_f64()*1000.0);
    }
}
