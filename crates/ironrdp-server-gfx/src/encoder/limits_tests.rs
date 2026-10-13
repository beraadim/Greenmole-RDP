use super::*;
use ironrdp_core::{decode, decode_cursor, ReadCursor};
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use ironrdp_pdu::bitmap::BitmapUpdateData;
use ironrdp_pdu::codecs::rfx::{Block, CodecChannel};
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdatePdu, Fragmentation};

use super::quality_tests::fixture;

#[test]
fn negotiated_icap_preserves_mode_entropy_and_client_codec_id_together() {
    for flags in [RfxICapFlags::empty(), RfxICapFlags::CODEC_MODE] {
        for entropy_bits in [EntropyBits::Rlgr1, EntropyBits::Rlgr3] {
            let wire = encode_vec(&RfxICap {
                flags,
                entropy_bits,
            })
            .unwrap();
            let capability: RfxICap = decode(&wire).unwrap();
            let mut codecs = UpdateEncoderCodecs::new();
            codecs.set_remotefx_capability(17, capability);
            let mut encoder = UpdateEncoder::new(
                DesktopSize {
                    width: 64,
                    height: 64,
                },
                CmdFlags::SET_SURFACE_BITS,
                codecs,
            )
            .unwrap();
            let output = encoder
                .bitmap_updater
                .as_mut()
                .unwrap()
                .handle(&fixture(1, 1, false))
                .unwrap();
            let SurfaceCommand::SetSurfaceBits(surface) = decode(&output.data).unwrap() else {
                panic!("surface")
            };
            assert_eq!(surface.extended_bitmap_data.codec_id, 17);
            let mut cursor = ReadCursor::new(surface.extended_bitmap_data.data);
            let mut checked = false;
            while !cursor.is_empty() {
                if let Block::CodecChannel(CodecChannel::Context(context)) =
                    decode_cursor(&mut cursor).unwrap()
                {
                    assert_eq!(
                        context.flags.contains(OperatingMode::IMAGE_MODE),
                        flags.contains(RfxICapFlags::CODEC_MODE)
                    );
                    let expected = match entropy_bits {
                        EntropyBits::Rlgr1 => ironrdp_pdu::codecs::rfx::EntropyAlgorithm::Rlgr1,
                        EntropyBits::Rlgr3 => ironrdp_pdu::codecs::rfx::EntropyAlgorithm::Rlgr3,
                    };
                    assert_eq!(context.entropy_algorithm, expected);
                    checked = true;
                }
            }
            assert!(checked);
        }
    }
}

fn reassemble(mut update: UpdateFragmenter, limit: usize) -> Vec<u8> {
    assert!(update.data.len() <= limit);
    let expected = update.data.clone();
    let mut output = Vec::new();
    let mut packet = vec![0; update.size_hint()];
    let mut fragments = Vec::new();
    while let Some(len) = update.next(&mut packet) {
        let mut cursor = ReadCursor::new(&packet[..len]);
        let _: FastPathHeader = decode_cursor(&mut cursor).unwrap();
        let pdu: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
        fragments.push(pdu.fragmentation);
        output.extend_from_slice(pdu.data);
        assert!(output.len() <= limit, "client reassembly buffer exceeded");
    }
    if fragments.len() == 1 {
        assert!(matches!(fragments[0], Fragmentation::Single));
    } else {
        assert!(matches!(fragments[0], Fragmentation::First));
        assert!(matches!(fragments.last(), Some(Fragmentation::Last)));
        assert!(fragments[1..fragments.len() - 1]
            .iter()
            .all(|flag| matches!(flag, Fragmentation::Next)));
    }
    assert_eq!(output, expected);
    output
}

/// Start with fresh decoder state for every partition. Check headers before
/// tiles and entropy-decode all coefficients against the corresponding source
/// pixels, including padded edge tiles and nonzero destination coordinates.
fn check_remotefx(
    bytes: &[u8],
    source: &BitmapUpdate,
    mode: OperatingMode,
    frame_index: u32,
    covered: &mut [u8],
) {
    let SurfaceCommand::SetSurfaceBits(surface) = decode(bytes).unwrap() else {
        panic!("expected surface bits")
    };
    let width = usize::from(surface.extended_bitmap_data.width);
    let height = usize::from(surface.extended_bitmap_data.height);
    let x = usize::from(surface.destination.left - source.x);
    let y = usize::from(surface.destination.top - source.y);
    assert_eq!(
        usize::from(surface.destination.right - surface.destination.left),
        width
    );
    assert_eq!(
        usize::from(surface.destination.bottom - surface.destination.top),
        height
    );
    assert_eq!(surface.extended_bitmap_data.codec_id, 3);
    let mut cursor = ReadCursor::new(surface.extended_bitmap_data.data);
    let first: Block<'_> = decode_cursor(&mut cursor).unwrap();
    assert!(matches!(first, Block::Sync(_)), "missing standalone header");
    let (mut context, mut channels, mut versions, mut begun, mut region, mut tiles, mut ended) =
        (false, false, false, false, false, false, false);
    while !cursor.is_empty() {
        match decode_cursor::<Block<'_>>(&mut cursor).unwrap() {
            Block::CodecChannel(CodecChannel::Context(value)) => {
                assert!(!begun);
                assert_eq!(value.flags, mode);
                context = true;
            }
            Block::Channels(_) => {
                assert!(!begun);
                channels = true;
            }
            Block::CodecVersions(_) => {
                assert!(!begun);
                versions = true;
            }
            Block::CodecChannel(CodecChannel::FrameBegin(value)) => {
                assert!(context && channels && versions && !begun);
                assert_eq!(value.index, frame_index);
                begun = true;
            }
            Block::CodecChannel(CodecChannel::Region(_)) => {
                assert!(begun && !region && !ended);
                region = true;
            }
            Block::CodecChannel(CodecChannel::TileSet(set)) => {
                assert!(region && !tiles && !ended);
                tiles = true;
                for tile in set.tiles {
                    let tile_x = usize::from(tile.x) * 64;
                    let tile_y = usize::from(tile.y) * 64;
                    assert!(tile_x < width && tile_y < height);
                    let tile_width = (width - tile_x).min(64);
                    let tile_height = (height - tile_y).min(64);
                    let offset = (y + tile_y) * source.stride.get() + (x + tile_x) * 4;
                    let (mut luma, mut cb, mut cr) = ([0; 4096], [0; 4096], [0; 4096]);
                    ironrdp_graphics::color_conversion::to_64x64_ycbcr_tile(
                        &source.data[offset..],
                        tile_width as u32,
                        tile_height as u32,
                        source.stride.get() as u32,
                        source.format,
                        &mut luma,
                        &mut cb,
                        &mut cr,
                    )
                    .unwrap();
                    for (mut expected, bytes, quant_index) in [
                        (luma, tile.y_data, tile.y_quant_index),
                        (cb, tile.cb_data, tile.cb_quant_index),
                        (cr, tile.cr_data, tile.cr_quant_index),
                    ] {
                        let quant = &set.quants[usize::from(quant_index)];
                        ironrdp_graphics::dwt::encode(&mut expected, &mut [0; 4096]);
                        ironrdp_graphics::quantization::encode(&mut expected, quant);
                        ironrdp_graphics::subband_reconstruction::encode(&mut expected[4032..]);
                        let mut decoded = [0; 4096];
                        ironrdp_graphics::rlgr::decode(set.entropy_algorithm, bytes, &mut decoded)
                            .unwrap();
                        assert_eq!(decoded, expected, "partition source pixels changed");
                    }
                    for row in 0..tile_height {
                        for column in 0..tile_width {
                            let index = (y + tile_y + row) * usize::from(source.width.get())
                                + x
                                + tile_x
                                + column;
                            covered[index] += 1;
                        }
                    }
                }
            }
            Block::CodecChannel(CodecChannel::FrameEnd(_)) => {
                assert!(tiles && !ended);
                ended = true;
            }
            other => panic!("unexpected RFX block: {other:?}"),
        }
    }
    assert!(ended);
}

#[tokio::test]
async fn noisy_remotefx_updates_fit_small_client_limits_and_decode_independently() {
    let bitmap = fixture(513, 257, true);
    for mode in [OperatingMode::IMAGE_MODE, OperatingMode::empty()] {
        for limit in [4096, 32_768] {
            let mut codecs = UpdateEncoderCodecs::new();
            codecs.set_remotefx(Some((EntropyBits::Rlgr3, 3, mode)));
            codecs.set_quality(crate::BitmapQuality::HighQuality);
            codecs.set_max_request_size(limit);
            let mut encoder = UpdateEncoder::new(
                DesktopSize {
                    width: 640,
                    height: 480,
                },
                CmdFlags::SET_SURFACE_BITS,
                codecs,
            )
            .unwrap();
            let mut updates = encoder.update(DisplayUpdate::Bitmap(bitmap.clone()));
            let mut covered = vec![0u8; 513 * 257];
            let mut count = 0;
            while let Some(update) = updates.next().await {
                let bytes = reassemble(update.unwrap(), limit as usize);
                let index = if mode.contains(OperatingMode::IMAGE_MODE) {
                    0
                } else {
                    count
                };
                check_remotefx(&bytes, &bitmap, mode, index, &mut covered);
                count += 1;
            }
            assert!(count > 1);
            assert!(covered.iter().all(|count| *count == 1));
        }
    }
}

#[test]
fn consecutive_remotefx_images_keep_headers_and_video_indices_reset_on_resize() {
    for mode in [OperatingMode::IMAGE_MODE, OperatingMode::empty()] {
        let mut handler = RemoteFxHandler::new(
            EntropyBits::Rlgr1,
            3,
            DesktopSize {
                width: 64,
                height: 64,
            },
            crate::BitmapQuality::Balanced,
            mode,
        );
        let bitmap = fixture(1, 1, false);
        for index in 0..3 {
            let output = handler.handle(&bitmap).unwrap();
            check_remotefx(
                &output.data,
                &bitmap,
                mode,
                if mode.contains(OperatingMode::IMAGE_MODE) {
                    0
                } else {
                    index
                },
                &mut [0],
            );
        }
        handler.set_desktop_size(DesktopSize {
            width: 128,
            height: 96,
        });
        let output = handler.handle(&bitmap).unwrap();
        check_remotefx(&output.data, &bitmap, mode, 0, &mut [0]);
    }
}

#[tokio::test]
async fn lossless_and_raw_partitions_cover_every_pixel_with_small_or_absent_capability() {
    let bitmap = fixture(417, 129, true);
    for raw in [false, true] {
        for limit in [0, 1024] {
            let mut codecs = UpdateEncoderCodecs::new();
            codecs.set_max_request_size(limit);
            let actual_limit = codecs.max_update_size();
            let mut encoder = UpdateEncoder::new(
                DesktopSize {
                    width: 640,
                    height: 480,
                },
                if raw {
                    CmdFlags::SET_SURFACE_BITS
                } else {
                    CmdFlags::empty()
                },
                codecs,
            )
            .unwrap();
            let mut updates = encoder.update(DisplayUpdate::Bitmap(bitmap.clone()));
            let mut covered = vec![0u8; 417 * 129];
            while let Some(update) = updates.next().await {
                let bytes = reassemble(update.unwrap(), actual_limit);
                if raw {
                    let SurfaceCommand::SetSurfaceBits(surface) = decode(&bytes).unwrap() else {
                        panic!("raw surface")
                    };
                    let width = usize::from(surface.extended_bitmap_data.width);
                    let height = usize::from(surface.extended_bitmap_data.height);
                    let left = usize::from(surface.destination.left - bitmap.x);
                    let top = usize::from(surface.destination.top - bitmap.y);
                    assert_eq!(surface.extended_bitmap_data.codec_id, 0);
                    for row in 0..height {
                        for col in 0..width {
                            let y = top + height - 1 - row;
                            let x = left + col;
                            let source = y * bitmap.stride.get() + x * 4;
                            let target = (row * width + col) * 4;
                            assert_eq!(
                                &surface.extended_bitmap_data.data[target..target + 4],
                                &bitmap.data[source..source + 4]
                            );
                            covered[y * 417 + x] += 1;
                        }
                    }
                } else {
                    let update: BitmapUpdateData<'_> = decode(&bytes).unwrap();
                    for rectangle in update.rectangles {
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
                            for col in 0..usize::from(rectangle.width) {
                                let y = usize::from(rectangle.rectangle.bottom - bitmap.y) - row;
                                let x = usize::from(rectangle.rectangle.left - bitmap.x) + col;
                                let source = y * bitmap.stride.get() + x * 4;
                                let target = (row * usize::from(rectangle.width) + col) * 3;
                                assert_eq!(
                                    &rgb[target..target + 3],
                                    &[
                                        bitmap.data[source + 2],
                                        bitmap.data[source + 1],
                                        bitmap.data[source]
                                    ]
                                );
                                covered[y * 417 + x] += 1;
                            }
                        }
                    }
                }
            }
            assert!(covered.iter().all(|count| *count == 1));
        }
    }
}

#[tokio::test]
async fn impossible_limits_fail_without_unbounded_splitting_and_large_cursors_fall_back() {
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_max_request_size(1);
    let mut encoder = UpdateEncoder::new(
        DesktopSize {
            width: 64,
            height: 64,
        },
        CmdFlags::empty(),
        codecs,
    )
    .unwrap();
    let mut updates = encoder.update(DisplayUpdate::Bitmap(fixture(1, 1, false)));
    assert!(updates.next().await.unwrap().is_err());
    assert!(updates.next().await.is_none());
    let mut updates = encoder.update(DisplayUpdate::RGBAPointer(RGBAPointer {
        width: 2,
        height: 2,
        hot_x: 0,
        hot_y: 0,
        data: vec![255; 16],
    }));
    let mut pointer = updates.next().await.unwrap().unwrap();
    let mut packet = vec![0; pointer.size_hint()];
    let len = pointer.next(&mut packet).unwrap();
    let mut cursor = ReadCursor::new(&packet[..len]);
    let _: FastPathHeader = decode_cursor(&mut cursor).unwrap();
    let pdu: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
    assert!(matches!(pdu.update_code, UpdateCode::DefaultPointer));
}
