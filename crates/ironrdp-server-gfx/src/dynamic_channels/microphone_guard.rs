//! Validate microphone fragment sizes before the legacy DVC reassembler.

use crate::microphone::{MicrophoneControl, MAX_PDU_SIZE};

// Header fields use U8/U16/U32 little-endian integers selected by two bits.
pub(super) fn field(bytes: &[u8], kind: u8) -> Option<(u32, usize)> {
    match kind {
        0 => Some((u32::from(*bytes.first()?), 1)),
        1 => Some((
            u32::from(u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?)),
            2,
        )),
        2 => Some((u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?), 4)),
        _ => None,
    }
}

/// The command, channel ID and offset immediately following that ID.
/// No media bytes are copied or interpreted here.
pub(super) fn channel_header(payload: &[u8]) -> Option<(u8, u32, usize)> {
    let header = *payload.first()?;
    let command = header >> 4;
    if !matches!(command, 2..=4) {
        return None;
    }
    let (channel_id, width) = field(&payload[1..], header & 3)?;
    Some((command, channel_id, 1 + width))
}

#[derive(Debug)]
pub(super) struct MicrophoneDvcGuard {
    pub(super) control: MicrophoneControl,
    remaining: usize,
}

impl MicrophoneDvcGuard {
    pub(super) fn new(control: MicrophoneControl) -> Self {
        Self {
            control,
            remaining: 0,
        }
    }

    pub(super) fn preflight(&mut self, payload: &[u8]) -> bool {
        let Some((command, channel_id, offset)) = channel_header(payload) else {
            return true;
        };
        if self.control.channel_id() != Some(channel_id) || command == 4 {
            return true;
        }
        if self.control.is_closed() {
            return false;
        }
        let valid = match command {
            2 => self.first_fragment(payload, offset),
            3 => self.data_fragment(payload.len() - offset),
            _ => true,
        };
        if !valid {
            tracing::warn!("Closed microphone channel after invalid or oversized DVC fragments");
            self.remaining = 0;
            self.control.quarantine();
        }
        valid
    }

    fn first_fragment(&mut self, payload: &[u8], offset: usize) -> bool {
        let Some((declared, length_width)) = field(&payload[offset..], (payload[0] >> 2) & 3)
        else {
            return false;
        };
        let Ok(declared) = usize::try_from(declared) else {
            return false;
        };
        let actual = payload.len() - offset - length_width;
        if self.remaining != 0 || declared == 0 || declared > MAX_PDU_SIZE || actual > declared {
            return false;
        }
        self.remaining = declared - actual;
        true
    }

    fn data_fragment(&mut self, actual: usize) -> bool {
        if actual > MAX_PDU_SIZE {
            return false;
        }
        if self.remaining != 0 {
            if actual > self.remaining {
                return false;
            }
            self.remaining -= actual;
        }
        true
    }
}
