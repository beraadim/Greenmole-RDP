//! The DVC callback is synchronous, so it stages input for the connection's
//! ordered PDU dispatcher. A separate task could let subsequent keyboard input
//! overtake a mouse button or move received earlier on the same connection.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ironrdp_ainput::{ClientPdu, MousePdu, ServerPdu, VersionPdu};
use ironrdp_core::{decode, impl_as_any};
use ironrdp_dvc::{DvcMessage, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::{decode_err, PduResult};
use tokio::sync::watch;

const MAX_PENDING_INPUT: usize = 128;

#[derive(Clone, Default)]
pub(super) struct AdditionalInputQueue(Arc<Mutex<InputQueueState>>);

#[derive(Default)]
struct InputQueueState {
    pending: VecDeque<(MousePdu, Option<Instant>)>,
    pdu_received_at: Option<Instant>,
}

/// Restore the previous context even if protocol processing is cancelled.
pub(super) struct PduInputContext {
    queue: AdditionalInputQueue,
    previous: Option<Instant>,
}

impl Drop for PduInputContext {
    fn drop(&mut self) {
        self.queue.0.lock().unwrap().pdu_received_at = self.previous;
    }
}

impl AdditionalInputQueue {
    fn push(&self, event: MousePdu) -> std::io::Result<()> {
        let mut pending = self.0.lock().unwrap();
        if pending.pending.len() == MAX_PENDING_INPUT {
            return Err(std::io::Error::other("additional input queue is full"));
        }
        let at = pending.pdu_received_at;
        pending.pending.push_back((event, at));
        Ok(())
    }

    pub(super) fn pop_timed(&self) -> Option<(MousePdu, Option<Instant>)> {
        self.0.lock().unwrap().pending.pop_front()
    }

    #[cfg(test)]
    pub(super) fn pop(&self) -> Option<MousePdu> {
        self.pop_timed().map(|(event, _)| event)
    }

    pub(super) fn received_at(&self) -> Option<Instant> {
        self.0.lock().unwrap().pdu_received_at
    }

    pub(super) fn context(&self, received_at: Option<Instant>) -> PduInputContext {
        let previous = std::mem::replace(&mut self.0.lock().unwrap().pdu_received_at, received_at);
        PduInputContext {
            queue: self.clone(),
            previous,
        }
    }

    pub(super) fn clear(&self) {
        let mut state = self.0.lock().unwrap();
        state.pending.clear();
        state.pdu_received_at = None;
    }
}

pub(super) struct AInputHandler {
    pending: AdditionalInputQueue,
    activity: watch::Sender<tokio::time::Instant>,
}

impl AInputHandler {
    pub(super) fn new(
        pending: AdditionalInputQueue,
        activity: watch::Sender<tokio::time::Instant>,
    ) -> Self {
        Self { pending, activity }
    }
}

impl_as_any!(AInputHandler);

impl DvcProcessor for AInputHandler {
    fn channel_name(&self) -> &str {
        ironrdp_ainput::CHANNEL_NAME
    }

    fn start(&mut self, _channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        Ok(vec![Box::new(ServerPdu::Version(VersionPdu::default()))])
    }

    fn close(&mut self, _channel_id: u32) {}

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        match decode(payload).map_err(|error| decode_err!(error))? {
            ClientPdu::Mouse(event) => {
                self.pending
                    .push(event)
                    .map_err(|error| decode_err!(error))?;
                self.activity.send_replace(tokio::time::Instant::now());
            }
        }
        Ok(Vec::new())
    }
}

impl DvcServerProcessor for AInputHandler {}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_ainput::MouseEventFlags;

    #[test]
    fn staging_preserves_the_pdu_read_time_and_context_is_scoped() {
        let queue = AdditionalInputQueue::default();
        let at = Instant::now() - std::time::Duration::from_secs(1);
        let event = MousePdu {
            flags: MouseEventFlags::MOVE,
            x: 0,
            y: 0,
            time: 0,
        };
        {
            let _context = queue.context(Some(at));
            queue.push(event.clone()).unwrap();
        }
        queue.push(event).unwrap();
        assert_eq!(queue.pop_timed().unwrap().1, Some(at));
        assert_eq!(queue.pop_timed().unwrap().1, None);
        assert_eq!(queue.received_at(), None);
    }

    #[test]
    fn input_queue_is_bounded_and_clear_discards_old_connection_events() {
        let queue = AdditionalInputQueue::default();
        for x in 0..MAX_PENDING_INPUT {
            queue
                .push(MousePdu {
                    flags: MouseEventFlags::MOVE,
                    x: x as i32,
                    y: 0,
                    time: 0,
                })
                .unwrap();
        }
        let first = queue.pop().unwrap();
        assert_eq!(first.x, 0);
        queue.push(first).unwrap();
        let extra = queue.pop().unwrap();
        queue.push(extra.clone()).unwrap();
        assert!(queue.push(extra).is_err());
        queue.clear();
        assert!(queue.pop().is_none());
    }
}
