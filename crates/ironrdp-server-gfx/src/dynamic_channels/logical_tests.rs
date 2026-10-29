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
        for declared in [65_537, u32::MAX] {
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
    channels.process(&first(2, 65_536, 1)).unwrap();
    for _ in 0..40 {
        channels.process(&data(2, 1600)).unwrap();
    }
    channels.process(&data(2, 1535)).unwrap();
    assert_eq!(*observed[2].lock().unwrap(), vec![65_536]);
    channels.process(&first(2, 10, 1)).unwrap();
    assert!(channels.process(&first(2, 10, 1)).is_err());
}
