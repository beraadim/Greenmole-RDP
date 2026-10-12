//! Completion timing belongs to the codec callback, never to its consumer.
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug)]
pub struct EncodeTiming {
    pub encode_duration: Duration,
    pub completed_at: Instant,
}

pub(crate) struct EncodeCompletion {
    started_at: Mutex<Option<Instant>>,
    ready: watch::Sender<Option<EncodeTiming>>,
}

impl Default for EncodeCompletion {
    fn default() -> Self {
        Self {
            started_at: Mutex::new(None),
            ready: watch::channel(None).0,
        }
    }
}

impl EncodeCompletion {
    pub fn begin(&self) {
        *self
            .started_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
        self.ready.send_replace(None);
    }

    pub fn finish(&self) {
        let completed_at = Instant::now();
        let encode_duration = self
            .started_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .map(|started| completed_at.saturating_duration_since(started))
            .unwrap_or_default();
        self.ready.send_replace(Some(EncodeTiming {
            encode_duration,
            completed_at,
        }));
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<EncodeTiming>> {
        self.ready.subscribe()
    }
    pub fn timing(&self) -> Option<EncodeTiming> {
        *self.ready.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completion_wakes_without_another_capture_and_survives_late_subscription() {
        let completion = EncodeCompletion::default();
        completion.begin();
        let mut ready = completion.subscribe();
        completion.finish();
        assert!(ready
            .wait_for(|timing| timing.is_some())
            .await
            .unwrap()
            .is_some());
        let mut late = completion.subscribe();
        assert!(late
            .wait_for(|timing| timing.is_some())
            .await
            .unwrap()
            .is_some());
        completion.begin();
        assert!(ready.borrow().is_none());
    }

    #[test]
    fn collection_delay_does_not_inflate_encode_duration() {
        let completion = EncodeCompletion::default();
        completion.begin();
        completion.finish();
        let timing = completion.timing().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            completion.timing().unwrap().encode_duration,
            timing.encode_duration
        );
        assert!(timing.completed_at.elapsed() >= Duration::from_millis(20));
    }
}
