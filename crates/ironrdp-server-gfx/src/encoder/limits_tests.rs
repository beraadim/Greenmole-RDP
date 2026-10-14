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

#[tokio::test]
async fn adaptive_bitmap_budgets_preserve_hard_caps_and_independent_noisy_updates() {
    let bitmap = fixture(321, 129, true);
    for entropy in [EntropyBits::Rlgr1, EntropyBits::Rlgr3] {
        let mut codecs = UpdateEncoderCodecs::new();
        codecs.set_remotefx(Some((entropy, 3, OperatingMode::IMAGE_MODE)));
        codecs.set_quality(crate::BitmapQuality::HighQuality);
        codecs.set_max_request_size(128 * 1024);
        let mut encoder = UpdateEncoder::new(
            DesktopSize {
                width: 640,
                height: 480,
            },
            CmdFlags::SET_SURFACE_BITS,
            codecs,
        )
        .unwrap();
        assert_eq!(encoder.set_bitmap_working_budget(usize::MAX), 128 * 1024);
        for requested in [1, 64 * 1024, 0] {
            let budget = encoder.set_bitmap_working_budget(requested);
            assert_eq!(
                budget,
                if requested == 0 {
                    128 * 1024
                } else {
                    requested.max(16 * 1024)
                }
            );
            assert_eq!(budget, encoder.bitmap_working_budget());
            let mut updates = encoder.update(DisplayUpdate::Bitmap(bitmap.clone()));
            let mut covered = vec![0u8; 321 * 129];
            while let Some(update) = updates.next().await {
                let bytes = reassemble(update.unwrap(), budget);
                check_remotefx(&bytes, &bitmap, OperatingMode::IMAGE_MODE, 0, &mut covered);
            }
            assert!(covered.iter().all(|count| *count == 1));
        }
    }
    let mut limits = UpdateLimits::from_client(4096);
    assert_eq!(limits.set_bitmap_budget(1), 4096);
    assert_eq!(limits.set_bitmap_budget(0), 4096);
}

#[tokio::test]
async fn abandoning_a_partitioned_frame_does_not_commit_unsent_pixels() {
    let mut bitmap = fixture(129, 65, true);
    bitmap.x = 0;
    bitmap.y = 0;
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_remotefx(Some((EntropyBits::Rlgr3, 3, OperatingMode::empty())));
    codecs.set_max_request_size(128 * 1024);
    let mut encoder = UpdateEncoder::new(
        DesktopSize {
            width: 129,
            height: 65,
        },
        CmdFlags::SET_SURFACE_BITS,
        codecs,
    )
    .unwrap();
    encoder.set_bitmap_working_budget(16 * 1024);
    let mut updates = encoder.update(DisplayUpdate::Bitmap(bitmap.clone()));
    let first = updates.next().await.unwrap().unwrap();
    check_remotefx(
        &reassemble(first, 16 * 1024),
        &bitmap,
        OperatingMode::empty(),
        0,
        &mut vec![0; 129 * 65],
    );
    drop(updates);
    assert!(encoder.framebuffer.is_none());
    encoder.set_bitmap_working_budget(32 * 1024);
    let mut updates = encoder.update(DisplayUpdate::Bitmap(bitmap.clone()));
    let mut covered = vec![0; 129 * 65];
    let mut index = 1;
    while let Some(update) = updates.next().await {
        check_remotefx(
            &reassemble(update.unwrap(), 32 * 1024),
            &bitmap,
            OperatingMode::empty(),
            index,
            &mut covered,
        );
        index += 1;
    }
    drop(updates);
    assert!(covered.iter().all(|count| *count == 1));
    assert!(encoder.framebuffer.is_some());
    assert!(encoder
        .update(DisplayUpdate::Bitmap(bitmap))
        .next()
        .await
        .is_none());
}

#[tokio::test]
async fn working_bitmap_budget_does_not_replace_a_legal_pointer_shape() {
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_max_request_size(128 * 1024);
    let mut encoder = UpdateEncoder::new(
        DesktopSize {
            width: 640,
            height: 480,
        },
        CmdFlags::empty(),
        codecs,
    )
    .unwrap();
    encoder.set_bitmap_working_budget(16 * 1024);
    let mut updates = encoder.update(DisplayUpdate::RGBAPointer(RGBAPointer {
        width: 64,
        height: 64,
        hot_x: 0,
        hot_y: 0,
        data: vec![255; 64 * 64 * 4],
    }));
    let mut output = updates.next().await.unwrap().unwrap();
    assert!(output.data.len() > 16 * 1024);
    let mut buffer = vec![0; output.size_hint()];
    let length = output.next(&mut buffer).unwrap();
    let mut cursor = ReadCursor::new(&buffer[..length]);
    let _: FastPathHeader = decode_cursor(&mut cursor).unwrap();
    let packet: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
    assert_eq!(packet.update_code, UpdateCode::NewPointer);
}

#[test]
fn remotefx_handler_releases_excess_serialization_scratch_after_success_or_rejection() {
    let bitmap = fixture(1, 1, false);
    let mut handler = RemoteFxHandler::new(
        EntropyBits::Rlgr3,
        3,
        DesktopSize {
            width: 64,
            height: 64,
        },
        crate::BitmapQuality::HighQuality,
        OperatingMode::empty(),
    );
    for (limit, accepted) in [(128, false), (16 * 1024, true)] {
        handler.output = Vec::with_capacity(rfx::MAX_CACHED_BYTES + 1);
        let result = handler.handle_with_limit(&bitmap, limit).unwrap();
        assert_eq!(result.is_some(), accepted);
        assert!(handler.output.capacity() <= rfx::MAX_CACHED_BYTES);
        if let Some(output) = result {
            check_remotefx(&output.data, &bitmap, OperatingMode::empty(), 0, &mut [0]);
        }
    }
}

fn check_pointer_packets(mut output: UpdateFragmenter, expected_code: UpdateCode, limit: usize) {
    assert!(output.data.len() <= limit);
    let expected = output.data.clone();
    let mut actual = Vec::new();
    let mut packet = vec![0; output.size_hint()];
    let mut count = 0;
    while let Some(length) = output.next(&mut packet) {
        let mut cursor = ReadCursor::new(&packet[..length]);
        let _: FastPathHeader = decode_cursor(&mut cursor).unwrap();
        let pdu: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
        assert_eq!(pdu.update_code, expected_code);
        actual.extend_from_slice(pdu.data);
        count += 1;
    }
    assert!(
        count > 0,
        "empty hide/default updates still require a packet"
    );
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn pointer_interleaving_preserves_pending_pixels_video_indices_and_framebuffer_commit() {
    let mut bitmap = fixture(129, 65, true);
    bitmap.x = 0;
    bitmap.y = 0;
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_remotefx(Some((EntropyBits::Rlgr3, 3, OperatingMode::empty())));
    codecs.set_max_request_size(128 * 1024);
    let mut encoder = UpdateEncoder::new(
        DesktopSize {
            width: 129,
            height: 65,
        },
        CmdFlags::SET_SURFACE_BITS,
        codecs,
    )
    .unwrap();
    encoder.set_bitmap_working_budget(16 * 1024);
    let mut updates = encoder.update(DisplayUpdate::Bitmap(bitmap.clone()));
    let mut covered = vec![0; 129 * 65];
    let first = updates.next().await.unwrap().unwrap();
    check_remotefx(
        &reassemble(first, 16 * 1024),
        &bitmap,
        OperatingMode::empty(),
        0,
        &mut covered,
    );
    assert!(updates.encoder.framebuffer.is_none());
    // Other display events cannot consume or replace this bitmap transaction.
    assert!(updates
        .encode_pointer(DisplayUpdate::Bitmap(bitmap.clone()))
        .unwrap()
        .is_none());
    assert!(updates
        .encode_pointer(DisplayUpdate::Resize(DesktopSize {
            width: 1,
            height: 1
        }))
        .unwrap()
        .is_none());
    let position: PointerPositionAttribute = decode(&[11, 0, 22, 0]).unwrap();
    for (pointer, code) in [
        (
            DisplayUpdate::PointerPosition(position),
            UpdateCode::PositionPointer,
        ),
        (
            DisplayUpdate::RGBAPointer(RGBAPointer {
                width: 64,
                height: 64,
                hot_x: 0,
                hot_y: 0,
                data: vec![255; 64 * 64 * 4],
            }),
            UpdateCode::NewPointer,
        ),
        (
            DisplayUpdate::ColorPointer(ColorPointer {
                width: 2,
                height: 2,
                hot_x: 0,
                hot_y: 0,
                xor_mask: vec![255; 16],
                and_mask: vec![0; 4],
            }),
            UpdateCode::NewPointer,
        ),
        (DisplayUpdate::HidePointer, UpdateCode::HiddenPointer),
        (DisplayUpdate::DefaultPointer, UpdateCode::DefaultPointer),
    ] {
        let output = updates.encode_pointer(pointer).unwrap().unwrap();
        check_pointer_packets(output, code, 128 * 1024);
        assert!(updates.encoder.framebuffer.is_none());
    }
    let mut index = 1;
    while let Some(update) = updates.next().await {
        check_remotefx(
            &reassemble(update.unwrap(), 16 * 1024),
            &bitmap,
            OperatingMode::empty(),
            index,
            &mut covered,
        );
        index += 1;
    }
    assert!(index > 1);
    drop(updates);
    assert!(covered.iter().all(|count| *count == 1));
    assert!(encoder.framebuffer.is_some());
    assert!(encoder
        .update(DisplayUpdate::Bitmap(bitmap))
        .next()
        .await
        .is_none());
}

#[tokio::test]
async fn interleaved_pointer_fallback_uses_the_hard_limit_without_consuming_iterator_state() {
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
    let mut updates = encoder.update(DisplayUpdate::HidePointer);
    let position: PointerPositionAttribute = decode(&[11, 0, 22, 0]).unwrap();
    let fallback = updates
        .encode_pointer(DisplayUpdate::PointerPosition(position))
        .unwrap()
        .unwrap();
    check_pointer_packets(fallback, UpdateCode::DefaultPointer, 1);
    let original = updates.next().await.unwrap().unwrap();
    check_pointer_packets(original, UpdateCode::HiddenPointer, 1);
    assert!(updates.next().await.is_none());
}
