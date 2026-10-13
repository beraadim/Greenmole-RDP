//! One rich copy may require two sequential, untagged CLIPRDR responses. Retain
//! at most one bounded HTML fragment and publish only once both flavors finish.

use super::ClipboardWorker;
use crate::{
    publication::{CopyToken, RemoteOffer},
    remote_formats::FormatKind,
    text::MAX_TEXT_PAYLOAD_BYTES,
};
use ironrdp_cliprdr::pdu::FormatDataResponse;

pub(super) struct PendingHtml {
    token: CopyToken,
    content: String,
}

impl ClipboardWorker {
    pub(super) fn expire_pending_html(&mut self) {
        if self
            .pending_html
            .as_ref()
            .is_some_and(|html| !self.shared.is_current(html.token, &self.pasteboard))
        {
            self.pending_html = None;
        }
    }

    pub(super) fn handle_rich_text_response(
        &mut self,
        offer: RemoteOffer,
        response: FormatDataResponse<'_>,
    ) {
        match offer.format.map(|format| format.kind) {
            Some(FormatKind::Html) => {
                let html = (!response.is_error())
                    .then(|| crate::html::unwrap_html_format(response.data()))
                    .flatten();
                if html.is_none() {
                    tracing::warn!(
                        text_fallback = offer.text_fallback.is_some(),
                        "Remote HTML unavailable, invalid or oversized"
                    );
                }
                if let Some(text_format) = offer.text_fallback {
                    self.pending_html = html.map(|content| PendingHtml {
                        token: offer.token,
                        content,
                    });
                    // This response has been consumed, so the next untagged
                    // response can safely be decoded as this same copy's text.
                    self.request_remote_format(RemoteOffer {
                        format: Some(text_format),
                        text_fallback: None,
                        token: offer.token,
                    });
                } else if let Some(html) = html {
                    self.publish(offer.token, |pb| pb.write_html(&html));
                }
            }
            Some(FormatKind::Text) => {
                // Bound the decoder too: UTF-16 can grow when converted to UTF-8.
                let text = (!response.is_error()
                    && response.data().len() <= MAX_TEXT_PAYLOAD_BYTES)
                    .then(|| response.to_unicode_string().ok())
                    .flatten()
                    .filter(|text| text.len() <= MAX_TEXT_PAYLOAD_BYTES);
                if text.is_none() {
                    tracing::warn!("Remote plain text unavailable, invalid or oversized");
                }
                let html = self
                    .pending_html
                    .take()
                    .filter(|html| html.token == offer.token);
                match (html, text) {
                    (Some(html), text) => {
                        self.publish(offer.token, |pb| {
                            pb.write_rich_text(&html.content, text.as_deref())
                        });
                    }
                    (None, Some(text)) => {
                        self.publish(offer.token, |pb| pb.write_string(&text));
                    }
                    (None, None) => {}
                }
            }
            _ => unreachable!("rich text handler called for a different format"),
        }
    }
}

#[cfg(test)]
#[path = "rich_text_tests.rs"]
mod tests;
