use std::sync::{Arc, Mutex};

use ironrdp_core::{decode, encode_vec, impl_as_any};
use ironrdp_dvc::pdu::{
    CapabilitiesResponsePdu, CapsVersion, ClosePdu, CreateResponsePdu, CreationStatus,
    DataFirstPdu, DataPdu, DrdynvcClientPdu, DrdynvcDataPdu, DrdynvcServerPdu,
};
use ironrdp_dvc::{DrdynvcServer, DvcMessage, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::{mcs::SendDataIndication, x224::X224, PduResult};
use ironrdp_svc::{server_encode_svc_messages, SvcMessage, SvcProcessor};

use super::tests::{Backend, Observed, GUAC_FORMATS};
use super::{MicrophoneChannel, FORMATS};
use crate::dynamic_channels::DynamicChannels;

struct Probe(Arc<Mutex<usize>>);
impl_as_any!(Probe);
impl DvcProcessor for Probe {
    fn channel_name(&self) -> &str {
        "test_other_channel"
    }
    fn start(&mut self, _: u32) -> PduResult<Vec<DvcMessage>> {
        Ok(Vec::new())
    }
    fn process(&mut self, _: u32, _: &[u8]) -> PduResult<Vec<DvcMessage>> {
        *self.0.lock().unwrap() += 1;
        Ok(Vec::new())
    }
}
impl DvcServerProcessor for Probe {}

fn process(channels: &mut DynamicChannels, pdu: DrdynvcClientPdu) -> Vec<SvcMessage> {
    channels.process(&encode_vec(&pdu).unwrap()).unwrap()
}

fn data(channels: &mut DynamicChannels, bytes: &[u8]) -> Vec<SvcMessage> {
    process(
        channels,
        DrdynvcClientPdu::Data(DrdynvcDataPdu::Data(DataPdu::new(0, bytes.to_vec()))),
    )
}

fn setup(
    fail_start: bool,
) -> (
    DynamicChannels,
    Arc<Mutex<Vec<Observed>>>,
    Arc<Mutex<usize>>,
) {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let probe = Arc::new(Mutex::new(0));
    let (channel, control) = MicrophoneChannel::new(Box::new(Backend {
        observed: observed.clone(),
        fail_start,
        fail_data: false,
    }));
    let mut channels = DynamicChannels::new(
        DrdynvcServer::new()
            .with_dynamic_channel(channel)
            .with_dynamic_channel(Probe(probe.clone())),
        Some(control),
    );
    channels.start().unwrap();
    process(
        &mut channels,
        DrdynvcClientPdu::Capabilities(CapabilitiesResponsePdu::new(CapsVersion::V1)),
    );
    for id in [0, 1] {
        process(
            &mut channels,
            DrdynvcClientPdu::Create(CreateResponsePdu::new(id, CreationStatus::OK)),
        );
    }
    data(&mut channels, &[1, 1, 0, 0, 0]);
    data(&mut channels, GUAC_FORMATS);
    data(&mut channels, &[7, 0, 0, 0, 0]);
    let response = data(&mut channels, &[4, 0, 0, 0, 0]);
    if fail_start {
        assert_close(response);
    } else {
        assert!(response.is_empty());
    }
    (channels, observed, probe)
}

fn assert_close(messages: Vec<SvcMessage>) {
    assert_eq!(messages.len(), 1);
    let wire = server_encode_svc_messages(messages, 1005, 1001).unwrap();
    let mcs: X224<SendDataIndication<'_>> = decode(&wire).unwrap();
    let pdu: DrdynvcServerPdu = decode(&mcs.0.user_data[8..]).unwrap();
    assert!(matches!(pdu, DrdynvcServerPdu::Close(close) if close.channel_id() == 0));
}

fn assert_other_channel_usable(channels: &mut DynamicChannels, probe: &Arc<Mutex<usize>>) {
    process(
        channels,
        DrdynvcClientPdu::Data(DrdynvcDataPdu::Data(DataPdu::new(1, vec![42]))),
    );
    assert_eq!(*probe.lock().unwrap(), 1);
}

#[test]
fn peer_close_stops_microphone_without_stopping_other_channels() {
    let (mut channels, observed, probe) = setup(false);
    assert_close(process(
        &mut channels,
        DrdynvcClientPdu::Close(ClosePdu::new(0)),
    ));
    assert_other_channel_usable(&mut channels, &probe);
    drop(channels);
    assert_eq!(
        observed
            .lock()
            .unwrap()
            .iter()
            .filter(|event| **event == Observed::Stop)
            .count(),
        1
    );
}

#[test]
fn backend_failure_closes_microphone_and_quarantines_later_data() {
    let (mut channels, observed, probe) = setup(true);
    assert!(data(&mut channels, &[6, 0, 0]).is_empty());
    assert_other_channel_usable(&mut channels, &probe);
    // Acknowledge the server's Close. It must not trigger another Close loop.
    assert!(process(&mut channels, DrdynvcClientPdu::Close(ClosePdu::new(0))).is_empty());
    drop(channels);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![Observed::Start(FORMATS[0]), Observed::Stop]
    );
}

#[test]
fn oversized_fragment_declaration_closes_before_legacy_reassembly() {
    let (mut channels, observed, probe) = setup(false);
    assert_close(process(
        &mut channels,
        DrdynvcClientPdu::Data(DrdynvcDataPdu::DataFirst(DataFirstPdu::new(
            0,
            u32::MAX,
            vec![6],
        ))),
    ));
    assert!(data(&mut channels, &[0; 64]).is_empty());
    assert_other_channel_usable(&mut channels, &probe);
    drop(channels);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![Observed::Start(FORMATS[0]), Observed::Stop]
    );
}

#[test]
fn normal_fragmented_microphone_packet_reaches_consumer_once() {
    let (mut channels, observed, _) = setup(false);
    let mut packet = vec![6];
    packet.extend_from_slice(&[0; 1764]);
    assert!(process(
        &mut channels,
        DrdynvcClientPdu::Data(DrdynvcDataPdu::DataFirst(DataFirstPdu::new(
            0,
            1765,
            packet[..1000].to_vec()
        ),))
    )
    .is_empty());
    assert!(data(&mut channels, &packet[1000..]).is_empty());
    assert_eq!(
        *observed.lock().unwrap(),
        vec![Observed::Start(FORMATS[0]), Observed::Data(1764)]
    );
}

#[test]
fn malformed_fragment_sequences_close_only_the_microphone() {
    for extra_fragment in [false, true] {
        let (mut channels, observed, probe) = setup(false);
        assert!(process(
            &mut channels,
            DrdynvcClientPdu::Data(DrdynvcDataPdu::DataFirst(DataFirstPdu::new(0, 10, vec![6]),))
        )
        .is_empty());
        let response = if extra_fragment {
            data(&mut channels, &[0; 10]) // More bytes than the declared remainder.
        } else {
            process(
                &mut channels,
                DrdynvcClientPdu::Data(DrdynvcDataPdu::DataFirst(DataFirstPdu::new(
                    0,
                    10,
                    vec![6],
                ))),
            ) // Replaces an incomplete message.
        };
        assert_close(response);
        assert_other_channel_usable(&mut channels, &probe);
        drop(channels);
        assert_eq!(
            observed
                .lock()
                .unwrap()
                .iter()
                .filter(|event| **event == Observed::Stop)
                .count(),
            1
        );
    }
}
