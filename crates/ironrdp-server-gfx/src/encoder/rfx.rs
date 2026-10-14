use std::io;

use ironrdp_acceptor::DesktopSize;
use ironrdp_core::{cast_int, cast_length, other_err, Encode as _, EncodeResult};
use ironrdp_graphics::color_conversion::to_64x64_ycbcr_tile;
use ironrdp_graphics::rlgr::RlgrError;
use ironrdp_pdu::codecs::rfx::{
    self, Block, ChannelsPdu, CodecChannel, CodecVersionsPdu, FrameBeginPdu, FrameEndPdu,
    OperatingMode, Quant, RegionPdu, RfxChannel, SyncPdu, TileSetPdu,
};
use ironrdp_pdu::rdp::capability_sets::EntropyBits;
use ironrdp_pdu::WriteCursor;

use crate::BitmapUpdate;

#[cfg(test)]
#[path = "rfx_reuse_tests.rs"]
mod reuse_tests;

// Retention limits are independent of desktop size and peer capability. Active
// candidates are bounded separately by UpdateLimits before entering the codec.
pub(super) const MAX_CACHED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct RfxEncoder {
    entropy_algorithm: rfx::EntropyAlgorithm,
    quant: Quant,
    mode: OperatingMode,
    frame_index: u32,
    data: UpdateEncoderData,
    #[cfg(test)]
    pub(super) compressed_tiles: usize,
}

impl RfxEncoder {
    pub(crate) fn new(
        entropy_bits: EntropyBits,
        quality: crate::BitmapQuality,
        mode: OperatingMode,
    ) -> Self {
        let entropy_algorithm = match entropy_bits {
            EntropyBits::Rlgr1 => rfx::EntropyAlgorithm::Rlgr1,
            EntropyBits::Rlgr3 => rfx::EntropyAlgorithm::Rlgr3,
        };
        Self {
            entropy_algorithm,
            quant: quality.remotefx_quant(),
            mode,
            frame_index: 0,
            data: UpdateEncoderData::default(),
            #[cfg(test)]
            compressed_tiles: 0,
        }
    }

    pub(crate) fn is_image_mode(&self) -> bool {
        self.mode.contains(OperatingMode::IMAGE_MODE)
    }

    pub(crate) fn reset(&mut self) {
        self.frame_index = 0;
    }

    pub(crate) fn encode(
        &mut self,
        bitmap: &BitmapUpdate,
        output: &mut Vec<u8>,
        desktop_size: Option<DesktopSize>,
        max_output: usize,
    ) -> EncodeResult<Option<usize>> {
        let result = self.encode_prepared(bitmap, output, desktop_size, max_output);
        self.data.trim_cache();
        result
    }

    fn encode_prepared(
        &mut self,
        bitmap: &BitmapUpdate,
        output: &mut Vec<u8>,
        desktop_size: Option<DesktopSize>,
        max_output: usize,
    ) -> EncodeResult<Option<usize>> {
        let entropy_algorithm = self.entropy_algorithm;
        let encoder = UpdateEncoder::new(bitmap, self.quant.clone(), entropy_algorithm);
        let (tiles_x, tiles_y) = encoder.tiles_xy();
        let count = tiles_x * tiles_y;
        if self.data.0.len() < count {
            self.data.0.resize_with(count, TileData::default);
        }
        #[cfg(test)]
        {
            self.compressed_tiles += count;
        }
        let tiles = encoder.encode(&mut self.data.0[..count])?;
        let mut blocks = Vec::with_capacity(8);

        // MS-RDPRFX 3.1.5 requires headers before every image; 3.1.8.3.1
        // permits repeating them in video mode too. Every partition remains
        // independently decodable, including after an oversized candidate.
        if let Some(desktop_size) = desktop_size {
            let width = desktop_size.width;
            let height = desktop_size.height;
            blocks.push(Block::Sync(SyncPdu));
            let context = rfx::ContextPdu {
                flags: self.mode,
                entropy_algorithm,
            };
            blocks.push(Block::CodecChannel(CodecChannel::Context(context)));

            let channels = ChannelsPdu(vec![RfxChannel {
                width: cast_length!("width", width)?,
                height: cast_length!("height", height)?,
            }]);
            blocks.push(Block::Channels(channels));

            blocks.push(Block::CodecVersions(CodecVersionsPdu));
        }

        // data messages
        let frame_begin = FrameBeginPdu {
            index: self.frame_index,
            number_of_regions: 1,
        };
        blocks.push(Block::CodecChannel(CodecChannel::FrameBegin(frame_begin)));

        let width = bitmap.width.get();
        let height = bitmap.height.get();
        let rectangles = vec![rfx::RfxRectangle {
            x: 0,
            y: 0,
            width,
            height,
        }];
        let region = RegionPdu { rectangles };
        blocks.push(Block::CodecChannel(CodecChannel::Region(region)));

        let quants = vec![self.quant.clone()];
        let tile_set = TileSetPdu {
            entropy_algorithm,
            quants,
            tiles,
        };
        blocks.push(Block::CodecChannel(CodecChannel::TileSet(tile_set)));

        let frame_end = FrameEndPdu;
        blocks.push(Block::CodecChannel(CodecChannel::FrameEnd(frame_end)));

        // Compression is complete. Protocol sizes depend only on the resulting
        // component lengths, so output growth never repeats DWT/RLGR work.
        let len = blocks.iter().try_fold(0usize, |size, block| {
            size.checked_add(block.size())
                .ok_or_else(|| other_err!("rfxenc", "serialized size overflow"))
        })?;
        if len > max_output {
            return Ok(None);
        }
        output.resize(len, 0);
        let mut cursor = WriteCursor::new(output);
        for block in blocks {
            block.encode(&mut cursor)?;
        }
        debug_assert_eq!(cursor.pos(), len);
        Ok(Some(len))
    }

    pub(super) fn commit_frame(&mut self) {
        if !self.is_image_mode() {
            self.frame_index = self.frame_index.wrapping_add(1);
        }
    }
}

pub(crate) struct UpdateEncoder<'a> {
    bitmap: &'a BitmapUpdate,
    quant: Quant,
    entropy_algorithm: rfx::EntropyAlgorithm,
}

#[derive(Debug, Default)]
struct UpdateEncoderData(Vec<TileData>);

impl UpdateEncoderData {
    fn retained_bytes(&self) -> usize {
        self.0.capacity() * core::mem::size_of::<TileData>()
            + self
                .0
                .iter()
                .map(|tile| tile.y.capacity() + tile.cb.capacity() + tile.cr.capacity())
                .sum::<usize>()
    }

    fn trim_cache(&mut self) {
        if self.retained_bytes() <= MAX_CACHED_BYTES {
            return;
        }
        let mut bytes = 0usize;
        let count = self
            .0
            .iter()
            .take_while(|tile| {
                bytes += core::mem::size_of::<TileData>()
                    + tile.y.capacity()
                    + tile.cb.capacity()
                    + tile.cr.capacity();
                bytes <= MAX_CACHED_BYTES
            })
            .count();
        if count < self.0.len() {
            self.0.truncate(count);
            self.0.shrink_to_fit();
        }
        // Allocators may retain spare capacity even after shrink_to_fit.
        if self.retained_bytes() > MAX_CACHED_BYTES {
            self.0 = Vec::new();
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TileData {
    y: Vec<u8>,
    cb: Vec<u8>,
    cr: Vec<u8>,
}

struct EncodedTile<'a> {
    y_data: &'a [u8],
    cb_data: &'a [u8],
    cr_data: &'a [u8],
}

impl<'a> UpdateEncoder<'a> {
    fn new(
        bitmap: &'a BitmapUpdate,
        quant: Quant,
        entropy_algorithm: rfx::EntropyAlgorithm,
    ) -> Self {
        Self {
            bitmap,
            quant,
            entropy_algorithm,
        }
    }

    fn tiles_xy(&self) -> (usize, usize) {
        (
            self.bitmap.width.get().div_ceil(64).into(),
            self.bitmap.height.get().div_ceil(64).into(),
        )
    }

    fn encode<'b>(&self, data: &'b mut [TileData]) -> EncodeResult<Vec<rfx::Tile<'b>>> {
        #[cfg(feature = "rayon")]
        use rayon::prelude::*;

        let (tiles_x, tiles_y) = self.tiles_xy();

        #[cfg(not(feature = "rayon"))]
        let chunks = data.iter_mut();
        #[cfg(feature = "rayon")]
        let chunks = data.par_iter_mut();

        let tiles: Vec<_> = (0..tiles_y)
            .flat_map(|y| (0..tiles_x).map(move |x| (x, y)))
            .collect();

        chunks
            .zip(tiles)
            .map(|(buf, (tile_x, tile_y))| {
                let EncodedTile {
                    y_data,
                    cb_data,
                    cr_data,
                } = self
                    .encode_tile(tile_x, tile_y, buf)
                    .map_err(|e| other_err!("rfxenc", source: e))?;

                let tile = rfx::Tile {
                    y_quant_index: 0,
                    cb_quant_index: 0,
                    cr_quant_index: 0,
                    x: cast_int!("tile_x", tile_x)?,
                    y: cast_int!("tile_y", tile_y)?,
                    y_data,
                    cb_data,
                    cr_data,
                };
                Ok(tile)
            })
            .collect()
    }

    fn encode_tile<'b>(
        &self,
        tile_x: usize,
        tile_y: usize,
        buf: &'b mut TileData,
    ) -> Result<EncodedTile<'b>, RlgrError> {
        #![allow(clippy::similar_names)] // It’s hard to find better names for cr, cb, etc.

        let bpp: usize = self.bitmap.format.bytes_per_pixel().into();
        let width: usize = self.bitmap.width.get().into();
        let height: usize = self.bitmap.height.get().into();

        let x = tile_x * 64;
        let y = tile_y * 64;
        let tile_width =
            u32::try_from(core::cmp::min(width - x, 64)).expect("can always fit in u32");
        let tile_height =
            u32::try_from(core::cmp::min(height - y, 64)).expect("can always fit in u32");
        let stride = self.bitmap.stride.get();
        let input = &self.bitmap.data[y * stride + x * bpp..];

        let stride = u32::try_from(stride).map_err(io::Error::other)?;
        let y = &mut [0i16; 4096];
        let cb = &mut [0i16; 4096];
        let cr = &mut [0i16; 4096];

        to_64x64_ycbcr_tile(
            input,
            tile_width,
            tile_height,
            stride,
            self.bitmap.format,
            y,
            cb,
            cr,
        )
        .map_err(RlgrError::Yuv)?;

        self.encode_component(y, &mut buf.y)?;
        self.encode_component(cb, &mut buf.cb)?;
        self.encode_component(cr, &mut buf.cr)?;

        Ok(EncodedTile {
            y_data: &buf.y,
            cb_data: &buf.cb,
            cr_data: &buf.cr,
        })
    }

    fn encode_component(
        &self,
        input: &mut [i16; 4096],
        output: &mut Vec<u8>,
    ) -> Result<(), RlgrError> {
        let mut temp = [0i16; 4096];
        ironrdp_graphics::dwt::encode(input, &mut temp);
        ironrdp_graphics::quantization::encode(input, &self.quant);
        ironrdp_graphics::subband_reconstruction::encode(&mut input[4032..]);
        super::rfx_rlgr::encode(self.entropy_algorithm, input, output);
        Ok(())
    }
}

#[cfg(feature = "__bench")]
#[expect(clippy::missing_panics_doc, reason = "panics in benches are allowed")]
pub(crate) mod bench {
    use super::*;

    pub fn rfx_enc_tile(
        bitmap: &BitmapUpdate,
        quant: &Quant,
        algo: rfx::EntropyAlgorithm,
        tile_x: usize,
        tile_y: usize,
    ) {
        let enc = UpdateEncoder::new(bitmap, quant.clone(), algo);
        let mut data = TileData::default();

        enc.encode_tile(tile_x, tile_y, &mut data)
            .expect("cannot propagate error in benchmark");
    }

    pub fn rfx_enc(bitmap: &BitmapUpdate, quant: &Quant, algo: rfx::EntropyAlgorithm) {
        let enc = UpdateEncoder::new(bitmap, quant.clone(), algo);
        let (tiles_x, tiles_y) = enc.tiles_xy();
        let mut data = vec![TileData::default(); tiles_x * tiles_y];

        enc.encode(&mut data)
            .expect("cannot propagate error in benchmark");
    }
}
