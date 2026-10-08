use ironrdp_core::impl_as_any;
use ironrdp_dvc::DrdynvcServer;
use ironrdp_pdu::{gcc::ChannelName, PduResult};
use ironrdp_svc::{CompressionCondition, SvcMessage, SvcProcessor, SvcServerProcessor};

/// The upstream DVC implementation logs rejected channel creation only at
/// debug level. Surface negotiation state without logging channel payloads.
#[derive(Debug)]
pub(crate) struct DynamicChannels(pub(crate) DrdynvcServer);

impl_as_any!(DynamicChannels);

impl SvcProcessor for DynamicChannels {
    fn channel_name(&self) -> ChannelName {
        self.0.channel_name()
    }

    fn compression_condition(&self) -> CompressionCondition {
        self.0.compression_condition()
    }

    fn start(&mut self) -> PduResult<Vec<SvcMessage>> {
        tracing::info!("Starting dynamic channel negotiation");
        self.0.start()
    }

    fn process(&mut self, payload: &[u8]) -> PduResult<Vec<SvcMessage>> {
        let response = self.0.process(payload)?;
        // MS-RDPEDYC puts Cmd in the upper four bits of the DVC header:
        // Create=1, Close=4, Capability=5. Payload is already dechunked by
        // StaticVirtualChannel, and successful processing above validates it.
        // Avoid reparsing or logging data messages (input, audio or graphics).
        if matches!(payload.first().map(|header| header >> 4), Some(1 | 4 | 5)) {
            tracing::info!(channels = ?self.0, "Dynamic channel negotiation state");
        }
        Ok(response)
    }
}

impl SvcServerProcessor for DynamicChannels {}
