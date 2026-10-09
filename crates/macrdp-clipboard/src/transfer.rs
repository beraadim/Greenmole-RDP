use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use ironrdp_cliprdr::pdu::*;

use crate::file::{read_file_range, FILE_TRANSFER_CHUNK_SIZE};

#[derive(Debug, Clone, Copy, PartialEq)]
enum RequestKind {
    Size,
    Data,
}

const MAX_INCOMING_FILES: usize = 256;
const MAX_INCOMING_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SESSION_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SESSION_FILES: usize = 1024;
// Bound work when a peer returns tiny partial results for one 64 KiB range.
const MAX_RANGE_FOLLOWUPS: u8 = 64;

#[derive(Debug, Clone, Copy)]
struct StreamMapping {
    file_index: u32,
    kind: RequestKind,
    position: usize,
    requested_size: usize,
    followups_remaining: u8,
}

pub struct FileTransferManager {
    local_files: Vec<PathBuf>,
    incoming_descriptors: Vec<FileDescriptor>,
    incoming_data: HashMap<u32, Vec<u8>>,
    incoming_received: HashMap<u32, usize>,
    incoming_complete: HashSet<u32>,
    incoming_failed: HashSet<u32>,
    stream_to_index: HashMap<u32, StreamMapping>,
    temp_dir: PathBuf,
    max_file_size: u64,
    locked: bool,
    next_stream_id: u32,
    stored_bytes: u64,
    stored_files: usize,
    // Each connection owns a private directory; remote names never overwrite an existing path.
    _directory: Option<tempfile::TempDir>,
}

impl FileTransferManager {
    pub fn new(temp_dir: PathBuf, max_file_size: u64) -> Self {
        fs::create_dir_all(&temp_dir).ok();
        let directory = tempfile::Builder::new()
            .prefix("session-")
            .tempdir_in(&temp_dir)
            .ok();
        if directory.is_none() {
            tracing::warn!("Cannot create private clipboard directory; incoming files disabled");
        }
        let temp_dir = directory
            .as_ref()
            .map(|dir| dir.path().to_path_buf())
            .unwrap_or(temp_dir);
        Self {
            local_files: Vec::new(),
            incoming_descriptors: Vec::new(),
            incoming_data: HashMap::new(),
            incoming_received: HashMap::new(),
            incoming_complete: HashSet::new(),
            incoming_failed: HashSet::new(),
            stream_to_index: HashMap::new(),
            temp_dir,
            max_file_size,
            locked: false,
            next_stream_id: 1,
            stored_bytes: 0,
            stored_files: 0,
            _directory: directory,
        }
    }

    pub fn set_local_files(&mut self, paths: Vec<PathBuf>) {
        self.local_files = paths;
    }

    pub fn build_file_list(&self) -> PackedFileList {
        let files = self
            .local_files
            .iter()
            .filter_map(|path| {
                let meta = fs::metadata(path).ok()?;
                let name = path.file_name()?.to_string_lossy().to_string();
                Some(FileDescriptor {
                    name,
                    file_size: Some(meta.len()),
                    attributes: None,
                    last_write_time: None,
                })
            })
            .collect();
        PackedFileList { files }
    }

    pub fn handle_contents_request(
        &mut self,
        request: &FileContentsRequest,
    ) -> FileContentsResponse<'static> {
        let index = request.index;
        let stream_id = request.stream_id;

        let Some(path) = self.local_files.get(index as usize) else {
            tracing::warn!(index, "File contents request for invalid file index");
            return FileContentsResponse::new_error(stream_id);
        };

        if request.flags.contains(FileContentsFlags::SIZE) {
            match fs::metadata(path) {
                Ok(meta) => {
                    if meta.len() > self.max_file_size {
                        tracing::warn!(
                            path = %path.display(),
                            size = meta.len(),
                            max = self.max_file_size,
                            "File exceeds size limit"
                        );
                        return FileContentsResponse::new_error(stream_id);
                    }
                    FileContentsResponse::new_size_response(stream_id, meta.len())
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to read file metadata");
                    FileContentsResponse::new_error(stream_id)
                }
            }
        } else if request.flags.contains(FileContentsFlags::DATA) {
            match read_file_range(
                path,
                request.position,
                request.requested_size,
                self.max_file_size,
            ) {
                Ok(data) => FileContentsResponse::new_data_response(stream_id, data),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to read file range");
                    FileContentsResponse::new_error(stream_id)
                }
            }
        } else {
            tracing::warn!("Unknown FileContentsRequest flags");
            FileContentsResponse::new_error(stream_id)
        }
    }

    pub fn set_incoming_descriptors(&mut self, descriptors: Vec<FileDescriptor>) {
        self.incoming_data.clear();
        self.incoming_received.clear();
        self.incoming_complete.clear();
        self.incoming_failed.clear();
        self.stream_to_index.clear();
        if descriptors.len() > MAX_INCOMING_FILES
            || descriptors.len() > MAX_SESSION_FILES.saturating_sub(self.stored_files)
            || self._directory.is_none()
        {
            tracing::warn!(count = descriptors.len(), "Rejected incoming file list");
            self.incoming_descriptors.clear();
        } else {
            self.incoming_descriptors = descriptors;
        }
    }

    pub fn generate_contents_requests(&mut self) -> Vec<FileContentsRequest> {
        let mut requests = Vec::new();
        let mut total_bytes = 0u64;

        for (index, desc) in self.incoming_descriptors.iter().enumerate() {
            let size_stream = self.next_stream_id;
            self.next_stream_id = self.next_stream_id.wrapping_add(1);
            self.stream_to_index.insert(
                size_stream,
                StreamMapping {
                    file_index: index as u32,
                    kind: RequestKind::Size,
                    position: 0,
                    requested_size: 8,
                    followups_remaining: 0,
                },
            );

            requests.push(FileContentsRequest {
                stream_id: size_stream,
                index: index as u32,
                flags: FileContentsFlags::SIZE,
                position: 0,
                requested_size: 8,
                data_id: None,
            });

            let Some(size) = desc.file_size else {
                tracing::warn!(
                    index,
                    name = desc.name,
                    "Skipping incoming file with unknown size"
                );
                self.incoming_failed.insert(index as u32);
                continue;
            };

            let remaining =
                MAX_INCOMING_BYTES.min(MAX_SESSION_FILE_BYTES.saturating_sub(self.stored_bytes));
            if size > self.max_file_size || size > remaining.saturating_sub(total_bytes) {
                tracing::warn!(
                    index,
                    name = desc.name,
                    size,
                    max = self.max_file_size,
                    "Skipping incoming file that exceeds configured size limit"
                );
                self.incoming_failed.insert(index as u32);
                continue;
            }
            total_bytes += size;
            if size == 0 {
                self.incoming_data.insert(index as u32, Vec::new());
                self.incoming_complete.insert(index as u32);
            }

            let mut position = 0u64;
            while position < size {
                let remaining = size - position;
                let requested_size = remaining.min(FILE_TRANSFER_CHUNK_SIZE as u64) as u32;
                let data_stream = self.next_stream_id;
                self.next_stream_id = self.next_stream_id.wrapping_add(1);
                self.stream_to_index.insert(
                    data_stream,
                    StreamMapping {
                        file_index: index as u32,
                        kind: RequestKind::Data,
                        position: position as usize,
                        requested_size: requested_size as usize,
                        followups_remaining: MAX_RANGE_FOLLOWUPS,
                    },
                );

                requests.push(FileContentsRequest {
                    stream_id: data_stream,
                    index: index as u32,
                    flags: FileContentsFlags::DATA,
                    position,
                    requested_size,
                    data_id: None,
                });

                position += requested_size as u64;
            }
        }

        requests
    }

    /// Accept a response once and request the remainder of a legal short read.
    /// MS-RDPECLIP cbRequested is a maximum, not an exact response length.
    pub fn handle_contents_response(
        &mut self,
        response: &FileContentsResponse<'_>,
    ) -> Option<FileContentsRequest> {
        let stream_id = response.stream_id();
        let Some(mapping) = self.stream_to_index.remove(&stream_id) else {
            tracing::warn!(stream_id, "Unknown stream_id in file contents response");
            return None;
        };
        let file_index = mapping.file_index;
        let kind = mapping.kind;

        if kind == RequestKind::Size {
            return None;
        }

        if self.incoming_failed.contains(&file_index) {
            return None;
        }

        let data = response.data();
        if data.is_empty()
            || data.len() > mapping.requested_size
            || (data.len() < mapping.requested_size && mapping.followups_remaining == 0)
        {
            tracing::warn!(
                stream_id,
                file_index,
                "Invalid or non-progressing file contents response"
            );
            self.incoming_failed.insert(file_index);
            self.incoming_data.remove(&file_index);
            self.incoming_received.remove(&file_index);
            return None;
        }

        let Some(desc) = self.incoming_descriptors.get(file_index as usize) else {
            tracing::warn!(file_index, "Missing descriptor for incoming file data");
            self.incoming_failed.insert(file_index);
            return None;
        };
        let expected = desc.file_size.unwrap_or(0) as usize;
        let current = self
            .incoming_received
            .get(&file_index)
            .copied()
            .unwrap_or(0);

        if current >= expected && expected > 0 {
            self.incoming_complete.insert(file_index);
            return None;
        }

        let incoming_total = current.saturating_add(data.len());
        if incoming_total > expected
            || incoming_total > self.max_file_size as usize
            || mapping
                .position
                .checked_add(data.len())
                .map_or(true, |end| end > expected)
        {
            tracing::warn!(
                file_index,
                expected,
                current,
                chunk = data.len(),
                max = self.max_file_size,
                "Rejecting incoming file data that exceeds declared or configured limits"
            );
            self.incoming_data.remove(&file_index);
            self.incoming_received.remove(&file_index);
            self.incoming_complete.remove(&file_index);
            self.incoming_failed.insert(file_index);
            return None;
        }

        // Responses may arrive out of order. Map each one to its requested offset and
        // consume the stream id once, so replayed chunks cannot corrupt completion counts.
        let file = self
            .incoming_data
            .entry(file_index)
            .or_insert_with(|| vec![0; expected]);
        file[mapping.position..mapping.position + data.len()].copy_from_slice(data);
        self.incoming_received.insert(file_index, incoming_total);

        let received = incoming_total;
        if received == expected {
            self.incoming_complete.insert(file_index);
        }
        if data.len() < mapping.requested_size {
            let stream_id = self.next_stream_id;
            self.next_stream_id = self.next_stream_id.wrapping_add(1);
            let position = mapping.position + data.len();
            let requested_size = mapping.requested_size - data.len();
            self.stream_to_index.insert(
                stream_id,
                StreamMapping {
                    file_index,
                    kind: RequestKind::Data,
                    position,
                    requested_size,
                    followups_remaining: mapping.followups_remaining - 1,
                },
            );
            return Some(FileContentsRequest {
                stream_id,
                index: file_index,
                flags: FileContentsFlags::DATA,
                position: position as u64,
                requested_size: requested_size as u32,
                data_id: None,
            });
        }
        None
    }

    pub fn all_incoming_complete(&self) -> bool {
        !self.incoming_descriptors.is_empty()
            && self.incoming_complete.len() + self.incoming_failed.len()
                == self.incoming_descriptors.len()
    }

    pub fn flush_incoming_files(&mut self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        // Separate paste batches retain their original names without overwriting an earlier
        // clipboard reference. The connection's TempDir removes all batches on disconnect.
        let batch = match tempfile::Builder::new()
            .prefix("paste-")
            .tempdir_in(&self.temp_dir)
        {
            Ok(directory) => directory,
            Err(error) => {
                tracing::warn!(%error, "Cannot create clipboard batch directory");
                self.clear();
                return paths;
            }
        };

        for (index, desc) in self.incoming_descriptors.iter().enumerate() {
            let index = index as u32;
            if !self.incoming_complete.contains(&index) {
                continue;
            }
            if let Some(data) = self.incoming_data.remove(&index) {
                if desc.name.is_empty()
                    || desc.name == "."
                    || desc.name == ".."
                    || desc.name.contains('/')
                    || desc.name.contains('\\')
                    || desc.name.contains('\0')
                {
                    tracing::warn!(name = desc.name, "Rejected file with suspicious path");
                    continue;
                }

                let file_path = batch.path().join(&desc.name);
                use std::io::Write;
                let write_result = (|| -> std::io::Result<()> {
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&file_path)?;
                    if let Err(error) = file.write_all(&data) {
                        drop(file);
                        let _ = fs::remove_file(&file_path);
                        return Err(error);
                    }
                    Ok(())
                })();
                match write_result {
                    Ok(()) => {
                        self.stored_bytes += data.len() as u64;
                        self.stored_files += 1;
                        tracing::debug!(path = %file_path.display(), bytes = data.len(), "Wrote incoming file");
                        paths.push(file_path);
                    }
                    Err(e) => {
                        tracing::warn!(path = %file_path.display(), error = %e, "Failed to write incoming file");
                    }
                }
            }
        }

        self.incoming_descriptors.clear();
        self.incoming_received.clear();
        self.incoming_complete.clear();
        self.incoming_failed.clear();
        self.stream_to_index.clear();
        if !paths.is_empty() {
            let _ = batch.keep();
        }
        paths
    }

    pub fn lock(&mut self) {
        self.locked = true;
    }

    pub fn unlock(&mut self) {
        self.locked = false;
        self.incoming_descriptors.clear();
        self.incoming_data.clear();
        self.incoming_received.clear();
        self.incoming_complete.clear();
        self.incoming_failed.clear();
        self.stream_to_index.clear();
    }

    pub fn clear(&mut self) {
        self.local_files.clear();
        self.incoming_descriptors.clear();
        self.incoming_data.clear();
        self.incoming_received.clear();
        self.incoming_complete.clear();
        self.incoming_failed.clear();
        self.stream_to_index.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "macrdp-transfer-test-{}-{}",
            std::process::id(),
            name,
        ));
        fs::create_dir_all(&dir).ok();
        dir
    }

    #[test]
    fn handle_size_request() {
        let dir = temp_dir("size");
        let file_path = dir.join("test.txt");
        fs::write(&file_path, b"hello world").unwrap();

        let mut mgr = FileTransferManager::new(dir.clone(), 1024 * 1024);
        mgr.set_local_files(vec![file_path]);

        let request = FileContentsRequest {
            stream_id: 1,
            index: 0,
            flags: FileContentsFlags::SIZE,
            position: 0,
            requested_size: 8,
            data_id: None,
        };

        let response = mgr.handle_contents_request(&request);
        assert_eq!(response.stream_id(), 1);
        let size = response.data_as_size().unwrap();
        assert_eq!(size, 11);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn handle_data_request() {
        let dir = temp_dir("data");
        let file_path = dir.join("data.bin");
        fs::write(&file_path, b"ABCDEF").unwrap();

        let mut mgr = FileTransferManager::new(dir.clone(), 1024 * 1024);
        mgr.set_local_files(vec![file_path]);

        let request = FileContentsRequest {
            stream_id: 2,
            index: 0,
            flags: FileContentsFlags::DATA,
            position: 2,
            requested_size: 3,
            data_id: None,
        };

        let response = mgr.handle_contents_request(&request);
        assert_eq!(response.data(), b"CDE");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn size_limit_enforcement() {
        let dir = temp_dir("limit");
        let file_path = dir.join("big.bin");
        fs::write(&file_path, vec![0u8; 200]).unwrap();

        let mut mgr = FileTransferManager::new(dir.clone(), 100);
        mgr.set_local_files(vec![file_path]);

        let request = FileContentsRequest {
            stream_id: 3,
            index: 0,
            flags: FileContentsFlags::SIZE,
            position: 0,
            requested_size: 8,
            data_id: None,
        };

        let response = mgr.handle_contents_request(&request);
        assert!(response.data().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generate_and_handle_incoming() {
        let dir = temp_dir("incoming");
        let mut mgr = FileTransferManager::new(dir.clone(), 1024 * 1024);

        let descriptors = vec![
            FileDescriptor {
                name: "a.txt".to_string(),
                file_size: Some(5),
                attributes: None,
                last_write_time: None,
            },
            FileDescriptor {
                name: "b.txt".to_string(),
                file_size: Some(3),
                attributes: None,
                last_write_time: None,
            },
        ];
        mgr.set_incoming_descriptors(descriptors);

        let requests = mgr.generate_contents_requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].flags.contains(FileContentsFlags::SIZE));
        assert!(requests[1].flags.contains(FileContentsFlags::DATA));

        let data_stream_a = requests[1].stream_id;
        let data_stream_b = requests[3].stream_id;

        let resp_a = FileContentsResponse::new_data_response(data_stream_a, b"hello".to_vec());
        assert!(mgr.handle_contents_response(&resp_a).is_none());
        assert!(!mgr.all_incoming_complete());

        let resp_b = FileContentsResponse::new_data_response(data_stream_b, b"bye".to_vec());
        assert!(mgr.handle_contents_response(&resp_b).is_none());
        assert!(mgr.all_incoming_complete());

        let paths = mgr.flush_incoming_files();
        assert_eq!(paths.len(), 2);
        assert_eq!(fs::read_to_string(&paths[0]).unwrap(), "hello");
        assert_eq!(fs::read_to_string(&paths[1]).unwrap(), "bye");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unlock_clears_state() {
        let dir = temp_dir("unlock");
        let mut mgr = FileTransferManager::new(dir.clone(), 1024 * 1024);

        mgr.set_local_files(vec![PathBuf::from("/tmp/fake")]);
        mgr.lock();
        assert!(mgr.locked);
        mgr.unlock();
        assert!(!mgr.locked);
        assert!(mgr.incoming_data.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn incoming_requests_are_chunked() {
        let dir = temp_dir("chunked");
        let mut mgr = FileTransferManager::new(dir.clone(), 1024 * 1024);

        mgr.set_incoming_descriptors(vec![FileDescriptor {
            name: "big.bin".to_string(),
            file_size: Some((FILE_TRANSFER_CHUNK_SIZE as u64) + 10),
            attributes: None,
            last_write_time: None,
        }]);

        let requests = mgr.generate_contents_requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].flags.contains(FileContentsFlags::SIZE));
        assert_eq!(requests[1].requested_size, FILE_TRANSFER_CHUNK_SIZE);
        assert_eq!(requests[2].requested_size, 10);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_incoming_file_is_rejected_before_transfer() {
        let dir = temp_dir("oversized");
        let mut mgr = FileTransferManager::new(dir.clone(), 100);

        mgr.set_incoming_descriptors(vec![FileDescriptor {
            name: "big.bin".to_string(),
            file_size: Some(101),
            attributes: None,
            last_write_time: None,
        }]);

        let requests = mgr.generate_contents_requests();
        assert_eq!(requests.len(), 1);
        assert!(mgr.all_incoming_complete());
        assert!(mgr.flush_incoming_files().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_incoming_chunk_is_rejected() {
        let dir = temp_dir("overflow");
        let mut mgr = FileTransferManager::new(dir.clone(), 5);

        mgr.set_incoming_descriptors(vec![FileDescriptor {
            name: "a.txt".to_string(),
            file_size: Some(5),
            attributes: None,
            last_write_time: None,
        }]);
        let requests = mgr.generate_contents_requests();
        let data_stream = requests[1].stream_id;

        let resp = FileContentsResponse::new_data_response(data_stream, b"abcdef".to_vec());
        assert!(mgr.handle_contents_response(&resp).is_none());

        assert!(mgr.all_incoming_complete());
        assert!(mgr.flush_incoming_files().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    fn descriptor(name: &str, size: u64) -> FileDescriptor {
        FileDescriptor {
            name: name.to_string(),
            file_size: Some(size),
            attributes: None,
            last_write_time: None,
        }
    }

    #[test]
    fn short_response_requests_only_missing_bytes_and_ignores_replays() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = FileTransferManager::new(dir.path().to_path_buf(), 1024 * 1024);
        mgr.set_incoming_descriptors(vec![descriptor(
            "short.bin",
            FILE_TRANSFER_CHUNK_SIZE as u64 + 3,
        )]);
        let requests = mgr.generate_contents_requests();
        assert!(mgr
            .handle_contents_response(&FileContentsResponse::new_data_response(
                requests[2].stream_id,
                b"end".to_vec()
            ))
            .is_none());
        let prefix =
            FileContentsResponse::new_data_response(requests[1].stream_id, vec![1; 16_384]);
        let remainder = mgr.handle_contents_response(&prefix).unwrap();
        assert_eq!(remainder.position, 16_384);
        assert_eq!(remainder.requested_size, FILE_TRANSFER_CHUNK_SIZE - 16_384);
        assert_ne!(remainder.stream_id, requests[1].stream_id);
        assert!(mgr.handle_contents_response(&prefix).is_none());
        assert!(!mgr.all_incoming_complete());
        assert!(mgr
            .handle_contents_response(&FileContentsResponse::new_data_response(
                remainder.stream_id,
                vec![2; remainder.requested_size as usize]
            ))
            .is_none());
        assert!(mgr.all_incoming_complete());
        let paths = mgr.flush_incoming_files();
        let actual = fs::read(&paths[0]).unwrap();
        assert!(actual[..16_384].iter().all(|byte| *byte == 1));
        assert!(actual[16_384..FILE_TRANSFER_CHUNK_SIZE as usize]
            .iter()
            .all(|byte| *byte == 2));
        assert_eq!(&actual[FILE_TRANSFER_CHUNK_SIZE as usize..], b"end");
    }

    #[test]
    fn zero_progress_and_excessive_short_reads_fail_without_unbounded_requests() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = FileTransferManager::new(dir.path().to_path_buf(), 1024);
        mgr.set_incoming_descriptors(vec![descriptor("empty-response", 10)]);
        let requests = mgr.generate_contents_requests();
        assert!(mgr
            .handle_contents_response(&FileContentsResponse::new_data_response(
                requests[1].stream_id,
                Vec::new()
            ))
            .is_none());
        assert!(mgr.incoming_failed.contains(&0));
        mgr.set_incoming_descriptors(vec![descriptor("tiny-responses", 1024)]);
        let requests = mgr.generate_contents_requests();
        let mut stream_id = requests[1].stream_id;
        for expected_position in 1..=MAX_RANGE_FOLLOWUPS {
            let next = mgr
                .handle_contents_response(&FileContentsResponse::new_data_response(
                    stream_id,
                    vec![1],
                ))
                .unwrap();
            assert_eq!(next.position, u64::from(expected_position));
            stream_id = next.stream_id;
        }
        assert!(mgr
            .handle_contents_response(&FileContentsResponse::new_data_response(stream_id, vec![1]))
            .is_none());
        assert!(mgr.incoming_failed.contains(&0));
        assert!(mgr.incoming_data.is_empty());
        assert!(mgr.incoming_received.is_empty());
    }

    #[test]
    fn incoming_file_count_and_aggregate_bytes_are_bounded_before_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = FileTransferManager::new(dir.path().to_path_buf(), MAX_INCOMING_BYTES);
        mgr.set_incoming_descriptors(
            (0..=MAX_INCOMING_FILES)
                .map(|i| descriptor(&format!("{i}"), 1))
                .collect(),
        );
        assert!(mgr.generate_contents_requests().is_empty());
        mgr.set_incoming_descriptors(vec![
            descriptor("first", MAX_INCOMING_BYTES),
            descriptor("second", 1),
        ]);
        let requests = mgr.generate_contents_requests();
        assert!(mgr.incoming_failed.contains(&1));
        assert!(requests
            .iter()
            .filter(|request| request.index == 1)
            .all(|request| request.flags == FileContentsFlags::SIZE));
        assert!(mgr.incoming_data.is_empty());
    }

    #[test]
    fn reordered_or_replayed_chunks_cannot_corrupt_incoming_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = FileTransferManager::new(dir.path().to_path_buf(), 1024 * 1024);
        mgr.set_incoming_descriptors(vec![descriptor(
            "ordered.bin",
            FILE_TRANSFER_CHUNK_SIZE as u64 + 3,
        )]);
        let requests = mgr.generate_contents_requests();
        let tail = FileContentsResponse::new_data_response(requests[2].stream_id, b"end".to_vec());
        assert!(mgr.handle_contents_response(&tail).is_none());
        assert!(mgr.handle_contents_response(&tail).is_none());
        assert!(!mgr.all_incoming_complete());
        assert!(mgr
            .handle_contents_response(&FileContentsResponse::new_data_response(
                requests[1].stream_id,
                vec![1; FILE_TRANSFER_CHUNK_SIZE as usize]
            ))
            .is_none());
        assert!(mgr.all_incoming_complete());
        let paths = mgr.flush_incoming_files();
        let bytes = fs::read(&paths[0]).unwrap();
        assert_eq!(&bytes[FILE_TRANSFER_CHUNK_SIZE as usize..], b"end");
        assert!(bytes[..FILE_TRANSFER_CHUNK_SIZE as usize]
            .iter()
            .all(|b| *b == 1));
    }

    #[test]
    fn zero_length_files_complete_and_nested_names_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = FileTransferManager::new(dir.path().to_path_buf(), 1024);
        mgr.set_incoming_descriptors(vec![
            descriptor("empty.txt", 0),
            descriptor("nested/file", 0),
        ]);
        mgr.generate_contents_requests();
        assert!(mgr.all_incoming_complete());
        let paths = mgr.flush_incoming_files();
        assert_eq!(paths.len(), 1);
        assert_eq!(fs::metadata(&paths[0]).unwrap().len(), 0);
    }

    #[test]
    fn repeated_paste_preserves_names_and_session_storage_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = FileTransferManager::new(dir.path().to_path_buf(), 1024);
        let mut paths = Vec::new();
        for _ in 0..2 {
            mgr.set_incoming_descriptors(vec![descriptor("same.txt", 0)]);
            mgr.generate_contents_requests();
            paths.extend(mgr.flush_incoming_files());
        }
        assert_eq!(paths.len(), 2);
        assert_ne!(paths[0], paths[1]);
        assert_eq!(paths[0].file_name(), paths[1].file_name());
        mgr.stored_bytes = MAX_SESSION_FILE_BYTES;
        mgr.set_incoming_descriptors(vec![descriptor("too-much", 1)]);
        assert!(mgr
            .generate_contents_requests()
            .iter()
            .all(|req| req.flags == FileContentsFlags::SIZE));
        assert!(mgr.incoming_failed.contains(&0));
    }
}
