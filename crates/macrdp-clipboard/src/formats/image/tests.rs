use super::*;

fn dib(width: i32, height: i32, bits: u16, pixels: &[u8]) -> Vec<u8> {
    let mut data = vec![0; 40];
    data[0..4].copy_from_slice(&40u32.to_le_bytes());
    data[4..8].copy_from_slice(&width.to_le_bytes());
    data[8..12].copy_from_slice(&height.to_le_bytes());
    data[12..14].copy_from_slice(&1u16.to_le_bytes());
    data[14..16].copy_from_slice(&bits.to_le_bytes());
    data.extend_from_slice(pixels);
    data
}

#[test]
fn rejects_hostile_dimensions_and_truncated_payload_before_pixel_allocation() {
    for (w, h) in [
        (0, 1),
        (-1, 1),
        (1, 0),
        (1, i32::MIN),
        (i32::MAX, 2),
        (2, i32::MAX),
        (8192, 8192),
        (8193, 1),
    ] {
        assert!(dib_to_png(&dib(w, h, 32, &[])).is_err(), "{w}x{h}");
    }
    // Dimensions within the limit must still have the entire padded payload.
    assert!(dib_to_png(&dib(4096, 4096, 32, &[0; 4])).is_err());
    assert!(dib_to_png(&dib(1, 2, 24, &[0; 7])).is_err());
    assert!(pixel_bytes(4096, 4096).is_ok());
    assert!(pixel_bytes(4096, 4097).is_err());
}

#[test]
fn rejects_unsupported_or_inconsistent_headers() {
    let valid = dib(1, 1, 32, &[0; 4]);
    for (offset, bytes) in [
        (0, 124u32.to_le_bytes()),
        (12, 0u32.to_le_bytes()),
        (16, 3u32.to_le_bytes()),
        (20, 9u32.to_le_bytes()),
        (32, 1u32.to_le_bytes()),
    ] {
        let mut data = valid.clone();
        data[offset..offset + 4].copy_from_slice(&bytes);
        assert!(dib_to_png(&data).is_err(), "field at {offset}");
    }
    for bits in [0, 1, 8, 16, 64, u16::MAX] {
        assert!(dib_to_png(&dib(1, 1, bits, &[0; 8])).is_err());
    }
}

#[test]
fn padded_24_bit_rows_have_correct_colors_and_orientation() {
    let bottom = [255, 0, 0, 73]; // Blue plus one ignored padding byte.
    let top = [0, 0, 255, 92]; // Red.
    for (height, rows) in [(2, [bottom, top]), (-2, [top, bottom])] {
        let data = dib(1, height, 24, &rows.concat());
        let image = image::load_from_memory(&dib_to_png(&data).unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(0, 1).0, [0, 0, 255, 255]);
    }
}

#[test]
fn windows_unused_alpha_is_opaque_and_real_alpha_is_preserved() {
    for (alpha, expected) in [(0, 255), (128, 128), (255, 255)] {
        let png = dib_to_png(&dib(1, -1, 32, &[30, 20, 10, alpha])).unwrap();
        let image = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [10, 20, 30, expected]);
    }
}

#[test]
fn bounded_native_formats_decode_and_malformed_images_fail() {
    let image =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 2, image::Rgb([5, 6, 7])));
    for format in [ImageFormat::Png, ImageFormat::Tiff, ImageFormat::Jpeg] {
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, format).unwrap();
        let result = png_to_dib(encoded.get_ref()).unwrap();
        assert_eq!(result.len(), 40 + 3 * 2 * 4);
        assert!(png_to_dib(&encoded.get_ref()[..12]).is_err());
    }
    assert!(png_to_dib(b"not an image").is_err());
    assert!(png_to_dib(b"GIF89a\x01\0\x01\0").is_err());
}

#[test]
fn compressed_image_dimensions_are_checked_before_decode() {
    let mut png = encode_png(&[1, 2, 3, 255], 1, 1).unwrap();
    // Rewrite a valid PNG's IHDR and its CRC without allocating declared pixels.
    png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    png[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    let mut crc = u32::MAX;
    for byte in &png[12..29] {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    png[29..33].copy_from_slice(&(!crc).to_be_bytes());
    assert!(png_to_dib(&png).is_err());

    // A minimal TIFF IFD with enormous declared dimensions and no strip data.
    let mut tiff = b"II\x2a\0\x08\0\0\0\x02\0".to_vec();
    for tag in [256u16, 257u16] {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    assert!(png_to_dib(&tiff).is_err());
}
