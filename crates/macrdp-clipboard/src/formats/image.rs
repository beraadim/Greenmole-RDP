//! Bounded clipboard image conversion. Validate headers before allocating pixel buffers.

use anyhow::{ensure, Context, Result};
use image::{ImageDecoder, ImageEncoder, ImageFormat, ImageReader, RgbaImage};
use std::io::{Cursor, Write};

pub const BITMAPINFOHEADER_SIZE: usize = 40;
pub const MAX_IMAGE_DIMENSION: u32 = 8192;
pub const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PIXEL_BYTES: u64 = MAX_IMAGE_PIXELS * 4;
// Includes room for PNG framing/deflate overhead and DIB headers.
pub const MAX_IMAGE_PAYLOAD_BYTES: usize = MAX_PIXEL_BYTES as usize + 1024 * 1024;

fn pixel_bytes(width: u32, height: u32) -> Result<usize> {
    ensure!(
        width > 0 && height > 0,
        "Clipboard image dimensions must be positive"
    );
    ensure!(
        width <= MAX_IMAGE_DIMENSION && height <= MAX_IMAGE_DIMENSION,
        "Clipboard image dimension exceeds 8192 pixels"
    );
    let pixels = u64::from(width) * u64::from(height);
    ensure!(
        pixels <= MAX_IMAGE_PIXELS,
        "Clipboard image exceeds 16 megapixels"
    );
    Ok((pixels * 4) as usize)
}

/// PNG, TIFF and JPEG are the only image types advertised by the pasteboard bridge.
fn decode_image(data: &[u8]) -> Result<RgbaImage> {
    ensure!(
        data.len() <= MAX_IMAGE_PAYLOAD_BYTES,
        "Clipboard image payload exceeds limit"
    );
    let format = image::guess_format(data)?;
    ensure!(
        matches!(
            format,
            ImageFormat::Png | ImageFormat::Tiff | ImageFormat::Jpeg
        ),
        "Unsupported clipboard image format"
    );
    let mut reader = ImageReader::with_format(Cursor::new(data), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_PIXEL_BYTES);
    reader.limits(limits);
    let decoder = reader
        .into_decoder()
        .context("Invalid clipboard image header")?;
    let (width, height) = decoder.dimensions();
    pixel_bytes(width, height)?;
    ensure!(
        decoder.total_bytes() <= MAX_PIXEL_BYTES,
        "Decoded clipboard image exceeds limit"
    );
    Ok(image::DynamicImage::from_decoder(decoder)?.into_rgba8())
}

/// Convert a bounded PNG/TIFF/JPEG image to uncompressed, bottom-up 32-bit CF_DIB.
pub fn png_to_dib(data: &[u8]) -> Result<Vec<u8>> {
    let rgba = decode_image(data)?;
    let (width, height) = rgba.dimensions();
    let size = pixel_bytes(width, height)?;
    let mut dib = Vec::new();
    dib.try_reserve_exact(BITMAPINFOHEADER_SIZE + size)?;
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    dib.extend_from_slice(&(height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&(size as u32).to_le_bytes());
    dib.extend_from_slice(&[0; 16]);
    for row in rgba.as_raw().chunks_exact(width as usize * 4).rev() {
        for p in row.chunks_exact(4) {
            dib.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    Ok(dib)
}

struct DibLayout {
    width: u32,
    height: u32,
    bottom_up: bool,
    bytes_per_pixel: usize,
    row_stride: usize,
    rgba_bytes: usize,
}

impl DibLayout {
    fn parse(data: &[u8]) -> Result<Self> {
        ensure!(data.len() >= BITMAPINFOHEADER_SIZE, "Truncated DIB header");
        ensure!(
            data.len() <= MAX_IMAGE_PAYLOAD_BYTES,
            "DIB payload exceeds limit"
        );
        let u32_at = |i| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
        ensure!(u32_at(0) == 40, "Unsupported DIB header size");
        let width = i32::from_le_bytes(data[4..8].try_into().unwrap());
        let signed_height = i32::from_le_bytes(data[8..12].try_into().unwrap());
        ensure!(
            width > 0 && signed_height != 0 && signed_height != i32::MIN,
            "Invalid DIB dimensions"
        );
        let width = width as u32;
        let height = signed_height.unsigned_abs();
        let rgba_bytes = pixel_bytes(width, height)?;
        ensure!(
            u16::from_le_bytes(data[12..14].try_into().unwrap()) == 1,
            "DIB must have one plane"
        );
        let bits = u16::from_le_bytes(data[14..16].try_into().unwrap());
        ensure!(
            matches!(bits, 24 | 32),
            "Only 24/32-bit DIB images are supported"
        );
        ensure!(
            u32_at(16) == 0,
            "Compressed/bitfield DIB images are unsupported"
        );
        ensure!(u32_at(32) == 0, "DIB color tables are unsupported");
        let bytes_per_pixel = usize::from(bits / 8);
        let row_stride = (width as usize * bytes_per_pixel)
            .checked_add(3)
            .context("DIB row size overflow")?
            & !3;
        let size = row_stride
            .checked_mul(height as usize)
            .context("DIB size overflow")?;
        let end = BITMAPINFOHEADER_SIZE
            .checked_add(size)
            .context("DIB size overflow")?;
        ensure!(data.len() >= end, "Truncated DIB pixel data");
        let declared_size = u32_at(20) as usize;
        ensure!(
            declared_size == 0 || declared_size == size,
            "Inconsistent DIB image size"
        );
        Ok(Self {
            width,
            height,
            bottom_up: signed_height > 0,
            bytes_per_pixel,
            row_stride,
            rgba_bytes,
        })
    }
}

/// CF_DIB supports BI_RGB BITMAPINFOHEADER with padded BGR/BGRA rows in either direction.
/// Other DIB variants are rejected explicitly instead of guessing pixel offsets or masks.
pub fn dib_to_png(data: &[u8]) -> Result<Vec<u8>> {
    let layout = DibLayout::parse(data)?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(layout.rgba_bytes)?;
    let data = &data[BITMAPINFOHEADER_SIZE..];
    // BI_RGB's fourth byte is often unused and zero on Windows. Preserve useful alpha
    // from producers that provide it, but do not make an all-zero-alpha image invisible.
    let has_alpha = layout.bytes_per_pixel == 4
        && data[..layout.row_stride * layout.height as usize]
            .chunks_exact(4)
            .any(|pixel| pixel[3] != 0);
    for y in 0..layout.height as usize {
        let source_y = if layout.bottom_up {
            layout.height as usize - 1 - y
        } else {
            y
        };
        let start = source_y * layout.row_stride;
        let row = &data[start..start + layout.width as usize * layout.bytes_per_pixel];
        for p in row.chunks_exact(layout.bytes_per_pixel) {
            pixels.extend_from_slice(&[p[2], p[1], p[0], if has_alpha { p[3] } else { 255 }]);
        }
    }
    encode_png(&pixels, layout.width, layout.height)
}

fn encode_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut writer = BoundedWriter(Vec::new());
    image::codecs::png::PngEncoder::new(&mut writer).write_image(
        pixels,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(writer.0)
}

struct BoundedWriter(Vec<u8>);
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_IMAGE_PAYLOAD_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "Encoded clipboard image exceeds limit",
            ));
        }
        self.0
            .try_reserve(bytes.len())
            .map_err(std::io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "image/tests.rs"]
mod tests;
