use super::*;
use crate::BitmapQuality;
use bytes::Bytes;
use core::num::NonZeroUsize;
use ironrdp_core::{decode_cursor, ReadCursor};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use ironrdp_pdu::bitmap::BitmapUpdateData;
use ironrdp_pdu::codecs::rfx::{Block, CodecChannel, EntropyAlgorithm, Quant};

pub(super) fn fixture(width: u16, height: u16, noise: bool) -> BitmapUpdate {
    let stride = usize::from(width) * 4 + 12;
    let mut data = vec![0; stride * usize::from(height - 1) + usize::from(width) * 4];
    let mut seed = 0x1234_5678u32;
    for y in 0..usize::from(height) {
        for x in 0..usize::from(width) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let color = if noise {
                seed.to_le_bytes()
            } else {
                // Dark gradients with fine colored lines, plus hard edges.
                let edge = if x % 13 == 0 || y % 17 == 0 { 120 } else { 0 };
                [
                    ((x + y) % 64 + edge) as u8,
                    ((2 * x + y) % 64) as u8,
                    ((x + 3 * y) % 64) as u8,
                    255,
                ]
            };
            let offset = y * stride + x * 4;
            data[offset..offset + 4].copy_from_slice(&color);
        }
    }
    BitmapUpdate {
        x: 7,
        y: 9,
        width: NonZeroU16::new(width).unwrap(),
        height: NonZeroU16::new(height).unwrap(),
        format: PixelFormat::BgrA32,
        data: Bytes::from(data),
        stride: NonZeroUsize::new(stride).unwrap(),
        dirty_rects: Vec::new(),
    }
}

fn decode_component(data: &[u8], quant: &Quant, entropy: EntropyAlgorithm) -> [i16; 4096] {
    let mut output = [0; 4096];
    ironrdp_graphics::rlgr::decode(entropy, data, &mut output).unwrap();
    ironrdp_graphics::subband_reconstruction::decode(&mut output[4032..]);
    ironrdp_graphics::quantization::decode(&mut output, quant);
    ironrdp_graphics::dwt::decode(&mut output, &mut [0; 4096]);
    output
}

fn remotefx_error(bitmap: &BitmapUpdate, quality: BitmapQuality, entropy: EntropyBits) -> u64 {
    let mut handler = RemoteFxHandler::new(
        entropy,
        3,
        DesktopSize {
            width: 64,
            height: 64,
        },
        quality,
        OperatingMode::IMAGE_MODE,
    );
    let output = handler.handle(bitmap).unwrap();
    let surface: SurfaceCommand<'_> = ironrdp_core::decode(&output.data).unwrap();
    let SurfaceCommand::SetSurfaceBits(surface) = surface else {
        panic!("expected surface bits")
    };
    let mut cursor = ReadCursor::new(surface.extended_bitmap_data.data);
    let mut error = None;
    while !cursor.is_empty() {
        let block: Block<'_> = decode_cursor(&mut cursor).unwrap();
        if let Block::CodecChannel(CodecChannel::TileSet(set)) = block {
            assert_eq!(set.tiles.len(), 1);
            assert_eq!(set.quants, vec![quality.remotefx_quant()]);
            let tile = &set.tiles[0];
            let quant = &set.quants[0];
            let (mut ey, mut ecb, mut ecr) = ([0i16; 4096], [0i16; 4096], [0i16; 4096]);
            ironrdp_graphics::color_conversion::to_64x64_ycbcr_tile(
                &bitmap.data,
                64,
                64,
                bitmap.stride.get() as u32,
                bitmap.format,
                &mut ey,
                &mut ecb,
                &mut ecr,
            )
            .unwrap();
            for (name, mut expected, bytes) in [
                ("y", ey, tile.y_data),
                ("cb", ecb, tile.cb_data),
                ("cr", ecr, tile.cr_data),
            ] {
                ironrdp_graphics::dwt::encode(&mut expected, &mut [0; 4096]);
                ironrdp_graphics::quantization::encode(&mut expected, quant);
                ironrdp_graphics::subband_reconstruction::encode(&mut expected[4032..]);
                let mut actual = [0i16; 4096];
                ironrdp_graphics::rlgr::decode(set.entropy_algorithm, bytes, &mut actual).unwrap();
                if let Some(index) = actual.iter().zip(&expected).position(|(a, b)| a != b) {
                    panic!("RLGR mismatch quality={quality:?} entropy={entropy:?} plane={name} index={index}: actual {:?}, expected {:?}", &actual[index..(index+12).min(4096)], &expected[index..(index+12).min(4096)]);
                }
            }
            let y = decode_component(tile.y_data, quant, set.entropy_algorithm);
            let cb = decode_component(tile.cb_data, quant, set.entropy_algorithm);
            let cr = decode_component(tile.cr_data, quant, set.entropy_algorithm);
            // Scalar inverse RemoteFX color transform using widened arithmetic.
            // Clamp after reconstruction without depending on the platform's
            // overflow behavior for intermediate fixed-point products.
            let mut rgba = vec![0; 4096 * 4];
            for index in 0..4096 {
                let luma = (i64::from(y[index]) + 4096) * 65536;
                let u = i64::from(cb[index]);
                let v = i64::from(cr[index]);
                let clamp = |value: i64| (value >> 21).clamp(0, 255) as u8;
                rgba[index * 4] = clamp(luma + 91915 * v);
                rgba[index * 4 + 1] = clamp(luma - 46818 * v - 22526 * u);
                rgba[index * 4 + 2] = clamp(luma + 115992 * u);
                rgba[index * 4 + 3] = 255;
            }
            let mut squared_error = 0u64;
            for row in 0..64 {
                for column in 0..64 {
                    let source = row * bitmap.stride.get() + column * 4;
                    let dest = (row * 64 + column) * 4;
                    for channel in 0..3 {
                        let difference = i64::from(bitmap.data[source + 2 - channel])
                            - i64::from(rgba[dest + channel]);
                        squared_error += (difference * difference) as u64;
                    }
                }
            }
            error = Some(squared_error);
        }
    }
    error.expect("RemoteFX tile set")
}

#[test]
fn high_quality_reduces_decoded_gradient_error_in_both_entropy_modes() {
    let bitmap = fixture(64, 64, false);
    for entropy in [EntropyBits::Rlgr1, EntropyBits::Rlgr3] {
        let performance = remotefx_error(&bitmap, BitmapQuality::Performance, entropy);
        let balanced = remotefx_error(&bitmap, BitmapQuality::Balanced, entropy);
        let high = remotefx_error(&bitmap, BitmapQuality::HighQuality, entropy);
        assert!(high < balanced, "high={high}, balanced={balanced}");
        assert!(
            balanced < performance,
            "balanced={balanced}, performance={performance}"
        );
    }
}

#[test]
fn high_quality_incompressible_tiles_fit_measured_component_buffers() {
    let bitmap = fixture(64, 64, true);
    for entropy in [EntropyBits::Rlgr1, EntropyBits::Rlgr3] {
        // The old fixed 4096-byte RLGR buffers can panic on these components.
        let error = remotefx_error(&bitmap, BitmapQuality::HighQuality, entropy);
        assert!(error < 4096 * 3 * 100);
    }
}

#[test]
fn tiny_remotefx_frame_keeps_initialization_headers_across_output_growth() {
    let bitmap = fixture(1, 1, false);
    let mut handler = RemoteFxHandler::new(
        EntropyBits::Rlgr3,
        3,
        DesktopSize {
            width: 64,
            height: 64,
        },
        BitmapQuality::HighQuality,
        OperatingMode::IMAGE_MODE,
    );
    for new_size in [
        None,
        Some(DesktopSize {
            width: 128,
            height: 96,
        }),
    ] {
        if let Some(size) = new_size {
            handler.set_desktop_size(size);
        }
        let output = handler.handle(&bitmap).unwrap();
        let surface: SurfaceCommand<'_> = ironrdp_core::decode(&output.data).unwrap();
        let SurfaceCommand::SetSurfaceBits(surface) = surface else {
            panic!("expected surface bits")
        };
        let mut cursor = ReadCursor::new(surface.extended_bitmap_data.data);
        let first: Block<'_> = decode_cursor(&mut cursor).unwrap();
        assert!(matches!(first, Block::Sync(_)));
        assert!(handler.desktop_size.is_some());
    }
}

#[test]
fn lossless_overrides_surface_codecs_and_roundtrips_rgb_across_bitmap_chunks() {
    for (noise, skip_alpha) in [(false, false), (false, true), (true, false), (true, true)] {
        let bitmap = fixture(257, 97, noise);
        let mut codecs = UpdateEncoderCodecs::new();
        codecs.set_quality(BitmapQuality::Lossless);
        codecs.set_allow_skip_alpha(skip_alpha);
        codecs.set_remotefx(Some((EntropyBits::Rlgr3, 3, OperatingMode::IMAGE_MODE)));
        #[cfg(feature = "qoi")]
        codecs.set_qoi(Some(4));
        #[cfg(feature = "qoiz")]
        codecs.set_qoiz(Some(5));
        let mut encoder = UpdateEncoder::new(
            DesktopSize {
                width: 300,
                height: 200,
            },
            CmdFlags::SET_SURFACE_BITS,
            codecs,
        )
        .unwrap();
        assert_eq!(encoder.bitmap_codec(), "rdp6-rle");
        let output = encoder
            .bitmap_updater
            .as_mut()
            .unwrap()
            .handle(&bitmap)
            .unwrap();
        let update: BitmapUpdateData<'_> = ironrdp_core::decode(&output.data).unwrap();
        assert!(update.rectangles.len() > 1);
        let mut pixel_count = 0;
        for rectangle in update.rectangles {
            assert_eq!(rectangle.bits_per_pixel, 32);
            let header: ironrdp_pdu::bitmap::rdp6::BitmapStreamHeader =
                ironrdp_core::decode(rectangle.bitmap_data).unwrap();
            assert_eq!(header.use_alpha, !skip_alpha);
            assert!(header.enable_rle_compression);
            assert_eq!(
                header.color_plane_definition,
                ironrdp_pdu::bitmap::rdp6::ColorPlaneDefinition::Argb
            );
            let mut rgb = Vec::new();
            BitmapStreamDecoder::default()
                .decode_bitmap_stream_to_rgb24(
                    rectangle.bitmap_data,
                    &mut rgb,
                    usize::from(rectangle.width),
                    usize::from(rectangle.height),
                )
                .unwrap();
            for row in 0..usize::from(rectangle.height) {
                let source_row = usize::from(rectangle.rectangle.bottom - bitmap.y) - row;
                for column in 0..usize::from(rectangle.width) {
                    let source = source_row * bitmap.stride.get() + column * 4;
                    let dest = (row * usize::from(rectangle.width) + column) * 3;
                    assert_eq!(
                        &rgb[dest..dest + 3],
                        &[
                            bitmap.data[source + 2],
                            bitmap.data[source + 1],
                            bitmap.data[source]
                        ]
                    );
                    pixel_count += 1;
                }
            }
        }
        assert_eq!(
            pixel_count,
            usize::from(bitmap.width.get()) * usize::from(bitmap.height.get())
        );
    }
}

#[test]
fn alpha_plane_for_narrow_tall_updates_stays_within_wire_length_limits() {
    let bitmap = fixture(1, 8192, true);
    for skip_alpha in [false, true] {
        let mut handler = BitmapHandler::new();
        handler.bitmap.set_skip_alpha(skip_alpha);
        let output = handler.handle(&bitmap).unwrap();
        let update: BitmapUpdateData<'_> = ironrdp_core::decode(&output.data).unwrap();
        let rows: usize = update
            .rectangles
            .iter()
            .map(|rect| usize::from(rect.height))
            .sum();
        assert_eq!(rows, 8192);
        if !skip_alpha {
            assert!(update.rectangles.len() > 1);
        }
        for rectangle in update.rectangles {
            let mut rgb = Vec::new();
            BitmapStreamDecoder::default()
                .decode_bitmap_stream_to_rgb24(
                    rectangle.bitmap_data,
                    &mut rgb,
                    1,
                    usize::from(rectangle.height),
                )
                .unwrap();
            for (row, pixel) in rgb.chunks_exact(3).enumerate() {
                let source_row = usize::from(rectangle.rectangle.bottom - bitmap.y) - row;
                let source = source_row * bitmap.stride.get();
                assert_eq!(
                    pixel,
                    [
                        bitmap.data[source + 2],
                        bitmap.data[source + 1],
                        bitmap.data[source]
                    ]
                );
            }
        }
    }
}
