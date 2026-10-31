//! Exercise the actual large raw baseline through every outgoing framing layer.
//! Synthetic pixels only: no native capture, socket, desktop or client required.

use crate::gfx::{GfxHandler, GfxState, RawGfxPdu};
use crate::{GfxFrameUpdate, GfxUncompressedUpdate, UncompressedRect};
use ironrdp_core::{decode, Decode, ReadCursor};
use ironrdp_dvc::pdu::{DrdynvcDataPdu, DrdynvcServerPdu};
use ironrdp_pdu::mcs;
use ironrdp_pdu::rdp::vc::dvc::gfx::{Codec1Type, PixelFormat, ServerPdu};
use ironrdp_pdu::rdp::vc::{ChannelControlFlags, ChannelPduHeader};
use ironrdp_pdu::x224::X224;
use ironrdp_svc::{server_encode_svc_messages, ChannelFlags};

const SVC_CHANNEL: u16 = 1005;
const DVC_CHANNEL: u32 = 1;
const USER_CHANNEL: u16 = 1001;

fn transport_bytes(wrapped: Vec<u8>) -> Vec<u8> {
    let messages = ironrdp_dvc::encode_dvc_messages(
        DVC_CHANNEL,
        vec![Box::new(RawGfxPdu(wrapped))],
        ChannelFlags::SHOW_PROTOCOL,
    )
    .unwrap();
    server_encode_svc_messages(messages, SVC_CHANNEL, USER_CHANNEL).unwrap()
}

/// Independently reconstruct the logical message from emitted TPKT/SVC/DVC
/// packets. Every DVC fragment fits one SVC packet under current chunk limits.
fn reassemble_transport(wire: &[u8]) -> (Vec<u8>, usize) {
    let mut cursor = 0;
    let mut fragments = 0;
    let mut expected = None;
    let mut complete = Vec::new();
    while cursor < wire.len() {
        assert_eq!(&wire[cursor..cursor + 2], &[3, 0]);
        let length = usize::from(u16::from_be_bytes([wire[cursor + 2], wire[cursor + 3]]));
        assert!(
            (8..1700).contains(&length),
            "unbounded transport packet: {length}"
        );
        let packet: X224<mcs::McsMessage<'_>> = decode(&wire[cursor..cursor + length]).unwrap();
        let mcs::McsMessage::SendDataIndication(data) = packet.0 else {
            panic!("expected server MCS channel data")
        };
        assert_eq!(data.channel_id, SVC_CHANNEL);
        assert_eq!(data.initiator_id, USER_CHANNEL);
        let header: ChannelPduHeader = decode(&data.user_data[..8]).unwrap();
        assert!(header.flags.contains(ChannelControlFlags::FLAG_FIRST));
        assert!(header.flags.contains(ChannelControlFlags::FLAG_LAST));
        assert!(!header
            .flags
            .contains(ChannelControlFlags::PACKET_COMPRESSED));
        assert_eq!(header.length as usize, data.user_data.len() - 8);
        let DrdynvcServerPdu::Data(fragment) = decode(&data.user_data[8..]).unwrap() else {
            panic!("expected dynamic channel data")
        };
        assert_eq!(fragment.channel_id(), DVC_CHANNEL);
        let payload = match &fragment {
            DrdynvcDataPdu::DataFirst(first) => {
                assert!(
                    expected.is_none(),
                    "one logical message must have one first fragment"
                );
                assert_eq!(fragments, 0);
                expected = Some(first.length() as usize);
                first.data()
            }
            DrdynvcDataPdu::Data(data) => {
                assert!(expected.is_some());
                data.data()
            }
        };
        assert!(!payload.is_empty());
        assert!(payload.len() <= DrdynvcDataPdu::MAX_DATA_SIZE);
        complete.extend_from_slice(payload);
        assert!(complete.len() <= expected.unwrap());
        fragments += 1;
        cursor += length;
    }
    assert_eq!(cursor, wire.len());
    assert_eq!(complete.len(), expected.unwrap());
    (complete, fragments)
}

/// Do not reuse the production ZGFX unwrapping helper for this wire assertion.
fn reassemble_zgfx(wrapped: &[u8]) -> (Vec<u8>, usize) {
    assert_eq!(
        wrapped[0], 0xe1,
        "the full baseline must use multipart ZGFX"
    );
    let segments = usize::from(u16::from_le_bytes([wrapped[1], wrapped[2]]));
    let expected = u32::from_le_bytes(wrapped[3..7].try_into().unwrap()) as usize;
    let mut complete = Vec::new();
    let mut cursor = 7;
    for _ in 0..segments {
        let length = u32::from_le_bytes(wrapped[cursor..cursor + 4].try_into().unwrap()) as usize;
        assert!((2..=65535).contains(&length));
        cursor += 4;
        assert_eq!(wrapped[cursor], 0x04, "uncompressed ZGFX segment header");
        complete.extend_from_slice(&wrapped[cursor + 1..cursor + length]);
        cursor += length;
    }
    assert_eq!(cursor, wrapped.len());
    assert_eq!(complete.len(), expected);
    (complete, segments)
}

#[test]
fn a_1920_by_1200_raw_baseline_round_trips_all_framing_before_avc() {
    const WIDTH: u16 = 1920;
    const HEIGHT: u16 = 1200;
    let pixel_count = usize::from(WIDTH) * usize::from(HEIGHT);
    let mut state = GfxState::new(WIDTH, HEIGHT, false, false);
    let update = GfxUncompressedUpdate {
        width: WIDTH,
        height: HEIGHT,
        rects: vec![UncompressedRect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
            pixel_data: [11, 77, 201, 99].repeat(pixel_count).into(),
        }],
    };
    let wrapped = GfxHandler::create_uncompressed_pdu(&mut state, &update);
    let wrapped_length = wrapped.len();
    let wire = transport_bytes(wrapped);
    let (received, fragments) = reassemble_transport(&wire);
    assert_eq!(received.len(), wrapped_length);
    let (raw, segments) = reassemble_zgfx(&received);
    assert_eq!(segments, 141);
    assert_eq!(fragments, wrapped_length.div_ceil(1590));
    assert!(wire.len() > 9_216_000 && wire.len() < 10 * 1024 * 1024);

    let mut cursor = ReadCursor::new(&raw);
    let ServerPdu::ResetGraphics(reset) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("baseline must reset output before creating its surface")
    };
    assert_eq!(
        (reset.width, reset.height),
        (u32::from(WIDTH), u32::from(HEIGHT))
    );
    assert_eq!(reset.monitors.len(), 1);
    assert_eq!(
        (reset.monitors[0].right, reset.monitors[0].bottom),
        (1919, 1199)
    );
    let ServerPdu::CreateSurface(surface) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("missing baseline surface")
    };
    assert_eq!(
        (surface.surface_id, surface.width, surface.height),
        (0, WIDTH, HEIGHT)
    );
    assert_eq!(surface.pixel_format, PixelFormat::XRgb);
    let ServerPdu::MapSurfaceToOutput(output) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("missing output mapping")
    };
    assert_eq!(
        (
            output.surface_id,
            output.output_origin_x,
            output.output_origin_y
        ),
        (0, 0, 0)
    );
    let ServerPdu::StartFrame(start) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("raw baseline must be inside frame boundaries")
    };
    let ServerPdu::WireToSurface1(image) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("missing raw baseline pixels")
    };
    assert_eq!(image.surface_id, 0);
    assert_eq!(image.codec_id, Codec1Type::Uncompressed);
    assert_eq!(image.pixel_format, PixelFormat::XRgb);
    assert_eq!(
        (
            image.destination_rectangle.left,
            image.destination_rectangle.top
        ),
        (0, 0)
    );
    // RDPEGFX_RECT16's right/bottom coordinates are exclusive for this command.
    assert_eq!(
        (
            image.destination_rectangle.right,
            image.destination_rectangle.bottom
        ),
        (WIDTH, HEIGHT)
    );
    assert_eq!(image.bitmap_data.len(), pixel_count * 4);
    assert!(image
        .bitmap_data
        .chunks_exact(4)
        .all(|pixel| pixel == [11, 77, 201, 0]));
    let ServerPdu::EndFrame(end) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("missing completed baseline boundary")
    };
    assert_eq!(start.frame_id, end.frame_id);
    assert!(cursor.is_empty());

    // The following AVC access unit uses the same surface after the complete
    // raw frame; it must not reset/delete the just-established baseline.
    let avc = GfxFrameUpdate {
        width: WIDTH,
        height: HEIGHT,
        enc_width: WIDTH,
        enc_height: HEIGHT,
        h264_data: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65]),
        h264_aux: None,
        is_keyframe: true,
    };
    let encoded = GfxHandler::create_frame_pdu(&mut state, &avc);
    assert_eq!(&encoded[..2], &[0xe0, 0x04]);
    let mut cursor = ReadCursor::new(&encoded[2..]);
    let ServerPdu::StartFrame(next_start) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("AVC must follow the baseline on its existing surface")
    };
    assert_eq!(next_start.frame_id, start.frame_id + 1);
    let ServerPdu::WireToSurface1(video) = ServerPdu::decode(&mut cursor).unwrap() else {
        panic!("missing AVC surface update")
    };
    assert_eq!(video.surface_id, image.surface_id);
    assert_eq!(video.codec_id, Codec1Type::Avc420);
    assert!(matches!(
        ServerPdu::decode(&mut cursor).unwrap(),
        ServerPdu::EndFrame(_)
    ));
    assert!(cursor.is_empty());

    eprintln!(
        "synthetic 1920x1200 baseline: raw={} ZGFX={} wire={} segments={} DVC_fragments={}",
        raw.len(),
        wrapped_length,
        wire.len(),
        segments,
        fragments
    );
}
