//! The DVC callback is synchronous, so it stages input for the connection's
//! ordered PDU dispatcher. A separate task could let subsequent keyboard input
//! overtake a mouse button or move received earlier on the same connection.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ironrdp_ainput::{ClientPdu, MousePdu, ServerPdu, VersionPdu};
use ironrdp_core::{decode, impl_as_any};
use ironrdp_dvc::{DvcMessage, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::{decode_err, PduResult};
use tokio::sync::watch;

const MAX_PENDING_INPUT: usize = 128;

#[derive(Clone, Default)]
pub(super) struct AdditionalInputQueue(Arc<Mutex<VecDeque<MousePdu>>>);

impl AdditionalInputQueue {
    fn push(&self, event: MousePdu) -> std::io::Result<()> {
        let mut pending = self.0.lock().unwrap();
        if pending.len() == MAX_PENDING_INPUT {
            return Err(std::io::Error::other("additional input queue is full"));
        }
        pending.push_back(event);
        Ok(())
    }

    pub(super) fn pop(&self) -> Option<MousePdu> {
        self.0.lock().unwrap().pop_front()
    }

    pub(super) fn clear(&self) {
        self.0.lock().unwrap().clear();
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
