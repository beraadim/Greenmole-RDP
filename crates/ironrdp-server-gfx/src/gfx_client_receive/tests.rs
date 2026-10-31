use super::*;

fn pdu(command: u16, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&command.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&((8 + body.len()) as u32).to_le_bytes());
    data.extend_from_slice(body);
    data
}

fn ack(frame: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(&frame.to_le_bytes());
    body.extend_from_slice(&frame.to_le_bytes());
    pdu(0x0d, &body)
}

#[test]
fn ordered_expected_messages_survive_optional_qoe_in_one_transport_buffer() {
    let mut input = ack(10);
    input.extend(pdu(0x16, &[0; 12]));
    input.extend(pdu(0x10, &0u16.to_le_bytes()));
    input.extend(ack(11));
    let mut observations = Observations::default();
    let messages = prepare(&input, &mut observations).unwrap();
    assert_eq!(messages.len(), 3);
    assert!(
        matches!(&messages[0], Prepared::Client(ClientPdu::FrameAcknowledge(pdu)) if pdu.frame_id == 10)
    );
    assert!(matches!(&messages[1], Prepared::CacheOffer(0)));
    assert!(
        matches!(&messages[2], Prepared::Client(ClientPdu::FrameAcknowledge(pdu)) if pdu.frame_id == 11)
    );
    assert_eq!(observations.ignored, 1);
}

#[test]
fn unknown_optional_lengths_do_not_disconnect_or_reinterpret_their_body() {
    for declared in [0, 1, 7, 1000, u32::MAX] {
        let mut input = ack(10);
        let mut optional = pdu(0x16, &[0; 12]);
        optional[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        optional[4..8].copy_from_slice(&declared.to_le_bytes());
        optional.extend(ack(999));
        input.extend(optional);
        let messages = prepare(&input, &mut Observations::default()).unwrap();
        assert_eq!(messages.len(), 1, "optional tail must not be guessed");
    }
    for length in 2..8 {
        let optional = &pdu(0x16, &[])[..length];
        assert!(prepare(optional, &mut Observations::default())
            .unwrap()
            .is_empty());
    }
}

#[test]
fn known_malformed_tails_reject_the_whole_preparation() {
    for command in [0x0du16, 0x10, 0x12] {
        for length in 2..8 {
            let mut data = ack(10);
            let short = pdu(command, &[]);
            data.extend_from_slice(&short[..length]);
            assert!(prepare(&data, &mut Observations::default()).is_err());
        }
        let mut data = ack(10);
        data.extend(pdu(command, &[]));
        assert!(prepare(&data, &mut Observations::default()).is_err());
    }
    let mut input = ack(10);
    input.push(0x0d);
    assert!(prepare(&input, &mut Observations::default()).is_err());
}

#[test]
fn supported_reserved_flags_are_ignored_and_body_bounds_stay_in_force() {
    let mut data = ack(10);
    data[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        prepare(&data, &mut Observations::default()).unwrap().len(),
        1
    );
    for declared in [0, 1, 7, 8, 19, 21, u32::MAX] {
        data[4..8].copy_from_slice(&declared.to_le_bytes());
        assert!(prepare(&data, &mut Observations::default()).is_err());
    }
}

#[test]
fn optional_observation_budget_and_batch_size_are_fixed() {
    let mut observations = Observations::default();
    for _ in 0..20 {
        assert!(prepare(&pdu(0x16, &[]), &mut observations)
            .unwrap()
            .is_empty());
    }
    assert_eq!(observations.ignored, IGNORED_LOG_LIMIT);
    assert!(prepare(&vec![0; MAX_MESSAGE_BYTES + 1], &mut observations).is_err());
    let mut batch = Vec::new();
    for _ in 0..MAX_COMMANDS {
        batch.extend(pdu(0x16, &[]));
    }
    assert!(prepare(&batch, &mut observations).unwrap().is_empty());
}
