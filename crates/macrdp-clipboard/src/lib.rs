pub mod file;
mod file_urls;
pub mod formats;
pub mod html;
pub mod pasteboard;
mod remote_formats;
pub mod transfer;

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::*;
use ironrdp_core::{impl_as_any, IntoOwned};
use ironrdp_server::{CliprdrServerFactory, ServerEvent, ServerEventSender, ServerEventTx};

use crate::formats::{
    dib_to_png, png_to_dib, uti_to_rdp_format_id, FORMAT_ID_FILE_LIST, FORMAT_ID_HTML,
};
use crate::pasteboard::PasteboardBridge;
use crate::remote_formats::{preferred_format, FormatKind, RemoteFormat};
use crate::transfer::FileTransferManager;

/// macOS clipboard backend implementing the CLIPRDR protocol.
///
/// Bridges between NSPasteboard and RDP CLIPRDR channel with:
/// - A polling thread that checks `NSPasteboard.changeCount` every 500ms
/// - Anti-echo mechanism to prevent clipboard feedback loops
/// - Text clipboard support (CF_UNICODETEXT <-> NSPasteboard text)
pub struct MacClipboardBackend {
    event_sender: ServerEventTx,
    last_change_count: Arc<AtomicI64>,
    transfer_manager: FileTransferManager,
    temp_dir: PathBuf,
    poll_handle: Option<JoinHandle<()>>,
    stop_signal: Arc<AtomicBool>,
    locked: bool,
    pending_paste_format: Option<RemoteFormat>,
    // CLIPRDR responses carry no format ID. Serialize requests and coalesce newer
    // offers instead of decoding an older response with the newest format.
    queued_paste_format: Option<Option<RemoteFormat>>,
    file_transfer: bool,
}

impl fmt::Debug for MacClipboardBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacClipboardBackend")
            .field("temp_dir", &self.temp_dir)
            .finish_non_exhaustive()
    }
}

impl_as_any!(MacClipboardBackend);

impl MacClipboardBackend {
    pub fn new(
        event_sender: ServerEventTx,
        temp_dir: PathBuf,
        max_file_size: u64,
        file_transfer: bool,
    ) -> Self {
        std::fs::create_dir_all(&temp_dir).ok();
        Self {
            event_sender,
            last_change_count: Arc::new(AtomicI64::new(0)),
            transfer_manager: FileTransferManager::new(temp_dir.clone(), max_file_size),
            temp_dir,
            poll_handle: None,
            stop_signal: Arc::new(AtomicBool::new(false)),
            locked: false,
            pending_paste_format: None,
            queued_paste_format: None,
            file_transfer,
        }
    }

    fn request_remote_format(&mut self, format: Option<RemoteFormat>) {
        let Some(format) = format else {
            return;
        };
        if self
            .event_sender
            .send(ServerEvent::Clipboard(ClipboardMessage::SendInitiatePaste(
                format.id,
            )))
            .is_ok()
        {
            tracing::debug!(format_id = format.id.value(), kind = ?format.kind, "Requesting remote clipboard format");
            self.pending_paste_format = Some(format);
        }
    }

    /// Build a format list from the current pasteboard contents.
    fn build_format_list(file_transfer: bool) -> Vec<ClipboardFormat> {
        let pb = PasteboardBridge::new();
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

impl CliprdrBackend for MacClipboardBackend {
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

    fn on_ready(&mut self) {
        // Initialize change count and start polling thread
        let pb = PasteboardBridge::new();
        let initial = pb.change_count();
        self.last_change_count.store(initial, Ordering::SeqCst);

        let last = self.last_change_count.clone();
        let sender = self.event_sender.clone();
        let stop = self.stop_signal.clone();
        let file_transfer = self.file_transfer;

        self.poll_handle = Some(std::thread::spawn(move || {
            let bridge = PasteboardBridge::new();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if stop.load(Ordering::Relaxed) || sender.is_closed() {
                    break;
                }

                let current = bridge.change_count();
                let prev = last.swap(current, Ordering::SeqCst);
                if current != prev {
                    // Clipboard changed -- build format list and notify server
                    let formats: Vec<ClipboardFormat> = bridge
                        .available_types()
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
                        .collect();
                    if !formats.is_empty() {
                        if sender
                            .send(ServerEvent::Clipboard(ClipboardMessage::SendInitiateCopy(
                                formats,
                            )))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }));
        tracing::info!("Clipboard polling started");
    }

    fn on_process_negotiated_capabilities(&mut self, _caps: ClipboardGeneralCapabilityFlags) {}

    fn on_request_format_list(&mut self) {
        let formats = Self::build_format_list(self.file_transfer);
        let _ = self
            .event_sender
            .send(ServerEvent::Clipboard(ClipboardMessage::SendInitiateCopy(
                formats,
            )));
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        // A new offer supersedes incomplete file downloads as well as the old
        // format data. Outstanding file responses are ignored by their stream IDs.
        self.transfer_manager.set_incoming_descriptors(Vec::new());
        let format = preferred_format(available_formats, self.file_transfer);
        if self.pending_paste_format.is_some() {
            self.queued_paste_format = Some(format);
        } else {
            self.request_remote_format(format);
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let pb = PasteboardBridge::new();
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
            id if id == FORMAT_ID_HTML => match pb.read_html() {
                Some(html_content) => {
                    let data = crate::html::wrap_html_format(&html_content);
                    FormatDataResponse::new_data(data)
                }
                None => FormatDataResponse::new_error(),
            },
            id if id == FORMAT_ID_FILE_LIST && self.file_transfer => {
                let paths = pb.read_file_urls();
                self.transfer_manager.set_local_files(paths);
                let file_list = self.transfer_manager.build_file_list();
                match FormatDataResponse::new_file_list(&file_list) {
                    Ok(resp) => resp,
                    Err(e) => {
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
        let format = self.pending_paste_format.take();
        if let Some(next) = self.queued_paste_format.take() {
            // The response belongs to a superseded copy; never publish it or
            // reinterpret it as the format selected by the latest advertisement.
            self.request_remote_format(next);
            return;
        }
        if response.is_error() {
            tracing::warn!("Received error format data response from client");
            return;
        }

        match format.map(|format| format.kind) {
            Some(FormatKind::Text) => {
                if let Ok(text) = response.to_unicode_string() {
                    let pb = PasteboardBridge::new();
                    let new_count = pb.write_string(&text);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                }
            }
            Some(FormatKind::Dib) => match dib_to_png(response.data()) {
                Ok(png_data) => {
                    let pb = PasteboardBridge::new();
                    let new_count = pb.write_image(&png_data);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                }
                Err(error) => tracing::warn!(%error, "Rejected clipboard image"),
            },
            Some(FormatKind::Html) => {
                if let Some(html_content) = crate::html::unwrap_html_format(response.data()) {
                    let pb = PasteboardBridge::new();
                    let new_count = pb.write_html(&html_content);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                }
            }
            Some(FormatKind::Files) if self.file_transfer => {
                if !crate::file::valid_file_list_payload(response.data()) {
                    tracing::warn!("Rejected malformed or oversized clipboard file list");
                    return;
                }
                if let Ok(file_list) = response.to_file_list() {
                    self.transfer_manager
                        .set_incoming_descriptors(file_list.files);
                    let requests = self.transfer_manager.generate_contents_requests();
                    for req in requests {
                        let _ = self
                            .event_sender
                            .send(ServerEvent::ClipboardFileContentsRequest(req));
                    }
                }
            }
            _ => tracing::warn!("Ignored unsolicited or unsupported clipboard response"),
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
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
        if !self.file_transfer {
            return;
        }
        if let Some(request) = self.transfer_manager.handle_contents_response(&response) {
            let _ = self
                .event_sender
                .send(ServerEvent::ClipboardFileContentsRequest(request));
        }

        if self.transfer_manager.all_incoming_complete() {
            let paths = self.transfer_manager.flush_incoming_files();
            if !paths.is_empty() {
                let pb = PasteboardBridge::new();
                let new_count = pb.write_file_urls(&paths);
                if new_count >= 0 {
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                    tracing::info!(count = paths.len(), "Wrote incoming files to pasteboard");
                }
            }
        }
    }

    fn on_lock(&mut self, _data_id: LockDataId) {
        self.locked = true;
        self.transfer_manager.lock();
    }

    fn on_unlock(&mut self, _data_id: LockDataId) {
        self.locked = false;
        self.transfer_manager.unlock();
    }
}

impl Drop for MacClipboardBackend {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        if let Some(handle) = self.poll_handle.take() {
            let _ = handle.join();
        }
    }
}

// --- Factory ---

/// Factory for creating `MacClipboardBackend` instances.
///
/// Implements `CliprdrServerFactory` which combines `CliprdrBackendFactory`
/// and `ServerEventSender`. The event sender is injected by the server
/// before `build_cliprdr_backend` is called.
pub struct MacClipboardFactory {
    event_sender: Option<ServerEventTx>,
    temp_dir: PathBuf,
    max_file_size: u64,
    file_transfer: bool,
}

impl MacClipboardFactory {
    pub fn new(temp_dir: PathBuf, max_file_size_mb: u32, file_transfer: bool) -> Self {
        Self {
            event_sender: None,
            temp_dir,
            max_file_size: max_file_size_mb as u64 * 1024 * 1024,
            file_transfer,
        }
    }
}

impl CliprdrBackendFactory for MacClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(MacClipboardBackend::new(
            self.event_sender
                .clone()
                .expect("event sender not set before building clipboard backend"),
            self.temp_dir.clone(),
            self.max_file_size,
            self.file_transfer,
        ))
    }
}

impl ServerEventSender for MacClipboardFactory {
    fn set_sender(&mut self, sender: ServerEventTx) {
        self.event_sender = Some(sender);
    }
}

impl CliprdrServerFactory for MacClipboardFactory {}

#[cfg(test)]
mod tests;
