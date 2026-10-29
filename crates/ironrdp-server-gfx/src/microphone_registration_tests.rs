use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ironrdp_core::{decode, encode_vec};
use ironrdp_dvc::pdu::{CapabilitiesResponsePdu, CapsVersion, DrdynvcClientPdu, DrdynvcServerPdu};
use ironrdp_svc::{StaticVirtualChannel, SvcProcessor};

use crate::dynamic_channels::DynamicChannels;
use crate::{MicrophonePcmFormat, MicrophoneServerBackend, MicrophoneServerFactory, RdpServer};

struct Factory {
    built: Arc<AtomicUsize>,
    stopped: Arc<AtomicUsize>,
}

struct Backend(Arc<AtomicUsize>);

impl MicrophoneServerFactory for Factory {
    fn build_backend(&self) -> Box<dyn MicrophoneServerBackend> {
        self.built.fetch_add(1, Ordering::Relaxed);
        Box::new(Backend(self.stopped.clone()))
    }
}

impl MicrophoneServerBackend for Backend {
    fn start(&mut self, _: MicrophonePcmFormat) -> anyhow::Result<()> {
        Ok(())
    }
    fn audio_data(&mut self, _: &[u8]) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn microphone_is_opt_in_independent_of_output_and_graphics_and_fresh_per_connection() {
    for enabled in [false, true] {
        let built = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicUsize::new(0));
        let factory: Option<Box<dyn MicrophoneServerFactory>> = enabled.then(|| {
            Box::new(Factory {
                built: built.clone(),
                stopped: stopped.clone(),
            }) as Box<dyn MicrophoneServerFactory>
        });
        let mut server = RdpServer::builder()
            .with_addr(([127, 0, 0, 1], 0))
            .with_no_security()
            .with_no_input()
            .with_no_display()
            .with_microphone_factory(factory)
            .build();
        server.gfx_enabled = false;
        assert!(server.sound_factory.is_none());
        for connection in 1..=2 {
            let mut channels = server.dynamic_channels();
            let caps = encode_vec(&DrdynvcClientPdu::Capabilities(
                CapabilitiesResponsePdu::new(CapsVersion::V1),
            ))
            .unwrap();
            let chunks = StaticVirtualChannel::chunkify(channels.process(&caps).unwrap()).unwrap();
            let names: Vec<_> = chunks
                .iter()
                .map(
                    |chunk| match decode::<DrdynvcServerPdu>(&chunk.filled()[8..]).unwrap() {
                        DrdynvcServerPdu::Create(create) => create.channel_name().to_owned(),
                        _ => panic!("expected channel creation"),
                    },
                )
                .collect();
            assert_eq!(names.iter().any(|name| name == "AUDIO_INPUT"), enabled);
            assert_eq!(
                built.load(Ordering::Relaxed),
                if enabled { connection } else { 0 }
            );
            drop(channels);
            assert_eq!(
                stopped.load(Ordering::Relaxed),
                if enabled { connection } else { 0 }
            );
        }
    }
}

#[tokio::test]
async fn dynamic_svc_budget_is_checked_before_static_channel_reassembly() {
    use ironrdp_pdu::{mcs::SendDataRequest, x224::X224};
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build();
    let channels = server.dynamic_channels();
    server.static_channels.insert(channels);
    server
        .static_channels
        .attach_channel_id(core::any::TypeId::of::<DynamicChannels>(), 1005);
    let mut fragment = u32::MAX.to_le_bytes().to_vec();
    fragment.extend_from_slice(&1u32.to_le_bytes()); // FIRST without LAST.
    fragment.push(0x50);
    let wire = encode_vec(&X224(SendDataRequest {
        initiator_id: 1002,
        channel_id: 1005,
        user_data: fragment.into(),
    }))
    .unwrap();
    let result = server.handle_x224(1003, 1002, &wire).await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("oversized outer DVC declaration was accepted"),
    };
    assert!(error.to_string().contains("receive limit"));
}
