pub mod file;
pub mod formats;
pub mod html;
pub mod pasteboard;
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
use ironrdp_server::{CliprdrServerFactory, ServerEvent, ServerEventSender};
use tokio::sync::mpsc;

use crate::formats::{
    dib_to_png, png_to_dib, uti_to_rdp_format_id, FORMAT_ID_FILE_LIST, FORMAT_ID_HTML,
};
use crate::pasteboard::PasteboardBridge;
use crate::transfer::FileTransferManager;

/// macOS clipboard backend implementing the CLIPRDR protocol.
///
/// Bridges between NSPasteboard and RDP CLIPRDR channel with:
/// - A polling thread that checks `NSPasteboard.changeCount` every 500ms
/// - Anti-echo mechanism to prevent clipboard feedback loops
/// - Text clipboard support (CF_UNICODETEXT <-> NSPasteboard text)
pub struct MacClipboardBackend {
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    last_change_count: Arc<AtomicI64>,
    remote_formats: Vec<ClipboardFormat>,
    transfer_manager: FileTransferManager,
    temp_dir: PathBuf,
    poll_handle: Option<JoinHandle<()>>,
    stop_signal: Arc<AtomicBool>,
    locked: bool,
    pending_paste_format: Option<u32>,
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
        event_sender: mpsc::UnboundedSender<ServerEvent>,
        temp_dir: PathBuf,
        max_file_size: u64,
        file_transfer: bool,
    ) -> Self {
        std::fs::create_dir_all(&temp_dir).ok();
        Self {
            event_sender,
            last_change_count: Arc::new(AtomicI64::new(0)),
            remote_formats: Vec::new(),
            transfer_manager: FileTransferManager::new(temp_dir.clone(), max_file_size),
            temp_dir,
            poll_handle: None,
            stop_signal: Arc::new(AtomicBool::new(false)),
            locked: false,
            pending_paste_format: None,
            file_transfer,
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
                if stop.load(Ordering::Relaxed) {
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
                        let _ = sender.send(ServerEvent::Clipboard(
                            ClipboardMessage::SendInitiateCopy(formats),
                        ));
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
        self.remote_formats = available_formats.to_vec();

        // Request the highest-fidelity format available, in order of preference
        let preferred = [FORMAT_ID_FILE_LIST, FORMAT_ID_HTML, 8, 13];
        for &fmt_id in &preferred {
            if fmt_id == FORMAT_ID_FILE_LIST && !self.file_transfer {
                continue;
            }
            if available_formats.iter().any(|f| f.id().value() == fmt_id) {
                self.pending_paste_format = Some(fmt_id);
                let _ = self.event_sender.send(ServerEvent::Clipboard(
                    ClipboardMessage::SendInitiatePaste(ClipboardFormatId::new(fmt_id)),
                ));
                return;
            }
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
        if response.is_error() {
            tracing::warn!("Received error format data response from client");
            return;
        }

        let format = self.pending_paste_format.take();
        match format {
            Some(13) => {
                if let Ok(text) = response.to_unicode_string() {
                    let pb = PasteboardBridge::new();
                    let new_count = pb.write_string(&text);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                }
            }
            Some(8) => {
                let data = response.data();
                if data.len() >= formats::BITMAPINFOHEADER_SIZE {
                    if let Ok(png_data) = dib_to_png(data) {
                        let pb = PasteboardBridge::new();
                        let new_count = pb.write_image(&png_data);
                        self.last_change_count.store(new_count, Ordering::SeqCst);
                    }
                }
            }
            Some(id) if id == FORMAT_ID_HTML => {
                if let Some(html_content) = crate::html::unwrap_html_format(response.data()) {
                    let pb = PasteboardBridge::new();
                    let new_count = pb.write_html(&html_content);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                }
            }
            Some(id) if id == FORMAT_ID_FILE_LIST && self.file_transfer => {
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
            _ => {
                // Fallback: content-based detection for backwards compatibility
                if let Ok(text) = response.to_unicode_string() {
                    let pb = PasteboardBridge::new();
                    let new_count = pb.write_string(&text);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                    return;
                }
                let data = response.data();
                if data.len() >= formats::BITMAPINFOHEADER_SIZE {
                    let header_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                    if header_size == 40 {
                        if let Ok(png_data) = dib_to_png(data) {
                            let pb = PasteboardBridge::new();
                            let new_count = pb.write_image(&png_data);
                            self.last_change_count.store(new_count, Ordering::SeqCst);
                        }
                    }
                }
            }
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
                self.last_change_count.store(new_count, Ordering::SeqCst);
                tracing::info!(count = paths.len(), "Wrote incoming files to pasteboard");
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
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
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
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}

impl CliprdrServerFactory for MacClipboardFactory {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_file_response_schedules_bounded_remainder_on_clipboard_channel() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, true);
        backend
            .transfer_manager
            .set_incoming_descriptors(vec![FileDescriptor {
                name: "test.txt".to_string(),
                file_size: Some(5),
                attributes: None,
                last_write_time: None,
            }]);
        let requests = backend.transfer_manager.generate_contents_requests();
        backend.on_file_contents_response(FileContentsResponse::new_data_response(
            requests[1].stream_id,
            b"he".to_vec(),
        ));
        let ServerEvent::ClipboardFileContentsRequest(remainder) = receiver.try_recv().unwrap()
        else {
            panic!("Expected missing-range request");
        };
        assert_eq!(remainder.position, 2);
        assert_eq!(remainder.requested_size, 3);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn disabling_file_transfer_disables_capability_and_ignores_remote_file_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, false);
        assert!(!backend
            .client_capabilities()
            .contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED));
        backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::new(
            FORMAT_ID_FILE_LIST,
        ))]);
        assert!(receiver.try_recv().is_err());
        assert!(backend.pending_paste_format.is_none());
        backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::new(13))]);
        assert_eq!(backend.pending_paste_format, Some(13));
        assert!(receiver.try_recv().is_ok());
    }
}
