//! Bound the outer static channel before the legacy SVC reassembler appends it.

use anyhow::{ensure, Result};
use ironrdp_core::decode;
use ironrdp_pdu::rdp::vc::{ChannelControlFlags, ChannelPduHeader};

// Incoming DRDYNVC PDUs are normally no larger than 1,600 bytes. Leave room
// for existing clients that coalesce data while bounding the outer layer too.
const MAX_DVC_SVC_MESSAGE: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct DynamicChannelSvcGuard {
    expected: usize,
    received: usize,
}

impl DynamicChannelSvcGuard {
    pub(crate) fn check(&mut self, payload: &[u8]) -> Result<()> {
        ensure!(payload.len() >= 8, "Truncated dynamic channel SVC header");
        let header: ChannelPduHeader = decode(&payload[..8])?;
        let declared = usize::try_from(header.length)?;
        ensure!(
            declared > 0 && declared <= MAX_DVC_SVC_MESSAGE,
            "Dynamic channel SVC message exceeds receive limit"
        );
        ensure!(
            !header
                .flags
                .contains(ChannelControlFlags::PACKET_COMPRESSED),
            "Compressed dynamic channel SVC input is unsupported"
        );
        let actual = payload.len() - 8;
        ensure!(actual > 0, "Empty dynamic channel SVC fragment");
        let first = header.flags.contains(ChannelControlFlags::FLAG_FIRST);
        let last = header.flags.contains(ChannelControlFlags::FLAG_LAST);
        if first {
            ensure!(
                self.expected == 0,
                "Dynamic channel SVC message interrupted by another first fragment"
            );
            self.expected = declared;
            self.received = 0;
        } else {
            ensure!(
                self.expected != 0,
                "Dynamic channel SVC continuation has no first fragment"
            );
        }
        ensure!(
            declared == self.expected,
            "Inconsistent dynamic channel SVC length"
        );
        let received = self
            .received
            .checked_add(actual)
            .ok_or_else(|| anyhow::anyhow!("Dynamic channel SVC length overflow"))?;
        ensure!(
            received <= self.expected,
            "Dynamic channel SVC fragments exceed declared length"
        );
        if last {
            ensure!(
                received == self.expected,
                "Truncated dynamic channel SVC final fragment"
            );
            *self = Self::default();
        } else {
            ensure!(
                received < self.expected,
                "Dynamic channel SVC completed without a last fragment"
            );
            self.received = received;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DynamicChannelSvcGuard, MAX_DVC_SVC_MESSAGE};

    fn chunk(declared: u32, flags: u32, data: &[u8]) -> Vec<u8> {
        let mut bytes = declared.to_le_bytes().to_vec();
        bytes.extend_from_slice(&flags.to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn complete_and_fragmented_messages_reset_receive_budget() {
        let mut guard = DynamicChannelSvcGuard::default();
        guard.check(&chunk(4, 3, &[1; 4])).unwrap();
        guard.check(&chunk(5, 1, &[1; 2])).unwrap();
        guard.check(&chunk(5, 0, &[2])).unwrap();
        guard.check(&chunk(5, 2, &[3; 2])).unwrap();
        guard.check(&chunk(4, 3, &[4; 4])).unwrap();
    }

    #[test]
    fn declarations_and_fragment_counts_cannot_bypass_receive_budget() {
        for bytes in [
            vec![],
            vec![0; 7],
            chunk(u32::MAX, 1, &[1]),
            chunk(u32::try_from(MAX_DVC_SVC_MESSAGE + 1).unwrap(), 1, &[1]),
            chunk(1, 0, &[1]),
            chunk(2, 3, &[1]),
            chunk(1, 3, &[1; 2]),
            chunk(1, 1, &[1]),
            chunk(0, 3, &[]),
            chunk(1, 3 | 0x0020_0000, &[1]),
        ] {
            assert!(DynamicChannelSvcGuard::default().check(&bytes).is_err());
        }
        for continuation in [
            chunk(9, 2, &[0; 9]),
            chunk(10, 1, &[0]),
            chunk(10, 2, &[0; 10]),
            chunk(10, 2, &[0; 8]),
        ] {
            let mut guard = DynamicChannelSvcGuard::default();
            guard.check(&chunk(10, 1, &[0])).unwrap();
            assert!(guard.check(&continuation).is_err());
        }
    }
}
