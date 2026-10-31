//! Independent client-wire fixtures for Windows graphics acknowledgement flow.
//!
//! Headers and bodies are written by hand rather than with the implementation's
//! encoder. These cases establish compatibility and bounds; the exact packet
//! rejected in the user's 0.1.38 session was not retained in the logs.

use std::sync::{Arc, Mutex};

use ironrdp_core::encode_vec;
use ironrdp_dvc::DvcProcessor;

use super::{GfxHandler, GfxState};

fn header(command: u16, flags: u16, length: u32) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&command.to_le_bytes());
    data.extend_from_slice(&flags.to_le_bytes());
    data.extend_from_slice(&length.to_le_bytes());
    data
}

fn ack(frame_id: u32) -> Vec<u8> {
    let mut data = header(0x000d, 0, 20);
    data.extend_from_slice(&0u32.to_le_bytes()); // Queue depth unavailable.
    data.extend_from_slice(&frame_id.to_le_bytes());
    data.extend_from_slice(&frame_id.to_le_bytes()); // Total frames decoded.
    data
}

fn qoe(frame_id: u32) -> Vec<u8> {
    let mut data = header(0x0016, 0, 20);
    data.extend_from_slice(&frame_id.to_le_bytes());
    data.extend_from_slice(&1234u32.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&2u16.to_le_bytes());
    data
}

fn handler(frame_count: u32) -> (GfxHandler, Arc<Mutex<GfxState>>) {
    let shared = Arc::new(Mutex::new(GfxState::new(640, 480, false, false)));
    let mut handler = GfxHandler::new(shared.clone());
    handler.start(7).unwrap();
    {
        let mut state = shared.lock().unwrap();
        state.caps_confirmed = true;
        state.avc420_supported = true;
        state.surface_created = true;
        for _ in 0..frame_count {
            state.next_frame_id();
        }
    }
    (handler, shared)
}

#[test]
fn after_ten_video_frames_batched_ack_qoe_ack_preserves_ack_progress() {
    let (mut handler, shared) = handler(12);
    for frame in 1..10 {
        assert!(handler.process(7, &ack(frame)).unwrap().is_empty());
    }
    let mut batch = ack(10);
    batch.extend_from_slice(&qoe(10));
    batch.extend_from_slice(&ack(11));
    assert!(handler.process(7, &batch).unwrap().is_empty());
    {
        let state = shared.lock().unwrap();
        assert_eq!(state.last_ack_frame, 11);
        assert_eq!(state.client_total_frames_decoded, 11);
        assert_eq!(state.pending_acks, 1);
        assert!(state.surface_created && state.caps_confirmed);
    }
    assert!(handler.process(7, &ack(12)).unwrap().is_empty());
    assert_eq!(shared.lock().unwrap().pending_acks, 0);
}

#[test]
fn reserved_ack_flags_do_not_disconnect_or_prevent_bounded_body_decode() {
    let (mut handler, shared) = handler(1);
    let mut data = ack(1);
    data[2..4].copy_from_slice(&0x8001u16.to_le_bytes());
    assert!(handler.process(7, &data).unwrap().is_empty());
    let state = shared.lock().unwrap();
    assert_eq!(state.last_ack_frame, 1);
    assert_eq!(state.pending_acks, 0);
}

#[test]
fn malformed_optional_qoe_tail_does_not_break_the_next_ack_message() {
    for declared in [0, 7, 19, 21, u32::MAX] {
        let (mut handler, shared) = handler(2);
        let mut optional = qoe(1);
        optional[2..4].copy_from_slice(&0xffffu16.to_le_bytes());
        optional[4..8].copy_from_slice(&declared.to_le_bytes());
        assert!(handler.process(7, &optional).unwrap().is_empty());
        assert_eq!(shared.lock().unwrap().pending_acks, 2);
        assert!(handler.process(7, &ack(1)).unwrap().is_empty());
        assert_eq!(shared.lock().unwrap().pending_acks, 1);
    }
}

#[test]
fn unsupported_optional_body_is_skipped_before_decoding_batched_known_ack() {
    let (mut handler, shared) = handler(1);
    // QoE is informational and this server does not consume its body. Even an
    // empty optional body must not be passed into the pinned client decoder.
    let mut batch = header(0x0016, 0xffff, 8);
    batch.extend_from_slice(&ack(1));
    assert!(handler.process(7, &batch).unwrap().is_empty());
    assert_eq!(shared.lock().unwrap().last_ack_frame, 1);
    assert_eq!(shared.lock().unwrap().pending_acks, 0);
}

#[test]
fn cache_offer_and_ack_in_one_channel_message_both_complete() {
    let (mut handler, shared) = handler(1);
    let mut batch = header(0x0010, 0, 22);
    batch.extend_from_slice(&1u16.to_le_bytes());
    batch.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    batch.extend_from_slice(&4096u32.to_le_bytes());
    batch.extend_from_slice(&ack(1));
    let replies = handler.process(7, &batch).unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(
        encode_vec(replies[0].as_ref()).unwrap(),
        [0xe0, 4, 0x11, 0, 0, 0, 10, 0, 0, 0, 0, 0],
        "one ZGFX-wrapped zero-import reply"
    );
    let state = shared.lock().unwrap();
    assert_eq!(state.channel_id, Some(7));
    assert_eq!(state.last_ack_frame, 1);
    assert_eq!(state.pending_acks, 0);
    assert!(state.surface_created && state.caps_confirmed);
}

#[test]
fn malformed_known_tail_cannot_apply_an_earlier_ack_from_the_same_batch() {
    let (mut handler, shared) = handler(2);
    let mut batch = ack(1);
    batch.extend_from_slice(&ack(2)[..19]); // Declared fixed ACK body is incomplete.
    assert!(handler.process(7, &batch).is_err());
    {
        let state = shared.lock().unwrap();
        assert_eq!(state.last_ack_frame, 0);
        assert_eq!(state.pending_acks, 2);
        assert_eq!(state.client_total_frames_decoded, 0);
    }
    assert!(handler.process(7, &ack(1)).unwrap().is_empty());
    assert_eq!(shared.lock().unwrap().pending_acks, 1);
}
