use std::sync::Arc;

use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_displaycontrol::server::DisplayControlHandler;
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

use crate::display::RdpServerDisplay;

/// One consumer preserves layout order and coalesces requests while activation
/// holds the display lock. Its lifetime follows the Display Control channel.
pub(crate) struct DisplayControlBackend {
    layouts: watch::Sender<Option<DisplayControlMonitorLayout>>,
    worker: JoinHandle<()>,
}

impl DisplayControlBackend {
    pub(crate) fn new(display: Arc<Mutex<Box<dyn RdpServerDisplay>>>) -> Self {
        let (layouts, mut receiver) = watch::channel(None);
        let worker = tokio::spawn(async move {
            while receiver.changed().await.is_ok() {
                let mut display = display.lock().await;
                // Read after acquiring the lock: requests received during
                // capture activation replace any older pending layout.
                let layout = receiver.borrow_and_update().clone();
                if let Some(layout) = layout {
                    display.request_layout(layout);
                }
            }
        });
        Self { layouts, worker }
    }
}

impl DisplayControlHandler for DisplayControlBackend {
    fn monitor_layout(&self, layout: DisplayControlMonitorLayout) {
        self.layouts.send_replace(Some(layout));
    }
}

impl Drop for DisplayControlBackend {
    fn drop(&mut self) {
        // Closing the watch sender alone would still allow a consumer already
        // waiting for the display lock to apply a previous client's layout.
        self.worker.abort();
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use ironrdp_displaycontrol::server::DisplayControlServer;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    use super::*;
    use crate::display::{DesktopSize, RdpServerDisplayUpdates};

    struct RecordingDisplay(mpsc::UnboundedSender<(u32, u32)>);

    #[async_trait::async_trait]
    impl RdpServerDisplay for RecordingDisplay {
        async fn size(&mut self) -> DesktopSize {
            DesktopSize {
                width: 1920,
                height: 1080,
            }
        }

        async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
            anyhow::bail!("capture is not used by layout tests")
        }

        fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
            self.0.send(layout.monitors()[0].dimensions()).unwrap();
        }
    }

    fn layout(width: u32, height: u32) -> DisplayControlMonitorLayout {
        DisplayControlMonitorLayout::new_single_primary_monitor(width, height, None, None).unwrap()
    }

    #[tokio::test]
    async fn pending_layouts_apply_only_the_latest_after_activation_unlocks() {
        let (applied, mut requests) = mpsc::unbounded_channel();
        let display: Arc<Mutex<Box<dyn RdpServerDisplay>>> =
            Arc::new(Mutex::new(Box::new(RecordingDisplay(applied))));
        let activation = display.lock().await;
        let backend = DisplayControlBackend::new(Arc::clone(&display));
        backend.monitor_layout(layout(1280, 720));
        // Let the old request reach the locked display before newer requests
        // arrive. Independent blocking tasks could acquire it in any order.
        tokio::task::yield_now().await;
        backend.monitor_layout(layout(1600, 900));
        backend.monitor_layout(layout(2560, 1440));
        drop(activation);

        assert_eq!(
            timeout(Duration::from_secs(1), requests.recv())
                .await
                .unwrap(),
            Some((2560, 1440))
        );
        tokio::task::yield_now().await;
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        // The same channel remains usable after the queued burst is consumed.
        backend.monitor_layout(layout(1920, 1080));
        assert_eq!(
            timeout(Duration::from_secs(1), requests.recv())
                .await
                .unwrap(),
            Some((1920, 1080))
        );
    }

    #[tokio::test]
    async fn dropping_channel_cancels_layout_waiting_for_display_lock() {
        let (applied, mut requests) = mpsc::unbounded_channel();
        let display: Arc<Mutex<Box<dyn RdpServerDisplay>>> =
            Arc::new(Mutex::new(Box::new(RecordingDisplay(applied))));
        let activation = display.lock().await;
        let backend = DisplayControlBackend::new(Arc::clone(&display));
        backend.monitor_layout(layout(1280, 720));
        let worker = backend.worker.abort_handle();
        tokio::task::yield_now().await;

        // Moving the backend into its channel must preserve the pending worker;
        // disposing that channel must cancel it before another session uses it.
        let channel = DisplayControlServer::new(Box::new(backend));
        assert!(!worker.is_finished());
        drop(channel);
        timeout(Duration::from_secs(1), async {
            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(activation);
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        let next_session = DisplayControlBackend::new(display);
        next_session.monitor_layout(layout(1920, 1080));
        assert_eq!(
            timeout(Duration::from_secs(1), requests.recv())
                .await
                .unwrap(),
            Some((1920, 1080))
        );
    }
}
