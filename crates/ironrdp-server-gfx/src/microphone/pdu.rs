//! Bounded PCM-only MS-RDPEAI messages. Parsers borrow media bytes.

use ironrdp_core::{ensure_size, Encode, EncodeResult, WriteCursor};
use ironrdp_dvc::{DvcEncode, DvcMessage};

use super::MicrophonePcmFormat;

// Enforced before DVC reassembly as well as before RDPEAI decoding. Normal
// 10 ms packets contain at most 1,920 bytes; format lists are much smaller.
pub(crate) const MAX_PDU_SIZE: usize = 16 * 1024;
const MAX_FORMATS: usize = 32;
const PCM_TAG: u16 = 1;

pub(super) enum ClientMessage<'a> {
    Version(u32),
    Formats(Vec<MicrophonePcmFormat>),
    OpenReply(i32),
    DataIncoming,
    Data(&'a [u8]),
    FormatChange(u32),
}

fn read_u16(bytes: &mut &[u8]) -> Option<u16> {
    let value = u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?);
    *bytes = &bytes[2..];
    Some(value)
}

fn read_u32(bytes: &mut &[u8]) -> Option<u32> {
    let value = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    *bytes = &bytes[4..];
    Some(value)
}

pub(super) fn parse(payload: &[u8]) -> Option<ClientMessage<'_>> {
    if payload.len() > MAX_PDU_SIZE {
        return None;
    }
    let (&message_id, mut bytes) = payload.split_first()?;
    match message_id {
        1 if bytes.len() == 4 => Some(ClientMessage::Version(read_u32(&mut bytes)?)),
        2 => {
            let count = usize::try_from(read_u32(&mut bytes)?).ok()?;
            // cbSizeFormatsPacket excludes optional trailing ExtraData. It is
            // not used as an allocation size or trusted to truncate the buffer.
            let _declared_size = read_u32(&mut bytes)?;
            if count > MAX_FORMATS || count > bytes.len() / 18 {
                return None;
            }
            let mut formats = Vec::with_capacity(count);
            for _ in 0..count {
                let tag = read_u16(&mut bytes)?;
                let channels = read_u16(&mut bytes)?;
                let sample_rate = read_u32(&mut bytes)?;
                let avg_bytes_per_second = read_u32(&mut bytes)?;
                let block_align = read_u16(&mut bytes)?;
                let bits_per_sample = read_u16(&mut bytes)?;
                let extra_size = usize::from(read_u16(&mut bytes)?);
                // The client is replying with a subset of our exact PCM
                // offers. Extended or compressed formats were never offered.
                if tag != PCM_TAG
                    || !matches!(channels, 1 | 2)
                    || !matches!(sample_rate, 44_100 | 48_000)
                    || bits_per_sample != 16
                    || extra_size != 0
                    || block_align != channels * 2
                    || avg_bytes_per_second != sample_rate * u32::from(block_align)
                {
                    return None;
                }
                formats.push(MicrophonePcmFormat {
                    sample_rate,
                    channels,
                });
            }
            // The specification allows arbitrary trailing ExtraData, which is
            // ignored. MAX_PDU_SIZE already bounds its memory footprint.
            Some(ClientMessage::Formats(formats))
        }
        4 if bytes.len() == 4 => Some(ClientMessage::OpenReply(i32::from_le_bytes(
            bytes.try_into().ok()?,
        ))),
        5 if bytes.is_empty() => Some(ClientMessage::DataIncoming),
        6 => Some(ClientMessage::Data(bytes)),
        7 if bytes.len() == 4 => Some(ClientMessage::FormatChange(read_u32(&mut bytes)?)),
        _ => None,
    }
}

struct RawPdu(Vec<u8>);

impl Encode for RawPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        ensure_size!(in: dst, size: self.0.len());
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MicrophoneServerPdu"
    }
    fn size(&self) -> usize {
        self.0.len()
    }
}

impl DvcEncode for RawPdu {}

fn append_format(bytes: &mut Vec<u8>, format: MicrophonePcmFormat) {
    bytes.extend_from_slice(&PCM_TAG.to_le_bytes());
    bytes.extend_from_slice(&format.channels.to_le_bytes());
    bytes.extend_from_slice(&format.sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(format.sample_rate * u32::from(format.channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(format.channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
}

pub(super) fn version() -> DvcMessage {
    // Version 1 fully supports PCM; version 2 only adds AAC behavior.
    Box::new(RawPdu(vec![1, 1, 0, 0, 0]))
}

pub(super) fn formats(formats: &[MicrophonePcmFormat]) -> DvcMessage {
    let mut bytes = vec![2];
    bytes.extend_from_slice(&u32::try_from(formats.len()).unwrap_or(0).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    for format in formats {
        append_format(&mut bytes, *format);
    }
    Box::new(RawPdu(bytes))
}

pub(super) fn open(format: MicrophonePcmFormat, frames: u32, index: u32) -> DvcMessage {
    let mut bytes = vec![3];
    bytes.extend_from_slice(&frames.to_le_bytes());
    bytes.extend_from_slice(&index.to_le_bytes());
    append_format(&mut bytes, format);
    Box::new(RawPdu(bytes))
}
