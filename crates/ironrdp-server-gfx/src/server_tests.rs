use super::*;
use crate::{KeyboardEvent, MouseEvent, RdpServerDisplayUpdates};
use std::sync::Mutex as StdMutex;

#[derive(Default)]
struct InputLifecycle {
    contexts: Vec<String>,
    connections: usize,
    disconnections: usize,
}

struct RecordingInput(Arc<StdMutex<InputLifecycle>>);

impl RdpServerInputHandler for RecordingInput {
    fn update_client_context(&mut self, context: ClientInputContext) {
        self.0.lock().unwrap().contexts.push(context.client_name);
    }

    fn on_connect(&mut self, _: ClientInputContext, _: Option<String>) {
        self.0.lock().unwrap().connections += 1;
    }

    fn on_disconnect(&mut self) {
        self.0.lock().unwrap().disconnections += 1;
    }

    fn keyboard(&mut self, _: KeyboardEvent) {}
    fn mouse(&mut self, _: MouseEvent) {}
}

struct CaptureUnavailable;

#[async_trait::async_trait]
impl RdpServerDisplay for CaptureUnavailable {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: 1920,
            height: 1080,
        }
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        // Stop the activation deterministically after its input lifecycle
        // callbacks, without requiring a real client or platform capture API.
        bail!("capture deliberately unavailable")
    }
}

#[tokio::test]
async fn resize_reactivation_refreshes_context_without_starting_a_new_session() {
    let lifecycle = Arc::new(StdMutex::new(InputLifecycle::default()));
    let mut server = RdpServer::new(
        RdpServerOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            security: RdpServerSecurity::None,
            codecs: BitmapCodecs(Vec::new()),
        },
        Box::new(RecordingInput(Arc::clone(&lifecycle))),
        Box::new(CaptureUnavailable),
        None,
        None,
    );
    for (reactivation, client_name) in [(false, "initial"), (true, "resized")] {
        let (stream, _peer) = tokio::io::duplex(1024);
        let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let result = AcceptorResult {
            static_channels: StaticChannelSet::new(),
            capabilities: Vec::new(),
            input_events: Vec::new(),
            user_channel_id: 1002,
            io_channel_id: 1003,
            reactivation,
            client_name: client_name.to_owned(),
            keyboard_layout: 0x0409,
        };
        let error = server
            .client_accepted(
                &mut reader,
                &mut writer,
                result,
                DesktopSize {
                    width: 1280,
                    height: 720,
                },
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("capture deliberately unavailable"));
    }

    let lifecycle = lifecycle.lock().unwrap();
    assert_eq!(lifecycle.contexts, ["initial", "resized"]);
    assert_eq!(lifecycle.connections, 1);
    assert_eq!(lifecycle.disconnections, 0);
}
