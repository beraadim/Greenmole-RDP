//! Bound all incoming logical DVC messages before legacy reassembly.

use ironrdp_pdu::{pdu_other_err, PduResult};

use super::microphone_guard::{channel_header, field};

// A valid maximum CacheImportOffer contains its 10-byte header/count plus
// 5461 metadata entries of 12 bytes (MS-RDPEGFX 2.2.2.16). This is six bytes
// above 64KiB. Keep the fixed per-channel bounds while admitting that message;
// microphone preflight still applies its own smaller media-message limit.
const MAX_LOGICAL_DVC_MESSAGE: usize = 10 + 5461 * 12;
// Registration currently includes Display Control, Additional Input, GFX and
// microphone. Add another bounded slot here when introducing another channel.
const REGISTERED_CHANNEL_LIMIT: usize = 4;

#[derive(Clone, Copy, Debug, Default)]
struct Fragment {
    expected: usize,
    received: usize,
}

#[derive(Debug, Default)]
pub(super) struct LogicalDvcGuard {
    fragments: [Fragment; REGISTERED_CHANNEL_LIMIT],
}

impl LogicalDvcGuard {
    pub(super) fn check(&mut self, payload: &[u8]) -> PduResult<()> {
        let Some((command, channel_id, offset)) = channel_header(payload) else {
            // Upstream validates non-data commands and malformed headers.
            return Ok(());
        };
        let index = usize::try_from(channel_id)
            .map_err(|_| pdu_other_err!("Dynamic channel ID exceeds registered channel limit"))?;
        let fragment = self
            .fragments
            .get_mut(index)
            .ok_or_else(|| pdu_other_err!("Dynamic channel ID exceeds registered channel limit"))?;
        if command == 4 {
            *fragment = Fragment::default();
            return Ok(());
        }
        if command == 2 {
            let (declared, width) = field(&payload[offset..], (payload[0] >> 2) & 3)
                .ok_or_else(|| pdu_other_err!("Invalid dynamic channel fragment length"))?;
            let declared = usize::try_from(declared).map_err(|_| {
                pdu_other_err!("Dynamic channel fragment length exceeds receive limit")
            })?;
            let actual = payload.len() - offset - width;
            if declared == 0 || declared > MAX_LOGICAL_DVC_MESSAGE || actual > declared {
                return Err(pdu_other_err!(
                    "Dynamic channel logical message exceeds receive limit"
                ));
            }
            if fragment.expected != 0 {
                return Err(pdu_other_err!(
                    "Dynamic channel message interrupted by another first fragment"
                ));
            }
            if actual != declared {
                *fragment = Fragment {
                    expected: declared,
                    received: actual,
                };
            }
        } else {
            let actual = payload.len() - offset;
            if actual > MAX_LOGICAL_DVC_MESSAGE {
                return Err(pdu_other_err!(
                    "Dynamic channel logical message exceeds receive limit"
                ));
            }
            if fragment.expected != 0 {
                // Both operands are already bounded, so subtraction/addition
                // stays bounded even when the peer sends arbitrary fragments.
                if actual > fragment.expected - fragment.received {
                    return Err(pdu_other_err!(
                        "Dynamic channel fragments exceed declared logical length"
                    ));
                }
                fragment.received += actual;
                if fragment.received == fragment.expected {
                    *fragment = Fragment::default();
                }
            }
        }
        Ok(())
    }
}
