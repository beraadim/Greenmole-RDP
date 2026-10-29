use ironrdp_core::impl_as_any;
use ironrdp_dvc::pdu::{ClosePdu, DrdynvcServerPdu};
use ironrdp_dvc::DrdynvcServer;
use ironrdp_pdu::{gcc::ChannelName, PduResult};
use ironrdp_svc::{CompressionCondition, SvcMessage, SvcProcessor, SvcServerProcessor};

use crate::microphone::MicrophoneControl;

mod microphone_guard;
use microphone_guard::{channel_header, MicrophoneDvcGuard};
mod svc_guard;
pub(crate) use svc_guard::DynamicChannelSvcGuard;
mod logical_guard;
use logical_guard::LogicalDvcGuard;
#[cfg(test)]
mod logical_tests;

/// The upstream DVC implementation logs rejected channel creation only at
/// debug level. Surface negotiation state without logging channel payloads.
#[derive(Debug)]
pub(crate) struct DynamicChannels {
    inner: DrdynvcServer,
    microphone: Option<MicrophoneDvcGuard>,
    logical_input: LogicalDvcGuard,
}

impl DynamicChannels {
    pub(crate) fn new(inner: DrdynvcServer, microphone: Option<MicrophoneControl>) -> Self {
        Self {
            inner,
            microphone: microphone.map(MicrophoneDvcGuard::new),
            logical_input: LogicalDvcGuard::default(),
        }
    }
}

fn close_message(channel_id: u32) -> SvcMessage {
    SvcMessage::from(DrdynvcServerPdu::Close(ClosePdu::new(channel_id)))
        .with_flags(ironrdp_svc::ChannelFlags::SHOW_PROTOCOL)
}

impl_as_any!(DynamicChannels);

impl SvcProcessor for DynamicChannels {
    fn channel_name(&self) -> ChannelName {
        self.inner.channel_name()
    }

    fn compression_condition(&self) -> CompressionCondition {
        self.inner.compression_condition()
    }

    fn start(&mut self) -> PduResult<Vec<SvcMessage>> {
        tracing::info!("Starting dynamic channel negotiation");
        self.inner.start()
    }

    fn process(&mut self, payload: &[u8]) -> PduResult<Vec<SvcMessage>> {
        // Reject oversized microphone declarations before upstream's generic
        // DVC reassembler can retain any fragments. Other channels keep their
        // own existing protocol policies.
        if let Some(microphone) = self.microphone.as_mut() {
            if !microphone.preflight(payload) {
                return Ok(microphone
                    .control
                    .take_close_request()
                    .map(close_message)
                    .into_iter()
                    .collect());
            }
        }
        // A logical DVC message may span many independently valid SVC PDUs.
        // Bound the generic legacy reassembler for graphics/control/input too.
        self.logical_input.check(payload)?;
        let mut response = self.inner.process(payload)?;
        if let Some(microphone) = self.microphone.as_mut() {
            // This old DrdynvcServer validates a peer Close but fails to call
            // DvcProcessor::close. Stop the consumer immediately, even if the
            // rest of the desktop connection remains open indefinitely.
            if let Some((4, channel_id, _)) = channel_header(payload) {
                if microphone.control.peer_closed(channel_id) {
                    response.push(close_message(channel_id));
                }
            }
            if let Some(channel_id) = microphone.control.take_close_request() {
                response.push(close_message(channel_id));
            }
        }
        // MS-RDPEDYC puts Cmd in the upper four bits of the DVC header:
        // Create=1, Close=4, Capability=5. Payload is already dechunked by
        // StaticVirtualChannel, and successful processing above validates it.
        // Avoid reparsing or logging data messages (input, audio or graphics).
        if matches!(payload.first().map(|header| header >> 4), Some(1 | 4 | 5)) {
            tracing::info!(channels = ?self.inner, "Dynamic channel negotiation state");
        }
        Ok(response)
    }
}

impl SvcServerProcessor for DynamicChannels {}
