//! Prepare bounded client GFX messages before changing any protocol state.
//!
//! Transport buffers and graphics PDUs have independent boundaries. Inspect
//! each command before deciding whether its body is relevant (MS-RDPEGFX
//! 3.1.5.1), then decode supported messages from their own declared slices.
//! Optional/unknown messages never expose their bodies to a decoder or log.

use ironrdp_pdu::rdp::vc::dvc::gfx::ClientPdu;
use ironrdp_pdu::{decode, pdu_other_err, PduResult};

use super::cache_import;

const HEADER_SIZE: usize = 8;
const MAX_MESSAGE_BYTES: usize = 65_542;
// Every complete PDU has at least an eight-byte header. This also bounds the
// number of prepared messages and responses even when the bodies are empty.
const MAX_COMMANDS: usize = MAX_MESSAGE_BYTES / HEADER_SIZE;
const IGNORED_LOG_LIMIT: u8 = 4;

pub(super) enum Prepared {
    Client(ClientPdu),
    CacheOffer(u16),
}

#[derive(Default)]
pub(super) struct Observations {
    ignored: u8,
}

#[derive(Clone, Copy)]
struct Envelope {
    cmd_id: i64,
    header_flags: i64,
    declared_bytes: i64,
    available_bytes: usize,
    stream_offset: usize,
}

impl Envelope {
    fn at(data: &[u8], stream_offset: usize) -> Self {
        Self {
            cmd_id: data.get(..2).map_or(-1, |bytes| {
                i64::from(u16::from_le_bytes([bytes[0], bytes[1]]))
            }),
            header_flags: data.get(2..4).map_or(-1, |bytes| {
                i64::from(u16::from_le_bytes([bytes[0], bytes[1]]))
            }),
            declared_bytes: data.get(4..8).map_or(-1, |bytes| {
                i64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }),
            available_bytes: data.len(),
            stream_offset,
        }
    }

    fn expected(self) -> bool {
        matches!(self.cmd_id, 0x0d | 0x10 | 0x12)
    }

    fn length(self) -> Option<usize> {
        usize::try_from(self.declared_bytes)
            .ok()
            .filter(|length| *length >= HEADER_SIZE && *length <= self.available_bytes)
    }

    fn log(self, state: &'static str) {
        tracing::info!(target: "macrdp_transition",
            component = "graphics_receive", phase = "envelope", state,
            worker_pid = std::process::id(),
            cmd_id = self.cmd_id, header_flags = self.header_flags,
            declared_bytes = self.declared_bytes,
            available_bytes = self.available_bytes, stream_offset = self.stream_offset,
            "Graphics receive envelope observation");
    }

    fn reject(self) -> ironrdp_pdu::PduError {
        self.log("rejected");
        pdu_other_err!("Invalid supported graphics client message")
    }
}

impl Observations {
    fn ignore(&mut self, envelope: Envelope) {
        if self.ignored < IGNORED_LOG_LIMIT {
            self.ignored += 1;
            envelope.log("ignored");
        }
    }
}

pub(super) fn prepare(data: &[u8], observations: &mut Observations) -> PduResult<Vec<Prepared>> {
    if data.len() > MAX_MESSAGE_BYTES || data.len() < 2 {
        return Err(Envelope::at(data, 0).reject());
    }
    let mut messages = Vec::new();
    let mut offset = 0;
    let mut commands = 0;
    let mut previous_was_optional = false;
    while offset < data.len() {
        let remaining = &data[offset..];
        let envelope = Envelope::at(remaining, offset);
        commands += 1;
        if envelope.cmd_id < 0 && previous_was_optional {
            // An optional message can report a length that leaves a partial
            // tail. It has no supported body semantics, so do not let that
            // unidentifiable tail disconnect an otherwise working session.
            observations.ignore(envelope);
            break;
        }
        if commands > MAX_COMMANDS || envelope.cmd_id < 0 {
            return Err(envelope.reject());
        }
        if !envelope.expected() {
            // Optional messages such as QoE are outside the supported subset.
            // If their length is unusable, ignore this bounded logical tail;
            // guessing a boundary could reinterpret body bytes as commands.
            observations.ignore(envelope);
            previous_was_optional = true;
            match envelope.length() {
                Some(length) => offset += length,
                None => break,
            }
            continue;
        }
        let length = envelope.length().ok_or_else(|| envelope.reject())?;
        let pdu = &remaining[..length];
        // Header flags have no server-side semantics. Like the pinned decoder,
        // ignore them rather than rejecting otherwise usable client traffic.
        let message = if envelope.cmd_id == 0x10 {
            let entries = cache_import::validate_offer(pdu)
                .map_err(|_| envelope.reject())?
                .ok_or_else(|| envelope.reject())?;
            Prepared::CacheOffer(entries)
        } else {
            Prepared::Client(decode(pdu).map_err(|_| envelope.reject())?)
        };
        messages.push(message);
        previous_was_optional = false;
        offset += length;
    }
    Ok(messages)
}

#[cfg(test)]
#[path = "gfx_client_receive/tests.rs"]
mod tests;
