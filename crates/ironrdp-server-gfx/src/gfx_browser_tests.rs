//! Verify the negotiated wire contract consumed by the browser passthrough.
use super::*;
use bytes::Bytes;
use ironrdp_core::{Decode, ReadCursor};

fn access_unit(width: u16, height: u16, keyframe: bool) -> GfxFrameUpdate {
    GfxFrameUpdate {
        h264_data: Bytes::from_static(if keyframe {
            &[
                0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0xb8,
            ]
        } else {
            &[0, 0, 0, 1, 0x41, 0xc0]
        }),
        width,
        height,
        enc_width: width.div_ceil(16) * 16,
        enc_height: height.div_ceil(16) * 16,
        is_keyframe: keyframe,
        h264_aux: None,
    }
}

fn unpack(bytes: &[u8]) -> Vec<ServerPdu> {
    let raw = unwrap_zgfx(bytes).expect("GFX data is segmented");
    let mut cursor = ReadCursor::new(&raw);
    let mut pdus = Vec::new();
    while !cursor.is_empty() {
        pdus.push(ServerPdu::decode(&mut cursor).unwrap());
    }
    pdus
}

fn assert_avc420_payload(pdus: &[ServerPdu], expected: &GfxFrameUpdate, frame_id: u32) {
    let start = pdus
        .iter()
        .position(|pdu| {
            matches!(pdu,
        ServerPdu::StartFrame(start) if start.frame_id == frame_id)
        })
        .unwrap();
    let ServerPdu::WireToSurface1(surface) = &pdus[start + 1] else {
        panic!("AVC payload must be enclosed by its frame boundaries")
    };
    assert_eq!(surface.codec_id, Codec1Type::Avc420);
    assert_eq!(surface.surface_id, 0);
    assert_eq!(
        surface.destination_rectangle,
        InclusiveRectangle {
            left: 0,
            top: 0,
            right: expected.width,
            bottom: expected.height,
        }
    );
    let stream = Avc420BitmapStream::decode(&mut ReadCursor::new(&surface.bitmap_data)).unwrap();
    assert_eq!(
        stream.rectangles,
        vec![surface.destination_rectangle.clone()]
    );
    assert_eq!(stream.quant_qual_vals.len(), 1);
    assert_eq!(stream.data, &expected.h264_data[..]);
    assert!(matches!(&pdus[start + 2], ServerPdu::EndFrame(end) if end.frame_id == frame_id));
}

#[test]
fn only_exact_opt_in_name_selects_continuous_avc420_and_reconnect_clears_it() {
    let mut state = GfxState::new(320, 180, true, false);
    state.avc444_supported = true;
    for name in [
        "Guacamole RDP",
        "Windows App",
        "GuacamoleAVC420-suffix",
        "guacamoleavc420",
    ] {
        state.set_client_graphics_preferences(name);
        assert!(!state.browser_avc420);
        assert!(state.use_avc444());
    }
    assert!(BROWSER_AVC420_CLIENT_NAME.is_ascii());
    assert!(BROWSER_AVC420_CLIENT_NAME.len() <= 15);
    state.set_client_graphics_preferences(BROWSER_AVC420_CLIENT_NAME);
    assert!(state.browser_avc420);
    assert!(!state.use_avc444());
    // The hint never advertises a codec the client has not negotiated.
    assert!(!state.avc420_supported);
    assert!(!state.is_ready());
    state.reset_connection_state(true);
    assert!(!state.browser_avc420);
}

#[test]
fn browser_receives_cropped_full_frame_avc420_even_when_host_and_client_support_444() {
    let mut state = GfxState::new(318, 178, true, false);
    state.set_client_graphics_preferences(BROWSER_AVC420_CLIENT_NAME);
    state.avc420_supported = true;
    state.avc444_supported = true;
    let mut frame = access_unit(318, 178, true);
    frame.h264_aux = Some(Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x7f]));
    let pdus = unpack(&GfxHandler::create_frame_pdu(&mut state, &frame));
    assert!(matches!(&pdus[0], ServerPdu::ResetGraphics(reset)
        if reset.width == 318 && reset.height == 178));
    assert!(matches!(&pdus[1], ServerPdu::CreateSurface(surface)
        if surface.width == 318 && surface.height == 178));
    assert_avc420_payload(&pdus, &frame, 1);
    assert_eq!(state.pending_acks, 1);
}

#[test]
fn browser_avc444_opt_in_preserves_configuration_and_negotiated_codec_limits() {
    let mut state = GfxState::new(318, 178, true, false);
    for name in ["GuacamoleAVC444-suffix", "guacamoleavc444", "Guacamole RDP"] {
        state.set_client_graphics_preferences(name);
        assert!(!state.browser_video());
    }
    assert!(BROWSER_AVC444_CLIENT_NAME.is_ascii());
    assert!(BROWSER_AVC444_CLIENT_NAME.len() <= 15);
    state.set_client_graphics_preferences(BROWSER_AVC444_CLIENT_NAME);
    assert!(state.browser_avc444 && state.browser_video());
    assert!(!state.browser_avc420);
    assert!(!state.use_avc444());
    state.avc420_supported = true;
    state.avc444_supported = true;
    assert!(state.use_avc444());
    let mut frame = access_unit(318, 178, true);
    frame.h264_aux = Some(Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x7f]));
    let pdus = unpack(&GfxHandler::create_frame_pdu(&mut state, &frame));
    let surface = pdus
        .iter()
        .find_map(|pdu| match pdu {
            ServerPdu::WireToSurface1(surface) => Some(surface),
            _ => None,
        })
        .unwrap();
    assert_eq!(surface.codec_id, Codec1Type::Avc444);
    let pair = Avc444BitmapStream::decode(&mut ReadCursor::new(&surface.bitmap_data)).unwrap();
    assert_eq!(pair.encoding, Encoding::LUMA_AND_CHROMA);
    assert_eq!(pair.stream1.data, &frame.h264_data[..]);
    assert_eq!(
        pair.stream2.unwrap().data,
        frame.h264_aux.as_ref().unwrap().as_ref()
    );
    state.avc444_enabled = false;
    let pdus = unpack(&GfxHandler::create_frame_pdu(
        &mut state,
        &access_unit(318, 178, true),
    ));
    assert_avc420_payload(&pdus, &access_unit(318, 178, true), 2);
    assert!(state.browser_video());
    state.reset_connection_state(true);
    assert!(!state.browser_video());
}

#[test]
fn raw_startup_then_avc_delta_and_resize_preserve_frame_and_surface_order() {
    let mut state = GfxState::new(320, 180, false, false);
    state.set_client_graphics_preferences(BROWSER_AVC420_CLIENT_NAME);
    state.avc420_supported = true;
    state.channel_id = Some(7);
    state.caps_confirmed = true;
    let raw = GfxUncompressedUpdate {
        width: 320,
        height: 180,
        rects: vec![crate::display::UncompressedRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            pixel_data: Bytes::from_static(&[0x10; 16]),
        }],
    };
    let raw_pdus = unpack(&GfxHandler::create_uncompressed_pdu(&mut state, &raw));
    assert!(raw_pdus.iter().any(|pdu| matches!(pdu,
        ServerPdu::WireToSurface1(surface) if surface.codec_id == Codec1Type::Uncompressed)));
    state.acknowledge_frame(1, QueueDepth::Unavailable, 1);
    for (id, keyframe) in [(2, true), (3, false)] {
        let frame = access_unit(320, 180, keyframe);
        let pdus = unpack(&GfxHandler::create_frame_pdu(&mut state, &frame));
        assert!(matches!(&pdus[0], ServerPdu::StartFrame(_)));
        assert_avc420_payload(&pdus, &frame, id);
        state.acknowledge_frame(id, QueueDepth::Unavailable, id);
        assert_eq!(state.pending_acks, 0);
    }
    let frame = access_unit(352, 208, true);
    let pdus = unpack(&GfxHandler::create_frame_pdu(&mut state, &frame));
    assert!(matches!(&pdus[0], ServerPdu::DeleteSurface(_)));
    assert!(matches!(&pdus[1], ServerPdu::ResetGraphics(_)));
    assert!(matches!(&pdus[2], ServerPdu::CreateSurface(_)));
    assert!(matches!(&pdus[3], ServerPdu::MapSurfaceToOutput(_)));
    assert_avc420_payload(&pdus, &frame, 4);
    assert!(state.browser_avc420);
    assert_eq!(state.channel_id, Some(7));
}

#[test]
fn chroma_changes_wait_for_new_connection_and_survive_channel_reactivation() {
    let mut state = GfxState::new(318, 178, false, false);
    let negotiate = |state: &mut GfxState| {
        state.set_client_graphics_preferences(BROWSER_AVC444_CLIENT_NAME);
        state.avc420_supported = true;
        state.avc444_supported = true;
    };
    state.begin_connection(None);
    negotiate(&mut state);
    state.chroma_mode = Some("avc444".into());
    assert!(
        !state.use_avc444(),
        "Pending chroma must not alter an active stream"
    );
    state.reset_connection_state(true);
    negotiate(&mut state);
    assert!(
        !state.use_avc444(),
        "A channel/resize activation is not a new connection"
    );

    state.begin_connection(None);
    assert!(state.avc444_enabled);
    assert!(
        !state.avc444_supported,
        "Reconnect must renegotiate all capabilities"
    );
    negotiate(&mut state);
    assert!(state.use_avc444());
    let mut frame = access_unit(318, 178, true);
    frame.h264_aux = Some(Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x7f]));
    let pdus = unpack(&GfxHandler::create_frame_pdu(&mut state, &frame));
    assert!(pdus
        .iter()
        .any(|pdu| matches!(pdu, ServerPdu::WireToSurface1(surface)
        if surface.codec_id == Codec1Type::Avc444)));

    state.chroma_mode = Some("avc420".into());
    assert!(
        state.use_avc444(),
        "The reverse transition also waits for reconnect"
    );
    state.reset_connection_state(true);
    negotiate(&mut state);
    assert!(state.use_avc444());
    state.begin_connection(None);
    negotiate(&mut state);
    assert!(!state.use_avc444());
    let pdus = unpack(&GfxHandler::create_frame_pdu(&mut state, &frame));
    assert_avc420_payload(&pdus, &frame, 1);
    state.begin_connection(None);
    assert!(
        !state.avc444_enabled,
        "The applied choice persists across later reconnects"
    );
}

#[test]
fn reconnect_preserves_durable_controls_but_discards_session_failures_and_metrics() {
    let mut state = GfxState::new(320, 180, false, true);
    state.target_frame_rate = Some(90);
    state.target_bitrate = Some(44_000_000);
    state.show_cursor = Some(false);
    state.resolution = Some("1920x1080".into());
    state.encoder_pref = Some("bitmap".into());
    state.target_quality = Some("lossless".into());
    state.manual_bitmap_mode = true;
    state.chroma_mode = Some("avc444".into());
    state.bitmap_fallback_mode = true;
    state.avc_fallback_to_uncompressed = true;
    state.clipboard_disabled = true;
    state.active_encoder = Some("failed-encoder");
    state.total_frames_sent = 42;
    state.pending_acks = 7;
    state.frame_id = 50;
    state.surface_created = true;
    state.avc420_supported = true;
    state.avc444_supported = true;
    state.browser_avc420 = true;
    let controls = state.controls_receiver();
    let peer = Some(std::net::IpAddr::from([192, 168, 86, 24]));
    for _ in 0..2 {
        assert_eq!(state.begin_connection(peer).as_deref(), Some("1920x1080"));
        assert_eq!(state.peer_addr, peer);
        assert_eq!(state.target_frame_rate, Some(90));
        assert_eq!(state.target_bitrate, Some(44_000_000));
        assert_eq!(state.show_cursor, Some(false));
        assert_eq!(state.encoder_pref.as_deref(), Some("bitmap"));
        assert_eq!(state.target_quality.as_deref(), Some("lossless"));
        assert_eq!(state.chroma_mode.as_deref(), Some("avc444"));
        assert!(state.avc444_enabled && state.gfx_validation_mode && state.manual_bitmap_mode);
        assert!(state.bitmap_mode());
        assert!(!state.avc420_supported && !state.avc444_supported && !state.browser_video());
        assert!(
            !state.bitmap_fallback_mode
                && !state.avc_fallback_to_uncompressed
                && !state.clipboard_disabled
        );
        assert!(!state.surface_created);
        assert_eq!(
            (state.total_frames_sent, state.frame_id, state.pending_acks),
            (0, 0, 0)
        );
        assert_eq!(state.active_encoder, None);
    }
    state.notify_controls_changed();
    assert!(
        controls.has_changed().unwrap(),
        "Reconnect must retain the open controls watch lane"
    );
}
