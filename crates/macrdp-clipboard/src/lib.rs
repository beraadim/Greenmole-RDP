pub mod file;
mod file_urls;
pub mod formats;
pub mod html;
pub mod pasteboard;
mod publication;
mod remote_formats;
mod runtime;
mod text;
pub mod transfer;
mod work_queue;
mod worker;

use ironrdp_cliprdr::{
    backend::{CliprdrBackend, CliprdrBackendFactory},
    pdu::*,
};
use ironrdp_core::{impl_as_any, IntoOwned};
use ironrdp_server::{CliprdrServerFactory, ServerEventSender, ServerEventTx};
use pasteboard::PasteboardBridge;
use publication::{PublicationState, RemoteOffer};
use remote_formats::{preferred_format, text_fallback};
use runtime::{ClipboardRuntime, Command};
use std::{fmt, path::PathBuf, sync::Arc};

/// Nonblocking CLIPRDR adapter. All file access and image conversion is ordered
/// on one bounded worker; callbacks only capture requests and copy admitted PDUs.
/// NSPasteboard change-count reads are used to stamp copy generations.
pub struct MacClipboardBackend {
    runtime: ClipboardRuntime,
    shared: Arc<PublicationState>,
    pasteboard: PasteboardBridge,
    temp_dir: PathBuf,
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
        let pasteboard = PasteboardBridge::new();
        let shared = Arc::new(PublicationState::new(pasteboard.change_count()));
        let runtime = ClipboardRuntime::new(
            event_sender,
            temp_dir.clone(),
            max_file_size,
            file_transfer,
            shared.clone(),
            pasteboard.clone(),
        );
        Self {
            runtime,
            shared,
            pasteboard,
            temp_dir,
            file_transfer,
        }
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
        self.runtime.start_monitor();
    }
    fn on_process_negotiated_capabilities(&mut self, _caps: ClipboardGeneralCapabilityFlags) {}
    fn on_request_format_list(&mut self) {
        self.runtime.enqueue(0, || Command::RequestFormatList);
    }
    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        let format = preferred_format(formats, self.file_transfer);
        let offer = RemoteOffer {
            token: self.shared.remote_copy(&self.pasteboard),
            format,
            text_fallback: text_fallback(formats, format),
        };
        self.runtime.enqueue(0, || Command::RemoteCopy(offer));
    }
    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.runtime.enqueue(0, || Command::FormatRequest(request));
    }
    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.runtime.enqueue(response.data().len(), || {
            Command::FormatResponse(response.into_owned())
        });
    }
    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        self.runtime.enqueue(0, || Command::FileRequest(request));
    }
    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        self.runtime.enqueue(response.data().len(), || {
            Command::FileResponse(response.into_owned())
        });
    }
    fn on_lock(&mut self, id: LockDataId) {
        self.runtime.enqueue(0, || Command::Lock(id));
    }
    fn on_unlock(&mut self, id: LockDataId) {
        self.runtime.enqueue(0, || Command::Unlock(id));
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
