//! Finish persistent bitmap cache negotiation without importing client data.
//!
//! MS-RDPEGFX 3.2.5.16 requires a CacheImportReply for every valid offer.
//! Importing entries is optional: a zero-entry reply leaves all graphics on
//! this server's fresh surface/encoder path. The pinned PDU crate supplies the
//! reply encoder but not the offer decoder, so validate only its bounded wire
//! envelope here; never allocate, retain, or log cache keys or entry metadata.

use ironrdp_pdu::rdp::vc::dvc::gfx::{CacheImportReplyPdu, ServerPdu};
use ironrdp_pdu::{pdu_other_err, PduResult};

use super::{make_dvc_message, DvcMessage};

const OFFER_COMMAND: [u8; 2] = 0x0010u16.to_le_bytes();
const HEADER_SIZE: usize = 8;
const FIXED_SIZE: usize = HEADER_SIZE + 2;
const ENTRY_SIZE: usize = 12;
// MS-RDPEGFX 2.2.2.16 requires cacheEntriesCount < 5462.
const MAX_ENTRIES: u16 = 5461;
const MAX_OFFER_SIZE: usize = FIXED_SIZE + MAX_ENTRIES as usize * ENTRY_SIZE;

/// Recognize one complete offer and return its required zero-import reply.
/// Errors are fixed strings and contain no bytes supplied by the peer.
pub(super) fn decline_offer(data: &[u8]) -> PduResult<Option<DvcMessage>> {
    if data.get(..2) != Some(OFFER_COMMAND.as_slice()) {
        return Ok(None);
    }
    if data.len() < FIXED_SIZE || data.len() > MAX_OFFER_SIZE {
        return Err(pdu_other_err!("Invalid graphics cache offer length"));
    }
    // Length was checked before accessing the fixed header and count.
    let flags = u16::from_le_bytes([data[2], data[3]]);
    let declared = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let entries = u16::from_le_bytes([data[8], data[9]]);
    let expected = FIXED_SIZE + usize::from(entries) * ENTRY_SIZE;
    if flags != 0
        || entries > MAX_ENTRIES
        || declared as usize != expected
        || data.len() != expected
    {
        return Err(pdu_other_err!("Invalid graphics cache offer envelope"));
    }

    let response = make_dvc_message(&ServerPdu::CacheImportReply(CacheImportReplyPdu {
        cache_slots: Vec::new(),
    }))?;
    tracing::info!(target: "macrdp_transition",
        component = "graphics_cache", phase = "offer", state = "declined",
        offer_count = entries, reply_count = 0u16,
        "Persistent graphics cache offer declined with an empty import reply");
    Ok(Some(response))
}

#[cfg(test)]
#[path = "gfx_cache_import/tests.rs"]
mod tests;
