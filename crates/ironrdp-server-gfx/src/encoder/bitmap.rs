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
    skip_alpha: bool,
}

#[cfg(test)]
mod tests {
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
}

impl BitmapEncoder {
    /// Size the initial output from visible pixels, not the source stride. A
    /// narrow dirty region can still borrow rows with a full-desktop stride.
    /// RDP6 normally fits within raw pixels plus per-chunk headers; callers may
    /// grow the output if an incompressible stream exceeds this estimate.
    pub(crate) fn initial_output_size(bitmap: &BitmapUpdate) -> Result<usize, BitmapEncodeError> {
        let row_bytes =
            usize::from(bitmap.width.get()) * usize::from(bitmap.format.bytes_per_pixel());
        if row_bytes == 0 || row_bytes > usize::from(u16::MAX) {
            return Err(BitmapEncodeError::Encode(invalid_field_err!(
                "bitmap",
                "Row exceeds bitmap header bounds"
            )));
        }
        let rows_per_chunk = usize::from(u16::MAX) / row_bytes;
        let height = usize::from(bitmap.height.get());
        // TS_UPDATE_BITMAP_DATA header (4), TS_BITMAP_DATA (18) and optional
        // TS_CD_HEADER (8). Arithmetic stays checked even on 32-bit targets.
        row_bytes
            .checked_mul(height)
            .and_then(|pixels| {
                height
                    .div_ceil(rows_per_chunk)
                    .checked_mul(26)
                    .and_then(|headers| pixels.checked_add(headers))
            })
            .and_then(|size| size.checked_add(4))
            .ok_or_else(|| {
                BitmapEncodeError::Encode(invalid_field_err!("bitmap", "Output size overflow"))
            })
    }

    pub(crate) fn new() -> Self {
        Self {
            buffer: vec![0; usize::from(u16::MAX)],
            skip_alpha: true,
        }
    }

    pub(crate) fn set_skip_alpha(&mut self, allowed: bool) {
        self.skip_alpha = allowed;
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
        // RLE adds a control byte per group of up to 15 raw samples. Including
        // alpha can exceed the old raw-pixel budget for narrow tall regions.
        // Bound both the uncompressed u16 length and compressed bitmap length
        // (which also includes the eight-byte compression header).
        let width = usize::from(bitmap.width.get());
        let plane_count = if self.skip_alpha { 3 } else { 4 };
        let compressed_row_bound = plane_count * (width + width.div_ceil(15));
        let compressed_rows = (usize::from(u16::MAX) - 9) / compressed_row_bound;
        let chunk_height = (u16::MAX / row_len)
            .min(u16::try_from(compressed_rows).unwrap_or(u16::MAX))
            .min(bitmap.height.get());
        if chunk_height == 0 {
            return Err(BitmapEncodeError::Encode(invalid_field_err!(
                "bitmap",
                "Row exceeds compressed bitmap bounds"
            )));
        }

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
        let chunk_bytes = stride
            .checked_mul(usize::from(chunk_height))
            .ok_or_else(|| {
                BitmapEncodeError::Encode(invalid_field_err!("bitmap", "Chunk stride overflow"))
            })?;
        let chunks = bitmap.data[..required].chunks(chunk_bytes);

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

                Self::encode_iter(
                    encoder,
                    bitmap.format,
                    pixels,
                    self.buffer.as_mut_slice(),
                    self.skip_alpha,
                )?
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
        skip_alpha: bool,
    ) -> Result<usize, BitmapEncodeError>
    where
        P: Iterator<Item = &'a [u8]> + Clone,
    {
        let written = match format {
            PixelFormat::ARgb32 | PixelFormat::XRgb32 => {
                Self::encode_planes::<_, ARgbChannels>(&mut encoder, src, dst, skip_alpha)?
            }
            PixelFormat::RgbA32 | PixelFormat::RgbX32 => {
                Self::encode_planes::<_, RgbAChannels>(&mut encoder, src, dst, skip_alpha)?
            }
            PixelFormat::ABgr32 | PixelFormat::XBgr32 => {
                Self::encode_planes::<_, ABgrChannels>(&mut encoder, src, dst, skip_alpha)?
            }
            PixelFormat::BgrA32 | PixelFormat::BgrX32 => {
                Self::encode_planes::<_, BgrAChannels>(&mut encoder, src, dst, skip_alpha)?
            }
        };

        Ok(written)
    }

    fn encode_planes<'a, P, F>(
        encoder: &mut BitmapStreamEncoder,
        src: P,
        dst: &mut [u8],
        skip_alpha: bool,
    ) -> Result<usize, BitmapEncodeError>
    where
        P: Iterator<Item = &'a [u8]> + Clone,
        F: ironrdp_graphics::rdp6::PixelFormat,
    {
        if skip_alpha {
            return encoder.encode_pixels_stream::<_, F>(src, dst, true);
        }
        // RDP desktop updates are opaque, including XRGB capture buffers whose
        // unused fourth byte need not be 255. Only omit this plane if the
        // client advertises DRAW_ALLOW_SKIP_ALPHA (MS-RDPBCGR 2.2.7.1.2).
        let r = src.clone().map(F::r);
        let g = src.clone().map(F::g);
        let b = src.clone().map(F::b);
        let alpha = src.map(|_| 255);
        let len = encoder.encode_channels_stream_alpha((r, g, b, alpha), dst, true)?;
        // ironrdp-graphics 0.7.0 emits an RGB-only header even from its alpha
        // encoder. Correct the typed header to describe the four encoded planes.
        let header = bitmap::rdp6::BitmapStreamHeader {
            enable_rle_compression: true,
            use_alpha: true,
            color_plane_definition: bitmap::rdp6::ColorPlaneDefinition::Argb,
        };
        header
            .encode(&mut WriteCursor::new(dst))
            .map_err(BitmapEncodeError::Encode)?;
        Ok(len)
    }
}
