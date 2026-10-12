use crate::file::{FILE_TRANSFER_CHUNK_SIZE, MAX_CLIPBOARD_FILES};
use ironrdp_cliprdr::pdu::*;
use std::collections::HashMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

const MAX_LOCKS: usize = 16;
// Leave room under macOS's common 256-descriptor soft limit for sockets,
// capture/encoder resources, diagnostics, and the incoming request window.
const MAX_PINNED_FILES: usize = 128;

struct SourceFile {
    descriptor: FileDescriptor,
    file: File,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl SourceFile {
    fn unchanged(&self, metadata: &Metadata) -> bool {
        #[cfg(unix)]
        if metadata_identity(metadata) != self.identity {
            return false;
        }
        metadata.is_file()
            && Some(metadata.len()) == self.descriptor.file_size
            && metadata.modified().ok() == Some(self.modified)
    }
}

#[cfg(unix)]
fn metadata_identity(metadata: &Metadata) -> (u64, u64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    // ctime detects same-length edits even if the original mtime is restored.
    // Metadata changes (including unlink) conservatively invalidate the transfer;
    // a newer clipboard copy alone never invalidates its retained source handles.
    (
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}

#[derive(Default)]
struct Snapshot {
    files: Vec<SourceFile>,
}

pub(super) struct OutgoingSnapshots {
    current: Arc<Snapshot>,
    advertised: Arc<Snapshot>,
    locks: HashMap<u32, Arc<Snapshot>>,
    max_file_size: u64,
}

impl OutgoingSnapshots {
    pub(super) fn new(max_file_size: u64) -> Self {
        Self {
            current: Arc::default(),
            advertised: Arc::default(),
            locks: HashMap::new(),
            max_file_size,
        }
    }

    fn pinned_files(&self) -> usize {
        // Count each retained snapshot once, even when the peer locks it repeatedly.
        let mut unique: Vec<&Arc<Snapshot>> = Vec::new();
        for snapshot in self.locks.values().chain(std::iter::once(&self.advertised)) {
            if !unique.iter().any(|other| Arc::ptr_eq(snapshot, other)) {
                unique.push(snapshot);
            }
        }
        unique.iter().map(|snapshot| snapshot.files.len()).sum()
    }

    pub(super) fn set_files(&mut self, paths: Vec<PathBuf>) -> bool {
        self.current = Arc::default();
        if paths.len() > MAX_CLIPBOARD_FILES
            || paths.len() > MAX_PINNED_FILES.saturating_sub(self.pinned_files())
        {
            tracing::warn!(
                count = paths.len(),
                "Rejected outgoing clipboard snapshot resource limit"
            );
            return false;
        }
        let mut files = Vec::new();
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.encode_utf16().count() >= 260 {
                continue;
            }
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                // fstat after opening pins the identity, while O_NONBLOCK prevents
                // a FIFO/device clipboard path from hanging the connection loop.
                options.custom_flags(libc::O_NONBLOCK);
            }
            let Ok(file) = options.open(&path) else {
                continue;
            };
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > self.max_file_size {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            files.push(SourceFile {
                descriptor: FileDescriptor {
                    name: name.to_owned(),
                    file_size: Some(metadata.len()),
                    attributes: None,
                    last_write_time: None,
                },
                file,
                modified,
                #[cfg(unix)]
                identity: metadata_identity(&metadata),
            });
        }
        self.current = Arc::new(Snapshot { files });
        true
    }

    pub(super) fn file_list(&mut self) -> PackedFileList {
        self.advertised = self.current.clone();
        PackedFileList {
            files: self
                .current
                .files
                .iter()
                .map(|file| file.descriptor.clone())
                .collect(),
        }
    }

    pub(super) fn locked_file_list(&mut self, id: u32) -> Option<PackedFileList> {
        let snapshot = self.locks.get(&id)?;
        self.advertised = snapshot.clone();
        Some(PackedFileList {
            files: snapshot
                .files
                .iter()
                .map(|file| file.descriptor.clone())
                .collect(),
        })
    }

    pub(super) fn lock(&mut self, id: u32) {
        // Duplicate locks must never retarget an ID to a newer clipboard copy.
        if self.locks.contains_key(&id) {
            return;
        }
        if self.locks.len() >= MAX_LOCKS {
            tracing::warn!("Rejected outgoing clipboard lock resource limit");
            return;
        }
        self.locks.insert(id, self.current.clone());
    }

    pub(super) fn unlock(&mut self, id: u32) {
        self.locks.remove(&id);
    }

    pub(super) fn contents(&self, request: &FileContentsRequest) -> FileContentsResponse<'static> {
        // An untagged index belongs to the last descriptor list actually sent,
        // never to a newer snapshot captured by a lock or local clipboard change.
        let snapshot = match request.data_id {
            Some(id) => self.locks.get(&id),
            None => Some(&self.advertised),
        };
        let Some(file) = snapshot.and_then(|snapshot| snapshot.files.get(request.index as usize))
        else {
            tracing::debug!(
                index = request.index,
                "Rejected unknown clipboard snapshot or index"
            );
            return FileContentsResponse::new_error(request.stream_id);
        };
        let result = (|| -> std::io::Result<FileContentsResponse<'static>> {
            if !file.unchanged(&file.file.metadata()?) {
                return Err(std::io::Error::other(
                    "Clipboard source changed after advertisement",
                ));
            }
            let size = file.descriptor.file_size.unwrap_or(0);
            if request.flags == FileContentsFlags::SIZE
                && request.position == 0
                && request.requested_size == 8
            {
                return Ok(FileContentsResponse::new_size_response(
                    request.stream_id,
                    size,
                ));
            }
            if request.flags != FileContentsFlags::DATA {
                return Ok(FileContentsResponse::new_error(request.stream_id));
            }
            let length = u64::from(request.requested_size.min(FILE_TRANSFER_CHUNK_SIZE))
                .min(size.saturating_sub(request.position)) as usize;
            let mut data = vec![0; length];
            let mut handle = &file.file;
            if length > 0 {
                handle.seek(SeekFrom::Start(request.position))?;
                handle.read_exact(&mut data)?;
            }
            if !file.unchanged(&file.file.metadata()?) {
                return Err(std::io::Error::other(
                    "Clipboard source changed during read",
                ));
            }
            Ok(FileContentsResponse::new_data_response(
                request.stream_id,
                data,
            ))
        })();
        result.unwrap_or_else(|error| {
            tracing::warn!(index = request.index, %error, "Cannot read outgoing clipboard snapshot");
            FileContentsResponse::new_error(request.stream_id)
        })
    }
}

#[cfg(test)]
mod tests;
