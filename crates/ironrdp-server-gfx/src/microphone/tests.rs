use std::sync::{Arc, Mutex};

use ironrdp_core::encode_vec;
use ironrdp_dvc::DvcProcessor;

use super::{
    pdu, MicrophoneChannel, MicrophoneControl, MicrophonePcmFormat, MicrophoneServerBackend,
    FORMATS, MAX_PDU_SIZE,
};

// A Guacamole-style reply accepting only PCM16, 44.1 kHz stereo. The
// cbSizeFormatsPacket is 27, including its 9-byte header and WAVEFORMATEX.
pub(crate) const GUAC_FORMATS: &[u8] = &[
    2, 1, 0, 0, 0, 27, 0, 0, 0, 1, 0, 2, 0, 0x44, 0xac, 0, 0, 0x10, 0xb1, 2, 0, 4, 0, 16, 0, 0, 0,
];
const VERSION: &[u8] = &[1, 1, 0, 0, 0];
const CONFIRM: &[u8] = &[7, 0, 0, 0, 0];
const OPEN_OK: &[u8] = &[4, 0, 0, 0, 0];

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Observed {
    Start(MicrophonePcmFormat),
    Data(usize),
    Stop,
}

pub(crate) struct Backend {
    pub(crate) observed: Arc<Mutex<Vec<Observed>>>,
    pub(crate) fail_start: bool,
    pub(crate) fail_data: bool,
}

impl MicrophoneServerBackend for Backend {
    fn start(&mut self, format: MicrophonePcmFormat) -> anyhow::Result<()> {
        self.observed.lock().unwrap().push(Observed::Start(format));
        anyhow::ensure!(!self.fail_start, "test start failure");
        Ok(())
    }

    fn audio_data(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.observed
            .lock()
            .unwrap()
            .push(Observed::Data(data.len()));
        anyhow::ensure!(!self.fail_data, "test data failure");
        Ok(())
    }

    fn stop(&mut self) {
        self.observed.lock().unwrap().push(Observed::Stop);
    }
}

fn channel() -> (
    MicrophoneChannel,
    MicrophoneControl,
    Arc<Mutex<Vec<Observed>>>,
) {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (channel, control) = MicrophoneChannel::new(Box::new(Backend {
        observed: observed.clone(),
        fail_start: false,
        fail_data: false,
    }));
    (channel, control, observed)
}

fn send(channel: &mut MicrophoneChannel, bytes: &[u8]) -> Vec<Vec<u8>> {
    channel
        .process(12, bytes)
        .unwrap()
        .iter()
        .map(|message| encode_vec(message.as_ref()).unwrap())
        .collect()
}

fn negotiate(channel: &mut MicrophoneChannel) {
    channel.start(12).unwrap();
    send(channel, VERSION);
    send(channel, GUAC_FORMATS);
    send(channel, CONFIRM);
    send(channel, OPEN_OK);
}

#[test]
fn guacamole_pcm_negotiation_and_packet_delivery() {
    let (mut channel, control, observed) = channel();
    let version = channel.start(12).unwrap();
    assert_eq!(encode_vec(version[0].as_ref()).unwrap(), VERSION);
    let formats = send(&mut channel, VERSION);
    assert_eq!(&formats[0][..9], &[2, 4, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&formats[0][9..27], &GUAC_FORMATS[9..]);
    assert!(send(&mut channel, &[5]).is_empty());
    let open = send(&mut channel, GUAC_FORMATS);
    assert_eq!(&open[0][..9], &[3, 0xb9, 1, 0, 0, 0, 0, 0, 0]); // 441 frames
    assert_eq!(&open[0][9..], &GUAC_FORMATS[9..]);
    assert!(observed.lock().unwrap().is_empty());
    send(&mut channel, CONFIRM);
    send(&mut channel, OPEN_OK);
    let mut packet = vec![6];
    packet.extend_from_slice(&[0; 1764]);
    send(&mut channel, &[5]);
    send(&mut channel, &packet);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![Observed::Start(FORMATS[0]), Observed::Data(1764)]
    );
    assert!(!control.is_closed());
    drop(channel);
    assert_eq!(observed.lock().unwrap().last(), Some(&Observed::Stop));
}

#[test]
fn windows_filtered_client_order_selects_48khz_mono() {
    let (mut channel, _, observed) = channel();
    channel.start(12).unwrap();
    send(&mut channel, &[1, 2, 0, 0, 0]);
    // Client chooses an order independent of the server's format indices.
    let reply = encode_vec(pdu::formats(&[FORMATS[2], FORMATS[0]]).as_ref()).unwrap();
    let open = send(&mut channel, &reply);
    assert_eq!(&open[0][..9], &[3, 0xe0, 1, 0, 0, 0, 0, 0, 0]); // 480 frames, index 0
    assert_eq!(&open[0][11..17], &[1, 0, 0x80, 0xbb, 0, 0]); // mono, 48000 Hz
    send(&mut channel, CONFIRM);
    send(&mut channel, &[4, 1, 0, 0, 0]); // Any nonnegative HRESULT succeeds.
    let mut packet = vec![6];
    packet.extend_from_slice(&[0; 960]);
    send(&mut channel, &packet);
    assert_eq!(
        *observed.lock().unwrap(),
        vec![Observed::Start(FORMATS[2]), Observed::Data(960)]
    );
}

#[test]
fn out_of_sequence_and_wrong_size_audio_never_reaches_backend() {
    let (mut channel, _, observed) = channel();
    channel.start(12).unwrap();
    send(&mut channel, OPEN_OK);
    send(&mut channel, GUAC_FORMATS);
    send(&mut channel, &[6, 0, 0]);
    assert!(observed.lock().unwrap().is_empty());
    send(&mut channel, VERSION);
    send(&mut channel, GUAC_FORMATS);
    send(&mut channel, &[7, 1, 0, 0, 0]); // Incorrect initial format index.
    send(&mut channel, OPEN_OK);
    assert!(observed.lock().unwrap().is_empty());
    send(&mut channel, CONFIRM);
    send(&mut channel, OPEN_OK);
    for size in [0, 1, 1763, 1765, MAX_PDU_SIZE] {
        let mut packet = vec![6];
        packet.resize(size + 1, 0);
        send(&mut channel, &packet);
    }
    assert_eq!(*observed.lock().unwrap(), vec![Observed::Start(FORMATS[0])]);
}

#[test]
fn malformed_format_lists_are_bounded_and_do_not_advance_negotiation() {
    let (mut channel, _, observed) = channel();
    channel.start(12).unwrap();
    send(&mut channel, VERSION);
    for length in 0..GUAC_FORMATS.len() {
        assert!(send(&mut channel, &GUAC_FORMATS[..length]).is_empty());
    }
    for changed_byte in [9, 11, 13, 17, 21, 23, 25] {
        let mut reply = GUAC_FORMATS.to_vec();
        reply[changed_byte] ^= 0x80;
        assert!(send(&mut channel, &reply).is_empty());
    }
    let mut count_overflow = GUAC_FORMATS.to_vec();
    count_overflow[1..5].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(send(&mut channel, &count_overflow).is_empty());
    let mut oversized = GUAC_FORMATS.to_vec();
    oversized.resize(MAX_PDU_SIZE + 1, 0);
    assert!(send(&mut channel, &oversized).is_empty());
    assert!(observed.lock().unwrap().is_empty());
    assert_eq!(send(&mut channel, GUAC_FORMATS)[0][0], 3);
}

#[test]
fn format_trailing_extra_data_is_ignored_within_bound() {
    let (mut channel, _, _) = channel();
    channel.start(12).unwrap();
    send(&mut channel, VERSION);
    let mut reply = GUAC_FORMATS.to_vec();
    reply.extend_from_slice(&[99, 88, 77]);
    assert_eq!(send(&mut channel, &reply)[0][0], 3);
}

#[test]
fn short_complete_pcm_frames_are_accepted_up_to_packet_budget() {
    let (mut channel, _, observed) = channel();
    negotiate(&mut channel);
    for size in [4, 8, 880, 1764] {
        let mut packet = vec![6];
        packet.resize(size + 1, 0);
        send(&mut channel, &packet);
    }
    assert_eq!(
        *observed.lock().unwrap(),
        vec![
            Observed::Start(FORMATS[0]),
            Observed::Data(4),
            Observed::Data(8),
            Observed::Data(880),
            Observed::Data(1764)
        ]
    );
}

#[test]
fn empty_formats_and_client_denial_close_only_the_microphone() {
    for denial in [false, true] {
        let (mut channel, control, observed) = channel();
        channel.start(12).unwrap();
        send(&mut channel, VERSION);
        if denial {
            send(&mut channel, GUAC_FORMATS);
            send(&mut channel, CONFIRM);
            send(&mut channel, &[4, 5, 0, 7, 0x80]); // E_ACCESSDENIED
        } else {
            send(&mut channel, &[2, 0, 0, 0, 0, 9, 0, 0, 0]);
        }
        assert!(control.is_closed());
        assert_eq!(control.take_close_request(), Some(12));
        assert_eq!(control.take_close_request(), None);
        drop(channel);
        assert_eq!(*observed.lock().unwrap(), vec![Observed::Stop]);
    }
}

#[test]
fn backend_failures_stop_once_and_request_channel_close() {
    for fail_start in [false, true] {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let (mut channel, control) = MicrophoneChannel::new(Box::new(Backend {
            observed: observed.clone(),
            fail_start,
            fail_data: !fail_start,
        }));
        negotiate(&mut channel);
        let mut packet = vec![6];
        packet.extend_from_slice(&[0; 1764]);
        send(&mut channel, &packet);
        send(&mut channel, &packet);
        assert!(control.is_closed());
        assert_eq!(control.take_close_request(), Some(12));
        assert!(!control.peer_closed(12)); // Acknowledgement of our own close.
        drop(channel);
        let observed = observed.lock().unwrap();
        assert_eq!(
            observed
                .iter()
                .filter(|event| **event == Observed::Stop)
                .count(),
            1
        );
        assert_eq!(
            observed
                .iter()
                .filter(|event| matches!(event, Observed::Data(_)))
                .count(),
            usize::from(!fail_start)
        );
    }
}

#[test]
fn peer_close_and_transport_drop_stop_unstarted_or_active_consumers_once() {
    for active in [false, true] {
        let (mut channel, control, observed) = channel();
        if active {
            negotiate(&mut channel);
        } else {
            channel.start(12).unwrap();
        }
        assert!(!control.peer_closed(99));
        assert!(control.peer_closed(12));
        channel.close(12);
        assert!(send(&mut channel, OPEN_OK).is_empty());
        assert!(channel.start(12).unwrap().is_empty());
        drop(channel);
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
    let (channel, _, observed) = channel();
    drop(channel);
    assert_eq!(*observed.lock().unwrap(), vec![Observed::Stop]);
}

#[test]
fn bounded_arbitrary_inputs_never_panic_or_allocate_from_declared_counts() {
    let mut seed = 19u64;
    for length in 0..1024 {
        let mut payload = Vec::with_capacity(length);
        for _ in 0..length {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            payload.push(seed.to_le_bytes()[0]);
        }
        let _ = pdu::parse(&payload);
    }
}
