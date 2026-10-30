//! Bounded input retained across a resize before protocol processing begins.

use std::collections::VecDeque;
use std::time::Instant;

use anyhow::{bail, Result};
use bytes::BytesMut;
use ironrdp_pdu::Action;

const MAX_PDUS: usize = 128;
const MAX_BYTES: usize = 1024 * 1024;
// TPKT's 16-bit length is the largest supported outer PDU length. Reserve room
// for a complete read before polling the socket, then apply TCP backpressure.
const MAX_WIRE_PDU_BYTES: usize = u16::MAX as usize;

#[derive(Debug, Default)]
pub(super) struct PendingInput {
    pdus: VecDeque<(Action, BytesMut, Option<Instant>)>,
    bytes: usize,
}

impl PendingInput {
    pub(super) fn can_read(&self) -> bool {
        self.pdus.len() < MAX_PDUS && self.bytes <= MAX_BYTES - MAX_WIRE_PDU_BYTES
    }

    pub(super) fn len(&self) -> usize {
        self.pdus.len()
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn push(&mut self, action: Action, bytes: BytesMut) -> Result<()> {
        self.push_at(action, bytes, Some(Instant::now()))
    }

    pub(super) fn push_at(
        &mut self,
        action: Action,
        bytes: BytesMut,
        received_at: Option<Instant>,
    ) -> Result<()> {
        if self.pdus.len() >= MAX_PDUS || bytes.len() > MAX_BYTES - self.bytes {
            bail!("too much input waiting for resize activation");
        }
        self.bytes += bytes.len();
        self.pdus.push_back((action, bytes, received_at));
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pdus.is_empty()
    }

    pub(super) fn pop_timed(&mut self) -> Option<(Action, BytesMut, Option<Instant>)> {
        let item = self.pdus.pop_front()?;
        self.bytes -= item.1.len();
        Some(item)
    }

    #[cfg(test)]
    pub(super) fn pop(&mut self) -> Option<(Action, BytesMut)> {
        self.pop_timed().map(|(action, bytes, _)| (action, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_queue_preserves_original_read_time_and_unknown_activation_time() {
        let mut queue = PendingInput::default();
        let at = Instant::now() - std::time::Duration::from_secs(2);
        queue
            .push_at(Action::FastPath, (&[1][..]).into(), Some(at))
            .unwrap();
        queue
            .push_at(Action::X224, (&[2][..]).into(), None)
            .unwrap();
        assert_eq!(queue.pop_timed().unwrap().2, Some(at));
        assert_eq!(queue.pop_timed().unwrap().2, None);
    }

    #[test]
    fn byte_and_count_limits_remain_bounded_across_resize_replay() {
        let mut queue = PendingInput::default();
        queue
            .push(Action::FastPath, vec![0; MAX_BYTES].as_slice().into())
            .unwrap();
        assert!(queue.push(Action::FastPath, (&[1][..]).into()).is_err());
        queue.pop().unwrap();
        for _ in 0..MAX_PDUS {
            queue.push(Action::FastPath, (&[1][..]).into()).unwrap();
        }
        assert!(queue.push(Action::FastPath, (&[1][..]).into()).is_err());
        queue.pop().unwrap();
        queue.push(Action::FastPath, (&[2][..]).into()).unwrap();
    }

    #[test]
    fn socket_reads_reserve_room_for_a_complete_wire_packet() {
        let mut queue = PendingInput::default();
        assert!(queue.can_read());
        queue
            .push(
                Action::X224,
                vec![0; MAX_BYTES - MAX_WIRE_PDU_BYTES].as_slice().into(),
            )
            .unwrap();
        assert!(queue.can_read());
        queue
            .push(Action::X224, vec![0; MAX_WIRE_PDU_BYTES].as_slice().into())
            .unwrap();
        assert!(!queue.can_read());
        assert_eq!(queue.bytes(), MAX_BYTES);
        queue.pop().unwrap();
        assert!(queue.can_read());
    }
}
