//! Validate the command identifier on the actual Fast-Path wire, independent of
//! the encoder's SurfaceCommand constructor. MS-RDPRFX 3.1.8.3.1 requires 0x0006.

use super::*;
use ironrdp_core::{decode_cursor, ReadCursor};
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdatePdu, Fragmentation};

fn codecs(mode: OperatingMode) -> UpdateEncoderCodecs {
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_remotefx(Some((EntropyBits::Rlgr3, 17, mode)));
    codecs
}

fn reassemble_update(mut output: UpdateFragmenter, expected_code: UpdateCode) -> Vec<u8> {
    let mut data = Vec::new();
    let mut packet = vec![0; output.size_hint()];
    let mut first = true;
    let mut complete = false;
    while let Some(len) = output.next(&mut packet) {
        assert!(!complete);
        let mut cursor = ReadCursor::new(&packet[..len]);
        let header: FastPathHeader = decode_cursor(&mut cursor).unwrap();
        let header_len = cursor.pos();
        let update: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        assert_eq!(header_len + header.data_length, len);
        assert_eq!(update.update_code, expected_code);
        match update.fragmentation {
            Fragmentation::Single => {
                assert!(first);
                complete = true;
            }
            Fragmentation::First => assert!(first),
            Fragmentation::Next => assert!(!first),
            Fragmentation::Last => {
                assert!(!first);
                complete = true;
            }
        }
        first = false;
        data.extend_from_slice(update.data);
    }
    assert!(complete);
    data
}

#[tokio::test]
async fn stream_only_surface_capability_selects_remotefx_in_both_modes() {
    for mode in [OperatingMode::empty(), OperatingMode::IMAGE_MODE] {
        let mut encoder = UpdateEncoder::new(
            DesktopSize {
                width: 64,
                height: 64,
            },
            CmdFlags::STREAM_SURFACE_BITS,
            codecs(mode),
        )
        .unwrap();
        assert_eq!(encoder.bitmap_codec(), "remotefx");
        let mut bitmap = quality_tests::fixture(64, 64, true);
        bitmap.x = 0;
        bitmap.y = 0;
        let output = encoder
            .update(DisplayUpdate::Bitmap(bitmap))
            .next()
            .await
            .unwrap()
            .unwrap();
        let bytes = reassemble_update(output, UpdateCode::SurfaceCommands);
        assert_eq!(&bytes[..2], &[0x06, 0x00]);
    }
}

#[tokio::test]
async fn remotefx_uses_stream_command_even_when_set_surface_bits_is_also_supported() {
    for mode in [OperatingMode::empty(), OperatingMode::IMAGE_MODE] {
        let mut encoder = UpdateEncoder::new(
            DesktopSize {
                width: 64,
                height: 64,
            },
            CmdFlags::SET_SURFACE_BITS | CmdFlags::STREAM_SURFACE_BITS,
            codecs(mode),
        )
        .unwrap();
        let output = encoder
            .update(DisplayUpdate::Bitmap(quality_tests::fixture(17, 19, true)))
            .next()
            .await
            .unwrap()
            .unwrap();
        let bytes = reassemble_update(output, UpdateCode::SurfaceCommands);
        assert_eq!(
            &bytes[..2],
            &[0x06, 0x00],
            "RemoteFX must use STREAM, not SET"
        );
    }
}

#[tokio::test]
async fn remotefx_without_stream_support_falls_back_to_compressed_bitmap_for_every_quality() {
    use ironrdp_graphics::rdp6::BitmapStreamDecoder;
    use ironrdp_pdu::bitmap::BitmapUpdateData;
    for mode in [OperatingMode::empty(), OperatingMode::IMAGE_MODE] {
        for flags in [
            CmdFlags::empty(),
            CmdFlags::SET_SURFACE_BITS,
            CmdFlags::FRAME_MARKER,
            CmdFlags::SET_SURFACE_BITS | CmdFlags::FRAME_MARKER,
        ] {
            let mut encoder = UpdateEncoder::new(
                DesktopSize {
                    width: 64,
                    height: 64,
                },
                flags,
                codecs(mode),
            )
            .unwrap();
            for quality in [
                crate::BitmapQuality::HighQuality,
                crate::BitmapQuality::Lossless,
                crate::BitmapQuality::Balanced,
            ] {
                encoder.set_quality(quality).unwrap();
                assert_eq!(encoder.bitmap_codec(), "rdp6-rle");
                assert_eq!(encoder.surface_command_id(), 0);
                assert_eq!(encoder.remotefx_image_mode(), None);
                assert!(!encoder.surface_frame_markers());
                assert!(encoder.begin_bitmap_frame().unwrap().is_none());
                let bitmap = quality_tests::fixture(17, 19, true);
                let mut iter = encoder.update(DisplayUpdate::Bitmap(bitmap.clone()));
                let mut encoded_updates = 0;
                while let Some(output) = iter.next().await {
                    encoded_updates += 1;
                    let bytes = reassemble_update(output.unwrap(), UpdateCode::Bitmap);
                    let update: BitmapUpdateData<'_> = ironrdp_core::decode(&bytes).unwrap();
                    for rect in update.rectangles {
                        let header: ironrdp_pdu::bitmap::rdp6::BitmapStreamHeader =
                            ironrdp_core::decode(rect.bitmap_data).unwrap();
                        assert!(header.enable_rle_compression);
                        let mut rgb = Vec::new();
                        BitmapStreamDecoder::default()
                            .decode_bitmap_stream_to_rgb24(
                                rect.bitmap_data,
                                &mut rgb,
                                rect.width.into(),
                                rect.height.into(),
                            )
                            .unwrap();
                        for row in 0..usize::from(rect.height) {
                            for col in 0..usize::from(rect.width) {
                                let src = (usize::from(rect.rectangle.bottom - bitmap.y) - row)
                                    * bitmap.stride.get()
                                    + col * 4;
                                let dst = (row * usize::from(rect.width) + col) * 3;
                                assert_eq!(
                                    &rgb[dst..dst + 3],
                                    &[bitmap.data[src + 2], bitmap.data[src + 1], bitmap.data[src]]
                                );
                            }
                        }
                    }
                }
                drop(iter);
                assert!(encoded_updates > 0, "fallback must produce a bitmap update");
                assert!(encoder.end_bitmap_frame().unwrap().is_none());
            }
        }
    }
}

#[tokio::test]
async fn raw_set_surface_bits_keeps_its_own_capability_and_wire_command() {
    for flags in [
        CmdFlags::SET_SURFACE_BITS,
        CmdFlags::STREAM_SURFACE_BITS,
        CmdFlags::empty(),
    ] {
        let mut encoder = UpdateEncoder::new(
            DesktopSize {
                width: 64,
                height: 64,
            },
            flags,
            UpdateEncoderCodecs::new(),
        )
        .unwrap();
        let has_set = flags.contains(CmdFlags::SET_SURFACE_BITS);
        assert_eq!(encoder.surface_command_id(), if has_set { 1 } else { 0 });
        assert_eq!(encoder.remotefx_image_mode(), None);
        let output = encoder
            .update(DisplayUpdate::Bitmap(quality_tests::fixture(17, 19, true)))
            .next()
            .await
            .unwrap()
            .unwrap();
        let bytes = reassemble_update(
            output,
            if has_set {
                UpdateCode::SurfaceCommands
            } else {
                UpdateCode::Bitmap
            },
        );
        if has_set {
            assert_eq!(&bytes[..2], &[0x01, 0x00]);
            let SurfaceCommand::SetSurfaceBits(surface) = ironrdp_core::decode(&bytes).unwrap()
            else {
                panic!("raw SetSurfaceBits")
            };
            assert_eq!(surface.extended_bitmap_data.codec_id, 0);
        } else {
            let _: ironrdp_pdu::bitmap::BitmapUpdateData<'_> =
                ironrdp_core::decode(&bytes).unwrap();
        }
    }
}

#[test]
fn stream_only_reconfiguration_retains_transport_support_and_marker_policy() {
    for mode in [OperatingMode::empty(), OperatingMode::IMAGE_MODE] {
        let mut encoder = UpdateEncoder::new(
            DesktopSize {
                width: 1920,
                height: 1200,
            },
            CmdFlags::STREAM_SURFACE_BITS | CmdFlags::FRAME_MARKER,
            codecs(mode),
        )
        .unwrap();
        assert_eq!(encoder.surface_command_id(), 6);
        assert_eq!(
            encoder.remotefx_image_mode(),
            Some(mode.contains(OperatingMode::IMAGE_MODE))
        );
        assert!(encoder.surface_frame_markers());
        encoder.set_quality(crate::BitmapQuality::Lossless).unwrap();
        assert_eq!(encoder.surface_command_id(), 0);
        assert!(!encoder.surface_frame_markers());
        encoder
            .set_quality(crate::BitmapQuality::HighQuality)
            .unwrap();
        assert_eq!(encoder.surface_command_id(), 6);
        assert!(encoder.surface_frame_markers());
    }
}
