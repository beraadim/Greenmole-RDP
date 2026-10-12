//! Bounded clipboard transfers. Incoming files are staged on disk; outgoing
//! snapshots retain open file identities until their protocol locks are released.

mod incoming;
mod outgoing;

use incoming::IncomingTransfers;
use ironrdp_cliprdr::pdu::*;
use outgoing::OutgoingSnapshots;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct FileTransferManager {
    incoming: TransferMonitor,
    outgoing: OutgoingSnapshots,
    // Drop staging files and handles before removing the session directory.
    _directory: Option<tempfile::TempDir>,
}

/// Only incoming state crosses threads. Outgoing source opens and metadata reads
/// cannot delay the independent timeout monitor.
#[derive(Clone)]
pub struct TransferMonitor(Arc<Mutex<IncomingTransfers>>);

impl TransferMonitor {
    pub fn expire(&self, now: Instant) {
        self.0.lock().unwrap().expire(now);
    }
    pub fn cancel(&self) {
        self.0.lock().unwrap().start(Vec::new(), Instant::now());
    }
}

impl FileTransferManager {
    pub fn new(temp_dir: PathBuf, max_file_size: u64) -> Self {
        let directory = std::fs::create_dir_all(&temp_dir).ok().and_then(|()| {
            tempfile::Builder::new()
                .prefix("session-")
                .tempdir_in(&temp_dir)
                .ok()
        });
        if directory.is_none() {
            tracing::warn!("Cannot create private clipboard directory; incoming files disabled");
        }
        Self {
            incoming: TransferMonitor(Arc::new(Mutex::new(IncomingTransfers::new(
                directory.as_ref().map(|dir| dir.path().to_owned()),
                max_file_size,
            )))),
            outgoing: OutgoingSnapshots::new(max_file_size),
            _directory: directory,
        }
    }

    pub fn set_local_files(&mut self, paths: Vec<PathBuf>) -> bool {
        self.outgoing.set_files(paths)
    }
    pub fn build_file_list(&mut self) -> PackedFileList {
        self.outgoing.file_list()
    }
    pub fn locked_file_list(&mut self, id: u32) -> Option<PackedFileList> {
        self.outgoing.locked_file_list(id)
    }
    pub fn handle_contents_request(
        &self,
        request: &FileContentsRequest,
    ) -> FileContentsResponse<'static> {
        self.outgoing.contents(request)
    }
    pub fn lock(&mut self, data_id: u32) {
        self.outgoing.lock(data_id);
    }
    pub fn unlock(&mut self, data_id: u32) {
        self.outgoing.unlock(data_id);
    }
    pub fn set_incoming_descriptors(&mut self, descriptors: Vec<FileDescriptor>) {
        self.incoming
            .0
            .lock()
            .unwrap()
            .start(descriptors, Instant::now());
    }
    pub fn generate_contents_requests(&mut self) -> Vec<FileContentsRequest> {
        self.incoming.0.lock().unwrap().requests(Instant::now())
    }
    pub fn handle_contents_response(
        &mut self,
        response: &FileContentsResponse<'_>,
    ) -> Vec<FileContentsRequest> {
        self.incoming
            .0
            .lock()
            .unwrap()
            .respond(response, Instant::now())
    }
    pub fn all_incoming_complete(&self) -> bool {
        self.incoming.0.lock().unwrap().complete()
    }
    pub fn flush_incoming_files(&mut self) -> Vec<PathBuf> {
        self.incoming.0.lock().unwrap().finish()
    }

    /// Serviced by the clipboard polling thread even when the peer sends nothing.
    pub fn monitor(&self) -> TransferMonitor {
        self.incoming.clone()
    }
}
