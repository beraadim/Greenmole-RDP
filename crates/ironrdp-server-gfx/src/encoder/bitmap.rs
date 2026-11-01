use core::num::NonZeroUsize;

use ironrdp_core::{cast_length, invalid_field_err, Encode as _, WriteCursor};
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
    omit_compression_header: bool,
}

#[cfg(test)]
#[path = "bitmap_tests.rs"]
mod tests;

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
            omit_compression_header: false,
        }
    }

    pub(crate) fn set_skip_alpha(&mut self, allowed: bool) {
        self.skip_alpha = allowed;
    }

    pub(crate) fn set_omit_compression_header(&mut self, allowed: bool) {
        self.omit_compression_header = allowed;
    }

    pub(super) fn compression_header_present(&self) -> bool {
        !self.omit_compression_header
    }

    pub(super) fn alpha_skipped(&self) -> bool {
        self.skip_alpha
    }

    /// Largest row strip that fits the uncompressed and compressed u16 fields.
    /// The display iterator uses this same bound before encoding, so each
    /// logical BitmapUpdate carries one rectangle for Windows compatibility.
    pub(super) fn max_rows(&self, bitmap: &BitmapUpdate) -> Result<u16, BitmapEncodeError> {
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
        // (including the eight-byte compression header). Keep this conservative
        // bound when the negotiated header is omitted, preserving the maximum
        // strip height and pre-encode work bound.
        let width = usize::from(bitmap.width.get());
        let plane_count = if self.skip_alpha { 3 } else { 4 };
        let compressed_row_bound = plane_count * (width + width.div_ceil(15));
        let compressed_rows = (usize::from(u16::MAX) - 9) / compressed_row_bound;
        let max_rows = (u16::MAX / row_len).min(u16::try_from(compressed_rows).unwrap_or(u16::MAX));
        if max_rows == 0 {
            return Err(BitmapEncodeError::Encode(invalid_field_err!(
                "bitmap",
                "Row exceeds compressed bitmap bounds"
            )));
        }
        Ok(max_rows)
    }

    pub(crate) fn encode(
        &mut self,
        bitmap: &BitmapUpdate,
        output: &mut [u8],
    ) -> Result<usize, BitmapEncodeError> {
        if bitmap.height.get() > self.max_rows(bitmap)? {
            return Err(BitmapEncodeError::Encode(invalid_field_err!(
                "bitmap",
                "Bitmap update exceeds one rectangle row strip"
            )));
        }
        let bytes_per_pixel = u16::from(bitmap.format.bytes_per_pixel());
        // max_rows checked this product before any output or source access.
        let row_len = bitmap.width.get() * bytes_per_pixel;

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
        // Several valid rectangles in one logical update render incompletely
        // on affected Windows clients. Encode only this independently bounded
        // strip; the iterator yields subsequent strips as separate updates.
        // https://github.com/FreeRDP/FreeRDP/pull/10455
        BitmapUpdateData::encode_header(1, &mut cursor).map_err(BitmapEncodeError::Encode)?;
        let height = bitmap.height.get();
        let encoder =
            BitmapStreamEncoder::new(NonZeroUsize::from(bitmap.width).get(), usize::from(height));
        let pixels = bitmap.data[..required]
            .chunks(stride)
            .map(|row| &row[..usize::from(row_len)])
            .rev()
            .flat_map(|row| row.chunks(usize::from(bytes_per_pixel)));
        let len = Self::encode_iter(
            encoder,
            bitmap.format,
            pixels,
            self.buffer.as_mut_slice(),
            self.skip_alpha,
        )?;
        let compressed_data_header = if self.omit_compression_header {
            None
        } else {
            Some(bitmap::CompressedDataHeader {
                main_body_size: cast_length!("main body size", len)
                    .map_err(BitmapEncodeError::Encode)?,
                // Preserve the existing byte-stride header convention.
                scan_width: row_len,
                uncompressed_size: height * row_len,
            })
        };
        let mut compression_flags = Compression::BITMAP_COMPRESSION;
        if self.omit_compression_header {
            compression_flags |= Compression::NO_BITMAP_COMPRESSION_HDR;
        }
        let data = BitmapData {
            rectangle: InclusiveRectangle {
                left: bitmap.x,
                top: bitmap.y,
                right: bitmap.x + bitmap.width.get() - 1,
                bottom: bitmap.y + height - 1,
            },
            width: u16::from(bitmap.width),
            height,
            bits_per_pixel: u16::from(bitmap.format.bytes_per_pixel()) * 8,
            compression_flags,
            compressed_data_header,
            bitmap_data: &self.buffer[..len],
        };
        data.encode(&mut cursor)
            .map_err(BitmapEncodeError::Encode)?;

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
