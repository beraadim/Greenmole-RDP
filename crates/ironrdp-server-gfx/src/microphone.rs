//! Client microphone capture over the MS-RDPEAI `AUDIO_INPUT` dynamic channel.
//!
//! This is independent of RDPSND, which sends the desktop's sound to the client.
//! Only explicitly configured servers register the microphone channel.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use ironrdp_core::impl_as_any;
use ironrdp_dvc::{DvcMessage, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::PduResult;

#[cfg(test)]
mod dvc_tests;
mod pdu;
#[cfg(test)]
mod tests;

pub(crate) use pdu::MAX_PDU_SIZE;

/// Interleaved, signed 16-bit little-endian PCM received from the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicrophonePcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// Supplies an isolated microphone consumer for each remote connection.
pub trait MicrophoneServerFactory {
    fn build_backend(&self) -> Box<dyn MicrophoneServerBackend>;
}

/// Receives client microphone samples on the RDP connection's task.
///
/// Methods must return promptly: enqueue into a bounded worker rather than
/// performing device I/O here. Errors must not include audio sample contents.
/// `stop` is called once when this connection ends, including failed negotiation,
/// a peer channel close, and a partially failed `start`. Implementations must
/// also release their resources on Drop, including any independent workers.
pub trait MicrophoneServerBackend: Send {
    fn start(&mut self, format: MicrophonePcmFormat) -> anyhow::Result<()>;
    fn audio_data(&mut self, data: &[u8]) -> anyhow::Result<()>;
    fn stop(&mut self);
}

// Guacamole selects the first PCM format offered. Keep the common 44.1 kHz
// stereo format first, and retain native Windows 48 kHz/mono alternatives.
const FORMATS: [MicrophonePcmFormat; 4] = [
    MicrophonePcmFormat {
        sample_rate: 44_100,
        channels: 2,
    },
    MicrophonePcmFormat {
        sample_rate: 48_000,
        channels: 2,
    },
    MicrophonePcmFormat {
        sample_rate: 48_000,
        channels: 1,
    },
    MicrophonePcmFormat {
        sample_rate: 44_100,
        channels: 1,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Initial,
    AwaitingVersion,
    AwaitingFormats,
    AwaitingFormatConfirm,
    AwaitingOpenReply,
    Opened,
    Closed,
}

struct Session {
    backend: Box<dyn MicrophoneServerBackend>,
    channel_id: Option<u32>,
    state: State,
    selected: Option<MicrophonePcmFormat>,
    selected_index: u32,
    packet_bytes: usize,
    stopped: bool,
    close_requested: bool,
    close_sent: bool,
    rejected: u64,
}

impl Session {
    fn stop(&mut self) {
        if !self.stopped {
            self.backend.stop();
            self.stopped = true;
        }
        self.state = State::Closed;
    }

    fn quarantine(&mut self) {
        self.stop();
        self.close_requested = self.channel_id.is_some();
    }

    fn rejected(&mut self) {
        self.rejected = self.rejected.saturating_add(1);
        // Never log network payloads or one message per malicious packet.
        if self.rejected == 1 || self.rejected.is_power_of_two() {
            tracing::warn!(
                rejected_messages = self.rejected,
                "Ignored invalid or out-of-sequence microphone message"
            );
        }
    }

    fn process(&mut self, payload: &[u8]) -> Vec<DvcMessage> {
        if self.state == State::Closed {
            return Vec::new();
        }
        let Some(message) = pdu::parse(payload) else {
            self.rejected();
            return Vec::new();
        };
        match message {
            pdu::ClientMessage::DataIncoming => {}
            pdu::ClientMessage::Version(version)
                if self.state == State::AwaitingVersion && matches!(version, 1 | 2) =>
            {
                self.state = State::AwaitingFormats;
                return vec![pdu::formats(&FORMATS)];
            }
            pdu::ClientMessage::Formats(formats) if self.state == State::AwaitingFormats => {
                // The client's reply establishes the final list and its order.
                // An index into our offered list would silently select a wrong
                // codec on clients that reorder or filter the advertised list.
                if formats.iter().any(|format| !FORMATS.contains(format)) {
                    self.rejected();
                    return Vec::new();
                }
                let Some(selected) = formats.first().copied() else {
                    tracing::info!("Client did not accept a supported microphone PCM format");
                    self.quarantine();
                    return Vec::new();
                };
                self.selected = Some(selected);
                self.selected_index = 0;
                // Ten milliseconds reduces latency and bounds every data PDU.
                let frames_per_packet = selected.sample_rate / 100;
                self.packet_bytes = usize::try_from(frames_per_packet).unwrap_or(0)
                    * usize::from(selected.channels)
                    * 2;
                self.state = State::AwaitingFormatConfirm;
                return vec![pdu::open(selected, frames_per_packet, self.selected_index)];
            }
            pdu::ClientMessage::FormatChange(index)
                if self.state == State::AwaitingFormatConfirm && index == self.selected_index =>
            {
                self.state = State::AwaitingOpenReply;
            }
            pdu::ClientMessage::OpenReply(result) if self.state == State::AwaitingOpenReply => {
                if result < 0 {
                    tracing::info!(
                        hresult = result,
                        "Client microphone capture was unavailable or denied"
                    );
                    self.quarantine();
                    return Vec::new();
                }
                let Some(format) = self.selected else {
                    self.quarantine();
                    return Vec::new();
                };
                if let Err(error) = self.backend.start(format) {
                    tracing::warn!(%error, "Failed to start remote microphone consumer");
                    self.quarantine();
                    return Vec::new();
                }
                self.state = State::Opened;
                tracing::info!(
                    sample_rate = format.sample_rate,
                    channels = format.channels,
                    packet_ms = 10,
                    "Client microphone capture started"
                );
            }
            pdu::ClientMessage::Data(data) if self.state == State::Opened => {
                // Tolerate a client's short final/capture fragment while still
                // requiring complete PCM frames and enforcing the requested
                // 10 ms maximum. Consumers accumulate partial packets if their
                // device needs fixed-size buffers.
                let frame_bytes = self
                    .selected
                    .map(|format| usize::from(format.channels) * 2)
                    .unwrap_or(0);
                if frame_bytes == 0
                    || data.is_empty()
                    || data.len() > self.packet_bytes
                    || data.len() % frame_bytes != 0
                {
                    self.rejected();
                    return Vec::new();
                }
                if let Err(error) = self.backend.audio_data(data) {
                    tracing::warn!(%error, "Remote microphone consumer stopped after an audio delivery error");
                    self.quarantine();
                }
            }
            _ => self.rejected(),
        }
        Vec::new()
    }
}

/// Shared only with the DVC wrapper to compensate for old IronRDP versions
/// not calling DvcProcessor::close when a peer closes its channel.
#[derive(Clone)]
pub(crate) struct MicrophoneControl(Arc<Mutex<Session>>);

impl fmt::Debug for MicrophoneControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock();
        f.debug_struct("MicrophoneControl")
            .field("channel_id", &state.channel_id)
            .field("state", &state.state)
            .finish()
    }
}

impl MicrophoneControl {
    fn lock(&self) -> MutexGuard<'_, Session> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn channel_id(&self) -> Option<u32> {
        self.lock().channel_id
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().state == State::Closed
    }

    pub(crate) fn quarantine(&self) {
        self.lock().quarantine();
    }

    pub(crate) fn take_close_request(&self) -> Option<u32> {
        let mut session = self.lock();
        if session.close_requested && !session.close_sent {
            session.close_sent = true;
            session.channel_id
        } else {
            None
        }
    }

    /// Return true only when a peer-initiated close needs an acknowledgement.
    pub(crate) fn peer_closed(&self, channel_id: u32) -> bool {
        let mut session = self.lock();
        if session.channel_id != Some(channel_id) {
            return false;
        }
        let acknowledge = !session.close_sent;
        session.close_sent = true;
        session.close_requested = false;
        session.stop();
        tracing::info!("Client microphone channel closed");
        acknowledge
    }
}

pub(crate) struct MicrophoneChannel {
    control: MicrophoneControl,
}

impl MicrophoneChannel {
    pub(crate) fn new(backend: Box<dyn MicrophoneServerBackend>) -> (Self, MicrophoneControl) {
        let control = MicrophoneControl(Arc::new(Mutex::new(Session {
            backend,
            channel_id: None,
            state: State::Initial,
            selected: None,
            selected_index: 0,
            packet_bytes: 0,
            stopped: false,
            close_requested: false,
            close_sent: false,
            rejected: 0,
        })));
        (
            Self {
                control: control.clone(),
            },
            control,
        )
    }
}

impl_as_any!(MicrophoneChannel);

impl DvcProcessor for MicrophoneChannel {
    fn channel_name(&self) -> &str {
        "AUDIO_INPUT"
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        let mut session = self.control.lock();
        // A closed channel is never allowed to revive a previous session's
        // consumer. A reconnect builds a fresh backend and fresh channel.
        if session.state != State::Initial {
            session.quarantine();
            return Ok(Vec::new());
        }
        session.channel_id = Some(channel_id);
        session.state = State::AwaitingVersion;
        tracing::info!(
            channel_id,
            "Client microphone channel opened; negotiating PCM capture"
        );
        Ok(vec![pdu::version()])
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        let mut session = self.control.lock();
        if session.channel_id != Some(channel_id) {
            session.rejected();
            return Ok(Vec::new());
        }
        Ok(session.process(payload))
    }

    fn close(&mut self, channel_id: u32) {
        self.control.peer_closed(channel_id);
    }
}

impl DvcServerProcessor for MicrophoneChannel {}

impl Drop for MicrophoneChannel {
    fn drop(&mut self) {
        self.control.lock().stop();
    }
}
