//! Ordered owner of clipboard file I/O and image conversion. Called only by the
//! dedicated worker, never by the RDP connection loop.

use crate::{
    formats::{dib_to_png, png_to_dib, uti_to_rdp_format_id, FORMAT_ID_FILE_LIST, FORMAT_ID_HTML},
    pasteboard::PasteboardBridge,
    publication::{CopyToken, PublicationState, RemoteOffer},
    remote_formats::{preferred_format, text_fallback, FormatKind},
    transfer::FileTransferManager,
};
use ironrdp_cliprdr::{
    backend::{ClipboardMessage, CliprdrBackend},
    pdu::*,
};
use ironrdp_core::{impl_as_any, IntoOwned};
use ironrdp_server::{ServerEvent, ServerEventTx};
use std::{fmt, path::PathBuf, sync::Arc, time::Instant};

#[path = "rich_text.rs"]
mod rich_text;

pub(crate) struct ClipboardWorker {
    event_sender: ServerEventTx,
    shared: Arc<PublicationState>,
    pasteboard: PasteboardBridge,
    incoming_token: Option<CopyToken>,
    transfer_manager: FileTransferManager,
    temp_dir: PathBuf,
    outgoing_change_count: Option<i64>,
    pending_descriptor_lock: Option<u32>,
    pending_paste_format: Option<RemoteOffer>,
    // CLIPRDR responses carry no format ID. Serialize requests and coalesce newer
    // offers instead of decoding an older response with the newest format.
    queued_paste_format: Option<RemoteOffer>,
    pending_html: Option<rich_text::PendingHtml>,
    file_transfer: bool,
}

impl fmt::Debug for ClipboardWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardWorker")
            .field("temp_dir", &self.temp_dir)
            .finish_non_exhaustive()
    }
}

impl_as_any!(ClipboardWorker);

impl ClipboardWorker {
    pub fn new(
        event_sender: ServerEventTx,
        temp_dir: PathBuf,
        max_file_size: u64,
        file_transfer: bool,
        shared: Arc<PublicationState>,
        pasteboard: PasteboardBridge,
    ) -> Self {
        Self {
            event_sender,
            shared,
            pasteboard,
            incoming_token: None,
            transfer_manager: FileTransferManager::new(temp_dir.clone(), max_file_size),
            temp_dir,
            outgoing_change_count: None,
            pending_descriptor_lock: None,
            pending_paste_format: None,
            queued_paste_format: None,
            pending_html: None,
            file_transfer,
        }
    }

    fn request_remote_format(&mut self, offer: RemoteOffer) {
        let Some(format) = offer.format else {
            self.pending_html = None;
            return;
        };
        if !self.shared.is_current(offer.token, &self.pasteboard) {
            self.pending_html = None;
            return;
        }
        // Arm before sending so a response or deadline cannot race the request.
        self.shared.start_request(Instant::now());
        if self
            .event_sender
            .send(ServerEvent::Clipboard(ClipboardMessage::SendInitiatePaste(
                format.id,
            )))
            .is_ok()
        {
            tracing::debug!(format_id = format.id.value(), kind = ?format.kind,
                generation = offer.token.generation, "Requesting remote clipboard format");
            self.pending_paste_format = Some(offer);
        } else {
            self.shared.finish_request();
            self.pending_html = None;
        }
    }

    pub(crate) fn remote_copy(&mut self, offer: RemoteOffer) {
        self.pending_html = None;
        self.transfer_manager.set_incoming_descriptors(Vec::new());
        self.incoming_token = None;
        if self.pending_paste_format.is_some() {
            self.queued_paste_format = Some(offer);
        } else {
            self.request_remote_format(offer);
        }
    }

    pub(crate) fn tick(&mut self) {
        self.expire_pending_html();
        if self
            .incoming_token
            .is_some_and(|token| !self.shared.is_current(token, &self.pasteboard))
        {
            self.transfer_manager.set_incoming_descriptors(Vec::new());
            self.incoming_token = None;
        }
        self.transfer_manager.expire(Instant::now());
    }

    fn refresh_local_files(&mut self, pasteboard: &PasteboardBridge) {
        let change_count = pasteboard.change_count();
        if self.outgoing_change_count != Some(change_count) {
            let paths = pasteboard.read_file_urls();
            if self.transfer_manager.set_local_files(paths) {
                self.outgoing_change_count = Some(change_count);
            } else {
                // A later unlock can free snapshot capacity without a new copy.
                self.outgoing_change_count = None;
            }
        }
    }

    fn dispatch_file_requests(&mut self, requests: Vec<FileContentsRequest>) {
        for request in requests {
            if self
                .event_sender
                .send(ServerEvent::ClipboardFileContentsRequest(request))
                .is_err()
            {
                self.transfer_manager.set_incoming_descriptors(Vec::new());
                break;
            }
        }
        if let Some(prepared) = self.transfer_manager.prepare_incoming_files() {
            let published = self.incoming_token.take().is_some_and(|token| {
                self.shared.publish(token, &self.pasteboard, |pb| {
                    pb.write_file_urls(&prepared.paths)
                })
            });
            if published {
                self.transfer_manager.retain_incoming_files(prepared);
            }
            // On cancellation/failure the private directory drops here on the
            // worker, removing every unpublished file and preserving quota.
        }
    }

    /// Build a format list from the current pasteboard contents.
    pub(crate) fn build_format_list(
        pb: &PasteboardBridge,
        file_transfer: bool,
    ) -> Vec<ClipboardFormat> {
        pb.available_types()
            .iter()
            .filter_map(|uti| {
                let fmt_id = uti_to_rdp_format_id(uti)?;
                if fmt_id == FORMAT_ID_FILE_LIST && !file_transfer {
                    return None;
                }
                let mut fmt = ClipboardFormat::new(ClipboardFormatId::new(fmt_id));
                match fmt_id {
                    FORMAT_ID_HTML => {
                        fmt = fmt.with_name(ClipboardFormatName::HTML);
                    }
                    FORMAT_ID_FILE_LIST => {
                        fmt = fmt.with_name(ClipboardFormatName::FILE_LIST);
                    }
                    _ => {}
                }
                Some(fmt)
            })
            .collect()
    }
}

impl CliprdrBackend for ClipboardWorker {
    fn temporary_directory(&self) -> &str {
        self.temp_dir.to_str().unwrap_or("/tmp/macrdp-clipboard")
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        let mut flags = ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES;
        if self.file_transfer {
            flags |= ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
                | ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
                | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA;
        }
        flags
    }

    fn on_ready(&mut self) {}

    fn on_process_negotiated_capabilities(&mut self, _caps: ClipboardGeneralCapabilityFlags) {}

    fn on_request_format_list(&mut self) {
        let formats = Self::build_format_list(&self.pasteboard, self.file_transfer);
        let _ = self
            .event_sender
            .send(ServerEvent::Clipboard(ClipboardMessage::SendInitiateCopy(
                formats,
            )));
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        let token = self.shared.remote_copy(&self.pasteboard);
        let format = preferred_format(available_formats, self.file_transfer);
        self.remote_copy(RemoteOffer {
            format,
            text_fallback: text_fallback(available_formats, format),
            token,
        });
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let pb = self.pasteboard.clone();
        let format_id = request.format.value();

        let response = match format_id {
            13 => {
                // CF_UNICODETEXT -- use ironrdp's built-in unicode string helper
                match pb.read_string() {
                    Some(text) => FormatDataResponse::new_unicode_string(&text),
                    None => FormatDataResponse::new_error(),
                }
            }
            8 => {
                // CF_DIB — convert macOS image to Windows DIB
                match pb.read_image() {
                    Some(png_data) => match png_to_dib(&png_data) {
                        Ok(dib) => FormatDataResponse::new_data(dib),
                        Err(e) => {
                            tracing::warn!("Failed to convert image to DIB: {e}");
                            FormatDataResponse::new_error()
                        }
                    },
                    None => FormatDataResponse::new_error(),
                }
            }
            id if id == FORMAT_ID_HTML => pb
                .read_html()
                .and_then(|html| crate::html::wrap_html_format(&html))
                .map(FormatDataResponse::new_data)
                .unwrap_or_else(FormatDataResponse::new_error),
            id if id == FORMAT_ID_FILE_LIST && self.file_transfer => {
                let file_list = if let Some(id) = self.pending_descriptor_lock.take() {
                    self.transfer_manager.locked_file_list(id)
                } else {
                    self.refresh_local_files(&pb);
                    Some(self.transfer_manager.build_file_list())
                };
                match file_list.as_ref().map(FormatDataResponse::new_file_list) {
                    None => FormatDataResponse::new_error(),
                    Some(Ok(resp)) => resp,
                    Some(Err(e)) => {
                        tracing::warn!("Failed to encode file list: {e}");
                        FormatDataResponse::new_error()
                    }
                }
            }
            _ => {
                tracing::warn!(format_id, "Unsupported clipboard format requested");
                FormatDataResponse::new_error()
            }
        };

        // Convert to owned and send
        let owned: OwnedFormatDataResponse = response.into_owned();
        let _ = self
            .event_sender
            .send(ServerEvent::Clipboard(ClipboardMessage::SendFormatData(
                owned,
            )));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        let offer = self.pending_paste_format.take();
        self.shared.finish_request();
        if let Some(next) = self.queued_paste_format.take() {
            // The response belongs to a superseded copy; never publish it or
            // reinterpret it as the format selected by the latest advertisement.
            self.pending_html = None;
            self.request_remote_format(next);
            return;
        }
        let Some(offer) = offer else {
            tracing::warn!("Ignored unsolicited clipboard format response");
            return;
        };
        if !self.shared.is_current(offer.token, &self.pasteboard) {
            self.pending_html = None;
            return;
        }
        if matches!(
            offer.format.map(|format| format.kind),
            Some(FormatKind::Html | FormatKind::Text)
        ) {
            self.handle_rich_text_response(offer, response);
            return;
        }
        if response.is_error() {
            tracing::warn!("Received error format data response from client");
            return;
        }

        match offer.format.map(|format| format.kind) {
            Some(FormatKind::Dib) => match dib_to_png(response.data()) {
                Ok(png_data) => {
                    self.shared.publish(offer.token, &self.pasteboard, |pb| {
                        pb.write_image(&png_data)
                    });
                }
                Err(error) => tracing::warn!(%error, "Rejected clipboard image"),
            },
            Some(FormatKind::Files) if self.file_transfer => {
                if !crate::file::valid_file_list_payload(response.data()) {
                    tracing::warn!("Rejected malformed or oversized clipboard file list");
                    return;
                }
                if let Ok(file_list) = response.to_file_list() {
                    self.incoming_token = Some(offer.token);
                    let requests = {
                        let transfers = &mut self.transfer_manager;
                        transfers.set_incoming_descriptors(file_list.files);
                        transfers.generate_contents_requests()
                    };
                    self.dispatch_file_requests(requests);
                }
            }
            _ => tracing::warn!("Ignored unsolicited or unsupported clipboard response"),
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        if request.data_id.is_some() && self.pending_descriptor_lock == request.data_id {
            // A client may lock after receiving descriptors. Its first contents
            // request consumes the pending descriptor association too.
            self.pending_descriptor_lock = None;
        }
        let response = if self.file_transfer {
            self.transfer_manager.handle_contents_request(&request)
        } else {
            FileContentsResponse::new_error(request.stream_id)
        };
        let owned: OwnedFileContentsResponse = response.into_owned();
        let _ = self
            .event_sender
            .send(ServerEvent::ClipboardFileContents(owned));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        if !self.file_transfer
            || self
                .incoming_token
                .is_some_and(|token| !self.shared.is_current(token, &self.pasteboard))
        {
            self.transfer_manager.set_incoming_descriptors(Vec::new());
            self.incoming_token = None;
            return;
        }
        let requests = self.transfer_manager.handle_contents_response(&response);
        self.dispatch_file_requests(requests);
    }

    fn on_lock(&mut self, data_id: LockDataId) {
        if !self.file_transfer {
            return;
        }
        self.refresh_local_files(&self.pasteboard.clone());
        self.transfer_manager.lock(data_id.0);
        self.pending_descriptor_lock = Some(data_id.0);
    }

    fn on_unlock(&mut self, data_id: LockDataId) {
        self.transfer_manager.unlock(data_id.0);
        if self.pending_descriptor_lock == Some(data_id.0) {
            self.pending_descriptor_lock = None;
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
