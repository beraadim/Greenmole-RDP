use std::sync::{Arc, Mutex};

use ironrdp_core::{encode_vec, impl_as_any};
use ironrdp_dvc::pdu::{
    CapabilitiesResponsePdu, CapsVersion, CreateResponsePdu, CreationStatus, DataFirstPdu, DataPdu,
    DrdynvcClientPdu, DrdynvcDataPdu,
};
use ironrdp_dvc::{DrdynvcServer, DvcMessage, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::PduResult;
use ironrdp_svc::SvcProcessor;

use super::DynamicChannels;

struct Recorder {
    name: &'static str,
    lengths: Arc<Mutex<Vec<usize>>>,
}
impl_as_any!(Recorder);
impl DvcProcessor for Recorder {
    fn channel_name(&self) -> &str {
        self.name
    }
    fn start(&mut self, _: u32) -> PduResult<Vec<DvcMessage>> {
        Ok(Vec::new())
    }
    fn process(&mut self, _: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        self.lengths.lock().unwrap().push(payload.len());
        Ok(Vec::new())
    }
}
impl DvcServerProcessor for Recorder {}

fn setup() -> (DynamicChannels, Vec<Arc<Mutex<Vec<usize>>>>) {
    let mut inner = DrdynvcServer::new();
    let mut observed = Vec::new();
    for name in ["display", "input", "graphics", "microphone"] {
        let lengths = Arc::new(Mutex::new(Vec::new()));
        inner = inner.with_dynamic_channel(Recorder {
            name,
            lengths: lengths.clone(),
        });
        observed.push(lengths);
    }
    let mut channels = DynamicChannels::new(inner, None);
    channels
        .process(
            &encode_vec(&DrdynvcClientPdu::Capabilities(
                CapabilitiesResponsePdu::new(CapsVersion::V1),
            ))
            .unwrap(),
        )
        .unwrap();
    for channel in 0..4 {
        channels
            .process(
                &encode_vec(&DrdynvcClientPdu::Create(CreateResponsePdu::new(
                    channel,
                    CreationStatus::OK,
                )))
                .unwrap(),
            )
            .unwrap();
    }
    (channels, observed)
}

fn first(channel_id: u32, declared: u32, actual: usize) -> Vec<u8> {
    encode_vec(&DrdynvcClientPdu::Data(DrdynvcDataPdu::DataFirst(
        DataFirstPdu::new(channel_id, declared, vec![1; actual]),
    )))
    .unwrap()
}

fn data(channel_id: u32, actual: usize) -> Vec<u8> {
    encode_vec(&DrdynvcClientPdu::Data(DrdynvcDataPdu::Data(DataPdu::new(
        channel_id,
        vec![2; actual],
    ))))
    .unwrap()
}

#[test]
fn interleaved_fragments_on_all_registered_channels_complete_independently() {
    let (mut channels, observed) = setup();
    for channel in 0..4 {
        channels.process(&first(channel, 1700, 1590)).unwrap();
    }
    for channel in (0..4).rev() {
        channels.process(&data(channel, 110)).unwrap();
    }
    for lengths in observed {
        assert_eq!(*lengths.lock().unwrap(), vec![1700]);
    }
}

#[test]
fn adversarial_large_declarations_and_unknown_ids_fail_before_reassembly() {
    for channel in 0..4 {
        for declared in [65_543, u32::MAX] {
            let (mut channels, observed) = setup();
            assert!(channels.process(&first(channel, declared, 1)).is_err());
            assert!(observed
                .iter()
                .all(|lengths| lengths.lock().unwrap().is_empty()));
        }
    }
    for channel in [4, 255, 65_535, u32::MAX] {
        let (mut channels, observed) = setup();
        assert!(channels.process(&first(channel, 65_536, 1)).is_err());
        assert!(channels.process(&data(channel, 1)).is_err());
        assert!(observed
            .iter()
            .all(|lengths| lengths.lock().unwrap().is_empty()));
    }
}

#[test]
fn repeated_small_fragments_cannot_exceed_any_channels_declared_budget() {
    let (mut channels, observed) = setup();
    // Four independent incomplete messages exercise the aggregate fixed slot
    // limit. No peer-provided channel ID can grow a map or allocate another slot.
    for channel in 0..4 {
        channels.process(&first(channel, 65_536, 1)).unwrap();
    }
    for _ in 0..40 {
        for channel in 0..4 {
            channels.process(&data(channel, 1600)).unwrap();
        }
    }
    for channel in 0..4 {
        assert!(channels.process(&data(channel, 1600)).is_err());
    }
    assert!(observed
        .iter()
        .all(|lengths| lengths.lock().unwrap().is_empty()));
}

#[test]
fn exactly_bounded_messages_complete_and_replacement_first_is_rejected() {
    let (mut channels, observed) = setup();
    channels.process(&first(2, 65_542, 1)).unwrap();
    for _ in 0..40 {
        channels.process(&data(2, 1600)).unwrap();
    }
    channels.process(&data(2, 1541)).unwrap();
    assert_eq!(*observed[2].lock().unwrap(), vec![65_542]);
    channels.process(&first(2, 10, 1)).unwrap();
    assert!(channels.process(&first(2, 10, 1)).is_err());
}

#[test]
fn maximum_valid_cache_offer_crosses_fragment_guard_and_receives_empty_reply() {
    use ironrdp_core::decode;
    use ironrdp_dvc::pdu::DrdynvcServerPdu;
    use ironrdp_svc::StaticVirtualChannel;

    use crate::gfx::{GfxHandler, GfxState};

    let state = Arc::new(Mutex::new(GfxState::new(640, 480, false, false)));
    let inner = DrdynvcServer::new().with_dynamic_channel(GfxHandler::new(state));
    let mut channels = DynamicChannels::new(inner, None);
    channels
        .process(
            &encode_vec(&DrdynvcClientPdu::Capabilities(
                CapabilitiesResponsePdu::new(CapsVersion::V1),
            ))
            .unwrap(),
        )
        .unwrap();
    channels
        .process(
            &encode_vec(&DrdynvcClientPdu::Create(CreateResponsePdu::new(
                0,
                CreationStatus::OK,
            )))
            .unwrap(),
        )
        .unwrap();

    // Maximum valid raw client CacheImportOffer: the metadata is deliberately
    // opaque here, just as it is to the zero-import server path.
    let mut offer = vec![0; 65_542];
    offer[..2].copy_from_slice(&0x10u16.to_le_bytes());
    offer[4..8].copy_from_slice(&65_542u32.to_le_bytes());
    offer[8..10].copy_from_slice(&5461u16.to_le_bytes());
    let mut fragments = offer.chunks(1400);
    let first_fragment = encode_vec(&DrdynvcClientPdu::Data(DrdynvcDataPdu::DataFirst(
        DataFirstPdu::new(0, 65_542, fragments.next().unwrap().to_vec()),
    )))
    .unwrap();
    assert!(channels.process(&first_fragment).unwrap().is_empty());
    let mut replies = Vec::new();
    for chunk in fragments {
        replies.extend(
            channels
                .process(
                    &encode_vec(&DrdynvcClientPdu::Data(DrdynvcDataPdu::Data(DataPdu::new(
                        0,
                        chunk.to_vec(),
                    ))))
                    .unwrap(),
                )
                .unwrap(),
        );
    }
    let chunks = StaticVirtualChannel::chunkify(replies).unwrap();
    assert_eq!(chunks.len(), 1);
    let reply: DrdynvcServerPdu = decode(&chunks[0].filled()[8..]).unwrap();
    let DrdynvcServerPdu::Data(DrdynvcDataPdu::Data(reply)) = reply else {
        panic!("expected a graphics reply on the original channel")
    };
    // Normal ZGFX framing followed by one zero-entry CacheImportReply.
    assert_eq!(reply.data(), [0xe0, 4, 0x11, 0, 0, 0, 10, 0, 0, 0, 0, 0]);

    // The one-byte-larger declaration must fail before the reassembler or GFX
    // handler sees any metadata, including after a successful maximum offer.
    assert!(channels.process(&first(0, 65_543, 1)).is_err());
    assert!(channels.process(&data(0, 65_543)).is_err());
}
