use std::sync::{Arc, Mutex};

use ironrdp_core::encode_vec;
use ironrdp_dvc::DvcProcessor;

use super::*;
use crate::gfx::{unwrap_zgfx, wrap_zgfx, GfxHandler, GfxState};

fn offer(entries: u16) -> Vec<u8> {
    let mut data = Vec::with_capacity(FIXED_SIZE + usize::from(entries) * ENTRY_SIZE);
    data.extend_from_slice(&OFFER_COMMAND);
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(
        &((FIXED_SIZE + usize::from(entries) * ENTRY_SIZE) as u32).to_le_bytes(),
    );
    data.extend_from_slice(&entries.to_le_bytes());
    for _ in 0..entries {
        data.extend_from_slice(&0x0706050403020100u64.to_le_bytes());
        data.extend_from_slice(&4096u32.to_le_bytes());
    }
    data
}

fn assert_empty_reply(message: &DvcMessage) {
    let wrapped = encode_vec(message.as_ref()).unwrap();
    let bytes = unwrap_zgfx(&wrapped).expect("reply uses normal GFX ZGFX envelope");
    // Check actual wire bytes independently of the reply's own encoder.
    assert_eq!(bytes, [0x11, 0, 0, 0, 10, 0, 0, 0, 0, 0]);
}

#[test]
fn valid_empty_and_multiple_entry_offers_receive_exact_zero_import_reply() {
    for entries in [0, 1, 3, MAX_ENTRIES] {
        assert_empty_reply(&decline_offer(&offer(entries)).unwrap().unwrap());
    }
}

#[test]
fn truncated_offer_is_rejected_at_every_boundary() {
    let data = offer(2);
    for length in 2..data.len() {
        assert!(decline_offer(&data[..length]).is_err(), "length={length}");
    }
    // An incomplete command identifier is not misclassified as an offer.
    assert!(decline_offer(&data[..1]).unwrap().is_none());
}

#[test]
fn offer_envelope_rejects_bad_lengths_trailing_data_and_excessive_counts() {
    let mut variants = Vec::new();
    for declared in [0, 7, 10, 21, 23, u32::MAX] {
        let mut data = offer(1);
        data[4..8].copy_from_slice(&declared.to_le_bytes());
        variants.push(data);
    }
    let mut trailing = offer(1);
    trailing.push(0);
    variants.push(trailing);
    let mut overdeclared = offer(1);
    overdeclared[8..10].copy_from_slice(&2u16.to_le_bytes());
    variants.push(overdeclared);
    variants.push(offer(MAX_ENTRIES + 1));
    let mut excessive_header = offer(0);
    excessive_header[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
    variants.push(excessive_header);
    for data in variants {
        assert!(decline_offer(&data).is_err());
    }
    // Reserved flags have no import semantics and must not disconnect an
    // otherwise usable client. The supported message body is still validated.
    let mut flags = offer(1);
    flags[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_empty_reply(&decline_offer(&flags).unwrap().unwrap());
}

#[test]
fn unrelated_messages_are_left_to_existing_processor() {
    for data in [&[][..], &[0x0d][..], &[0x12, 0][..], &[0x16, 0, 0, 0][..]] {
        assert!(decline_offer(data).unwrap().is_none());
    }
}

#[test]
fn handler_replies_to_repeated_raw_and_segmented_offers_without_mutating_graphics() {
    let state = Arc::new(Mutex::new(GfxState::new(640, 480, false, false)));
    let mut handler = GfxHandler::new(state.clone());
    handler.start(7).unwrap();
    {
        let mut state = state.lock().unwrap();
        state.caps_confirmed = true;
        state.surface_created = true;
        state.frame_id = 42;
        state.pending_acks = 2;
    }
    for data in [offer(0), offer(2), wrap_zgfx(&offer(3)), offer(1)] {
        let responses = handler.process(7, &data).unwrap();
        assert_eq!(responses.len(), 1);
        assert_empty_reply(&responses[0]);
        let state = state.lock().unwrap();
        assert_eq!(state.channel_id, Some(7));
        assert!(state.caps_confirmed && state.surface_created);
        assert_eq!((state.frame_id, state.pending_acks), (42, 2));
    }
    assert!(handler.process(7, &offer(1)[..11]).is_err());
}

#[test]
fn all_short_client_headers_return_errors_without_panicking() {
    let state = Arc::new(Mutex::new(GfxState::new(640, 480, false, false)));
    let mut handler = GfxHandler::new(state);
    for command in [0x0du16, 0x10, 0x12] {
        let mut header = [0; 8];
        header[..2].copy_from_slice(&command.to_le_bytes());
        for length in 0..8 {
            assert!(handler.process(7, &header[..length]).is_err());
            assert!(handler.process(7, &wrap_zgfx(&header[..length])).is_err());
        }
    }
}

#[test]
fn supported_client_envelope_checks_lengths_and_ignores_reserved_flags() {
    let state = Arc::new(Mutex::new(GfxState::new(640, 480, false, false)));
    let mut handler = GfxHandler::new(state);
    let mut ack = vec![0; 20];
    ack[..2].copy_from_slice(&0x0du16.to_le_bytes());
    ack[4..8].copy_from_slice(&20u32.to_le_bytes());
    assert!(handler.process(7, &ack).unwrap().is_empty());
    for declared in [0, 7, 8, 19, 21, u32::MAX] {
        ack[4..8].copy_from_slice(&declared.to_le_bytes());
        assert!(handler.process(7, &ack).is_err());
    }
    ack[4..8].copy_from_slice(&20u32.to_le_bytes());
    ack[2] = 1;
    assert!(handler.process(7, &ack).unwrap().is_empty());
}

#[test]
fn malformed_known_bodies_are_bounded_and_do_not_panic() {
    let state = Arc::new(Mutex::new(GfxState::new(640, 480, false, false)));
    let mut handler = GfxHandler::new(state.clone());
    for command in [0x0du16, 0x12] {
        // Both supported body decoders check their fixed fields. Capability
        // counts and lengths also remain bounded by the actual supplied data.
        for length in 8..32 {
            let mut data = vec![0xff; length];
            data[..2].copy_from_slice(&command.to_le_bytes());
            data[2..4].fill(0);
            data[4..8].copy_from_slice(&(length as u32).to_le_bytes());
            let _ = handler.process(7, &data);
        }
    }
    let state = state.lock().unwrap();
    assert!(!state.caps_confirmed && !state.surface_created);
}

#[test]
fn malformed_segment_wrappers_never_create_partial_or_reversed_slices() {
    let mut zero_segment = vec![0xe1, 1, 0, 0, 0, 0, 0];
    zero_segment.extend_from_slice(&0u32.to_le_bytes());
    zero_segment.push(4);
    let mut variants = vec![zero_segment, vec![0xe1; 7], vec![0xe0, 4]];
    let valid = wrap_zgfx(&vec![0; 70_000]);
    assert_eq!(unwrap_zgfx(&valid).unwrap().len(), 70_000);
    variants.push(valid[..valid.len() - 1].to_vec());
    let mut excessive_size = valid.clone();
    excessive_size[3..7].copy_from_slice(&u32::MAX.to_le_bytes());
    variants.push(excessive_size);
    let mut wrong_count = valid.clone();
    wrong_count[1..3].copy_from_slice(&3u16.to_le_bytes());
    variants.push(wrong_count);
    let mut trailing = valid;
    trailing.push(0);
    variants.push(trailing);
    for data in variants {
        assert!(unwrap_zgfx(&data).is_none());
    }
}
