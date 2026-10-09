use core::num::NonZeroUsize;

use ironrdp_core::{cast_int, cast_length, invalid_field_err, Encode as _, WriteCursor};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::rdp6::{
    ABgrChannels, ARgbChannels, BgrAChannels, BitmapEncodeError, BitmapStreamEncoder, RgbAChannels,
};
use ironrdp_pdu::bitmap::{self, BitmapData, BitmapUpdateData, Compression};
use ironrdp_pdu::geometry::InclusiveRectangle;

use crate::BitmapUpdate;

// PERF: we could also remove the need for this buffer
#[derive(Clone)]
pub(crate) struct BitmapEncoder {
    buffer: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use ironrdp_graphics::rdp6::BitmapStreamDecoder;
    use std::num::NonZeroU16;

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
}

impl BitmapEncoder {
    pub(crate) fn new() -> Self {
        Self {
            buffer: vec![0; usize::from(u16::MAX)],
        }
    }

    pub(crate) fn encode(
        &mut self,
        bitmap: &BitmapUpdate,
        output: &mut [u8],
    ) -> Result<usize, BitmapEncodeError> {
        let bytes_per_pixel = u16::from(bitmap.format.bytes_per_pixel());
        let row_len = bitmap
            .width
            .get()
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| {
                BitmapEncodeError::Encode(invalid_field_err!(
                    "bitmap",
                    "Row exceeds bitmap header bounds"
                ))
            })?;
        let chunk_height = u16::MAX / row_len;

        let mut cursor = WriteCursor::new(output);
        let stride = bitmap.stride.get();
        let required = usize::from(bitmap.height.get() - 1)
            .checked_mul(stride)
            .and_then(|prefix| prefix.checked_add(usize::from(row_len)))
            .ok_or_else(|| {
                BitmapEncodeError::Encode(invalid_field_err!("bitmap", "Invalid bitmap extent"))
            })?;
        if stride < usize::from(row_len) || bitmap.data.len() < required {
            return Err(BitmapEncodeError::Encode(invalid_field_err!(
                "bitmap",
                "Truncated bitmap rows"
            )));
        }
        let chunks = bitmap.data[..required].chunks(stride * usize::from(chunk_height));

        let total = cast_int!("chunks length lower bound", chunks.size_hint().0)
            .map_err(BitmapEncodeError::Encode)?;
        BitmapUpdateData::encode_header(total, &mut cursor).map_err(BitmapEncodeError::Encode)?;

        for (i, chunk) in chunks.enumerate() {
            // Subregions omit padding after their final row. Floor division
            // lost that row, even though its pixels are present in the slice.
            let height = cast_int!("bitmap height", chunk.len().div_ceil(stride))
                .map_err(BitmapEncodeError::Encode)?;
            let i: u16 = cast_int!("chunk idx", i).map_err(BitmapEncodeError::Encode)?;
            let top = bitmap.y + i * chunk_height;

            let encoder = BitmapStreamEncoder::new(
                NonZeroUsize::from(bitmap.width).get(),
                usize::from(height),
            );

            let len = {
                let pixels = chunk
                    .chunks(stride)
                    .map(|row| &row[..usize::from(row_len)])
                    .rev()
                    .flat_map(|row| row.chunks(usize::from(bytes_per_pixel)));

                Self::encode_iter(encoder, bitmap.format, pixels, self.buffer.as_mut_slice())?
            };

            let data = BitmapData {
                rectangle: InclusiveRectangle {
                    left: bitmap.x,
                    top,
                    right: bitmap.x + bitmap.width.get() - 1,
                    bottom: top + height - 1,
                },
                width: u16::from(bitmap.width),
                height,
                bits_per_pixel: u16::from(bitmap.format.bytes_per_pixel()) * 8,
                compression_flags: Compression::BITMAP_COMPRESSION,
                compressed_data_header: Some(bitmap::CompressedDataHeader {
                    main_body_size: cast_length!("main body size", len)
                        .map_err(BitmapEncodeError::Encode)?,
                    // cbScanWidth is measured in bytes, not pixels. Our 32bpp
                    // rows are always DWORD aligned, including odd widths.
                    scan_width: row_len,
                    uncompressed_size: height * row_len,
                }),
                bitmap_data: &self.buffer[..len],
            };

            data.encode(&mut cursor)
                .map_err(BitmapEncodeError::Encode)?;
        }

        Ok(cursor.pos())
    }

    fn encode_iter<'a, P>(
        mut encoder: BitmapStreamEncoder,
        format: PixelFormat,
        src: P,
        dst: &mut [u8],
    ) -> Result<usize, BitmapEncodeError>
    where
        P: Iterator<Item = &'a [u8]> + Clone,
    {
        let written = match format {
            PixelFormat::ARgb32 | PixelFormat::XRgb32 => {
                encoder.encode_pixels_stream::<_, ARgbChannels>(src, dst, true)?
            }
            PixelFormat::RgbA32 | PixelFormat::RgbX32 => {
                encoder.encode_pixels_stream::<_, RgbAChannels>(src, dst, true)?
            }
            PixelFormat::ABgr32 | PixelFormat::XBgr32 => {
                encoder.encode_pixels_stream::<_, ABgrChannels>(src, dst, true)?
            }
            PixelFormat::BgrA32 | PixelFormat::BgrX32 => {
                encoder.encode_pixels_stream::<_, BgrAChannels>(src, dst, true)?
            }
        };

        Ok(written)
    }
}
