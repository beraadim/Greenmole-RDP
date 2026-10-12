//! Keep the freshest image without losing damage from overwritten captures.

use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

use crate::{CaptureEvent, CapturedFrame};

const MAX_PENDING_DAMAGE_RECTS: usize = 128;

#[derive(Clone, Copy, Debug, Default)]
pub struct CaptureDeliveryStats {
    pub queue_ms: f64,
    pub coalesced_frames: u64,
}

/// Damage is relative to the previous capture, not the previous delivery.
pub(crate) fn accumulate_damage(latest: &mut CapturedFrame, previous: &CapturedFrame) {
    if latest.width != previous.width
        || latest.height != previous.height
        || latest.dirty_rects.is_empty()
        || previous.dirty_rects.is_empty()
        || latest.dirty_rects.len() + previous.dirty_rects.len() > MAX_PENDING_DAMAGE_RECTS
    {
        // Empty damage means a complete refresh. Bound metadata during stalls.
        latest.dirty_rects.clear();
    } else {
        latest
            .dirty_rects
            .extend(previous.dirty_rects.iter().cloned());
    }
}

#[derive(Default)]
struct Pending {
    frame: Option<CapturedFrame>,
    status: Option<CaptureEvent>,
    captured_at: Option<Instant>,
    coalesced: u64,
    closed: bool,
    last_delivery: CaptureDeliveryStats,
}

#[derive(Default)]
pub(crate) struct FrameMailbox {
    pending: Mutex<Pending>,
    ready: Notify,
}

pub(crate) struct FrameSender(pub Arc<FrameMailbox>);

impl FrameSender {
    pub fn send(&self, event: CaptureEvent) {
        let mut pending = self.0.pending.lock().unwrap();
        match event {
            CaptureEvent::Frame(mut frame) => {
                if let Some(previous) = pending.frame.take() {
                    accumulate_damage(&mut frame, &previous);
                    pending.coalesced = pending.coalesced.saturating_add(1);
                }
                pending.frame = Some(frame);
                pending.captured_at = Some(Instant::now());
                pending.status = None;
            }
            status => pending.status = Some(status),
        }
        drop(pending);
        self.0.ready.notify_one();
    }
}

impl Drop for FrameSender {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().closed = true;
        self.0.ready.notify_one();
    }
}

impl Pending {
    fn take_frame(&mut self) -> Option<CapturedFrame> {
        let pending = self;
        if let Some(frame) = pending.frame.take() {
            pending.last_delivery = CaptureDeliveryStats {
                queue_ms: pending
                    .captured_at
                    .take()
                    .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or_default(),
                coalesced_frames: pending.coalesced,
            };
            tracing::trace!(
                capture_queue_ms = pending.last_delivery.queue_ms,
                coalesced_frames = pending.coalesced,
                "Delivered latest captured frame"
            );
            pending.coalesced = 0;
            Some(frame)
        } else {
            None
        }
    }
}

impl FrameMailbox {
    pub fn try_recv_frame(&self) -> Option<CapturedFrame> {
        self.pending.lock().unwrap().take_frame()
    }

    pub fn try_recv(&self) -> Option<CaptureEvent> {
        let mut pending = self.pending.lock().unwrap();
        pending
            .take_frame()
            .map(CaptureEvent::Frame)
            .or_else(|| pending.status.take())
    }

    pub fn last_delivery_stats(&self) -> CaptureDeliveryStats {
        self.pending.lock().unwrap().last_delivery
    }

    pub async fn recv(&self) -> Option<CaptureEvent> {
        loop {
            // notify_one retains a permit across the check/await race. There is
            // only one receiver, and cancellation never consumes pending data.
            let ready = self.ready.notified();
            if let Some(event) = self.try_recv() {
                return Some(event);
            }
            {
                let pending = self.pending.lock().unwrap();
                if pending.closed && pending.frame.is_none() && pending.status.is_none() {
                    return None;
                }
            }
            ready.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FrameData, Rect};
    use bytes::Bytes;

    fn frame(timestamp: u64, x: u32) -> CapturedFrame {
        CapturedFrame {
            width: 100,
            height: 100,
            stride: 400,
            data: FrameData::Raw(Bytes::new()),
            timestamp_us: timestamp,
            dirty_rects: vec![Rect {
                x,
                y: 0,
                width: 1,
                height: 1,
            }],
            content_rect: None,
        }
    }

    #[test]
    fn pixel_refresh_preserves_capture_status_and_delivery_accounting() {
        let mailbox = Arc::new(FrameMailbox::default());
        let sender = FrameSender(mailbox.clone());
        sender.send(CaptureEvent::Frame(frame(1, 1)));
        sender.send(CaptureEvent::Frame(frame(2, 2)));
        sender.send(CaptureEvent::Unavailable);
        let latest = mailbox.try_recv_frame().unwrap();
        assert_eq!(latest.timestamp_us, 2);
        assert_eq!(latest.dirty_rects.len(), 2);
        assert_eq!(mailbox.last_delivery_stats().coalesced_frames, 1);
        assert!(mailbox.try_recv_frame().is_none());
        assert!(matches!(
            mailbox.try_recv(),
            Some(CaptureEvent::Unavailable)
        ));
    }

    #[tokio::test]
    async fn stalled_consumer_gets_latest_image_and_all_damage_before_idle() {
        let mailbox = Arc::new(FrameMailbox::default());
        let sender = FrameSender(mailbox.clone());
        for i in 0..10 {
            sender.send(CaptureEvent::Frame(frame(i, i as u32)));
        }
        sender.send(CaptureEvent::Idle);
        let Some(CaptureEvent::Frame(latest)) = mailbox.recv().await else {
            panic!("missing frame")
        };
        assert_eq!(latest.timestamp_us, 9);
        assert_eq!(latest.dirty_rects.len(), 10);
        let mut changed_columns: Vec<_> = latest.dirty_rects.iter().map(|rect| rect.x).collect();
        changed_columns.sort_unstable();
        assert_eq!(changed_columns, (0..10).collect::<Vec<_>>());
        assert_eq!(mailbox.last_delivery_stats().coalesced_frames, 9);
        assert!(matches!(mailbox.recv().await, Some(CaptureEvent::Idle)));
        drop(sender);
        assert!(mailbox.recv().await.is_none());
    }

    #[test]
    fn unknown_damage_and_long_stalls_require_full_refresh() {
        let mut latest = frame(2, 2);
        let mut previous = frame(1, 1);
        previous.dirty_rects.clear();
        accumulate_damage(&mut latest, &previous);
        assert!(latest.dirty_rects.is_empty());
        let mut latest = frame(2, 2);
        let mut previous = frame(1, 1);
        previous
            .dirty_rects
            .resize(MAX_PENDING_DAMAGE_RECTS, previous.dirty_rects[0].clone());
        accumulate_damage(&mut latest, &previous);
        assert!(latest.dirty_rects.is_empty());
    }

    #[test]
    fn resize_invalidates_old_damage_coordinates() {
        let mut latest = frame(2, 2);
        let previous = frame(1, 1);
        latest.width = 200;
        accumulate_damage(&mut latest, &previous);
        assert!(latest.dirty_rects.is_empty());
    }

    #[tokio::test]
    async fn cancelled_receive_does_not_lose_next_capture() {
        let mailbox = Arc::new(FrameMailbox::default());
        let sender = FrameSender(mailbox.clone());
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, mailbox.recv())
                .await
                .is_err()
        );
        sender.send(CaptureEvent::Frame(frame(42, 0)));
        let Some(CaptureEvent::Frame(latest)) = mailbox.recv().await else {
            panic!("missing frame")
        };
        assert_eq!(latest.timestamp_us, 42);
    }
}
