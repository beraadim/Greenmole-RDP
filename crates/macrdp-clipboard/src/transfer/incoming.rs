use crate::file::{FILE_TRANSFER_CHUNK_SIZE, MAX_CLIPBOARD_FILES};
use ironrdp_cliprdr::pdu::*;
use std::collections::{HashMap, VecDeque};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const REQUEST_WINDOW: usize = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_INCOMING_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SESSION_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SESSION_FILES: usize = 1024;
const MAX_RANGE_FOLLOWUPS: u8 = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    PendingSize,
    WaitingSize,
    Downloading,
    Complete,
    Failed,
}

struct IncomingFile {
    descriptor: FileDescriptor,
    state: State,
    staging: Option<tempfile::NamedTempFile>,
    completed: Option<tempfile::TempPath>,
    size: u64,
    next_position: u64,
    received: u64,
}

#[derive(Clone, Copy)]
struct Range {
    index: usize,
    size_request: bool,
    position: u64,
    length: u32,
    followups: u8,
    deadline: Instant,
}

struct Batch {
    files: Vec<IncomingFile>,
    outstanding: HashMap<u32, Range>,
    retry: VecDeque<Range>,
    reserved_bytes: u64,
    started: Instant,
    directory: tempfile::TempDir,
}

/// Completed files retain their private directory until pasteboard publication
/// succeeds. Dropping a stale batch removes it without consuming session quota.
pub(crate) struct PreparedIncoming {
    pub paths: Vec<PathBuf>,
    bytes: u64,
    directory: tempfile::TempDir,
}

pub(super) struct IncomingTransfers {
    directory: Option<PathBuf>,
    max_file_size: u64,
    batch: Option<Batch>,
    // Never reuse an ID during a connection, including after cancellation.
    next_stream: u64,
    stored_bytes: u64,
    stored_files: usize,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && name.encode_utf16().count() < 260
}

impl IncomingTransfers {
    pub(super) fn new(directory: Option<PathBuf>, max_file_size: u64) -> Self {
        Self {
            directory,
            max_file_size,
            batch: None,
            next_stream: 1,
            stored_bytes: 0,
            stored_files: 0,
        }
    }

    pub(super) fn start(&mut self, descriptors: Vec<FileDescriptor>, now: Instant) {
        self.batch = None;
        if descriptors.is_empty() {
            return;
        }
        if descriptors.len() > MAX_CLIPBOARD_FILES
            || descriptors.len() > MAX_SESSION_FILES.saturating_sub(self.stored_files)
        {
            tracing::warn!(
                count = descriptors.len(),
                "Rejected incoming clipboard file count"
            );
            return;
        }
        let Some(directory) = &self.directory else {
            return;
        };
        let Ok(directory) = tempfile::Builder::new()
            .prefix("paste-")
            .tempdir_in(directory)
        else {
            tracing::warn!("Cannot create incoming clipboard batch directory");
            return;
        };
        let budget = MAX_INCOMING_BYTES.min(MAX_SESSION_BYTES.saturating_sub(self.stored_bytes));
        let mut reserved_bytes = 0;
        let mut names = std::collections::HashSet::new();
        let files = descriptors
            .into_iter()
            .map(|descriptor| {
                let size = descriptor.file_size.unwrap_or(0);
                let accepted = valid_name(&descriptor.name)
                    && names.insert(descriptor.name.clone())
                    && size <= self.max_file_size
                    && size <= budget.saturating_sub(reserved_bytes);
                if accepted {
                    reserved_bytes += size;
                }
                IncomingFile {
                    descriptor,
                    state: if accepted {
                        State::PendingSize
                    } else {
                        State::Failed
                    },
                    staging: None,
                    completed: None,
                    size,
                    next_position: 0,
                    received: 0,
                }
            })
            .collect();
        self.batch = Some(Batch {
            files,
            outstanding: HashMap::new(),
            retry: VecDeque::new(),
            reserved_bytes,
            started: now,
            directory,
        });
    }

    /// Deadlines are checked both by responses and independently by the poller.
    /// Expiring the whole unpublished batch prevents incomplete paste publication.
    pub(super) fn expire(&mut self, now: Instant) {
        let expired = self.batch.as_ref().is_some_and(|batch| {
            now.saturating_duration_since(batch.started) >= TRANSFER_TIMEOUT
                || batch
                    .outstanding
                    .values()
                    .any(|range| now >= range.deadline)
        });
        if expired {
            tracing::warn!("Incoming clipboard transfer timed out; staging discarded");
            self.batch = None;
        }
    }

    pub(super) fn requests(&mut self, now: Instant) -> Vec<FileContentsRequest> {
        self.expire(now);
        let mut requests = Vec::new();
        let Some(batch) = &mut self.batch else {
            return requests;
        };
        while batch.outstanding.len() < REQUEST_WINDOW {
            let range = batch.retry.pop_front().or_else(|| {
                for (index, file) in batch.files.iter_mut().enumerate() {
                    if file.state == State::PendingSize {
                        file.state = State::WaitingSize;
                        return Some(Range {
                            index,
                            size_request: true,
                            position: 0,
                            length: 8,
                            followups: 0,
                            deadline: now + REQUEST_TIMEOUT,
                        });
                    }
                    if file.state == State::Downloading && file.next_position < file.size {
                        let position = file.next_position;
                        let length =
                            (file.size - position).min(u64::from(FILE_TRANSFER_CHUNK_SIZE)) as u32;
                        file.next_position += u64::from(length);
                        return Some(Range {
                            index,
                            size_request: false,
                            position,
                            length,
                            followups: MAX_RANGE_FOLLOWUPS,
                            deadline: now + REQUEST_TIMEOUT,
                        });
                    }
                }
                None
            });
            let Some(range) = range else { break };
            let Ok(stream_id) = u32::try_from(self.next_stream) else {
                tracing::warn!("Clipboard stream ID space exhausted; transfer cancelled");
                self.batch = None;
                return Vec::new();
            };
            self.next_stream += 1;
            requests.push(FileContentsRequest {
                stream_id,
                index: range.index as u32,
                flags: if range.size_request {
                    FileContentsFlags::SIZE
                } else {
                    FileContentsFlags::DATA
                },
                position: range.position,
                requested_size: range.length,
                data_id: None,
            });
            batch.outstanding.insert(stream_id, range);
        }
        requests
    }

    fn fail(batch: &mut Batch, index: usize, reason: &'static str) {
        tracing::warn!(index, reason, "Incoming clipboard file rejected");
        let file = &mut batch.files[index];
        file.state = State::Failed;
        file.staging = None;
        file.completed = None;
        batch.outstanding.retain(|_, range| range.index != index);
        batch.retry.retain(|range| range.index != index);
    }

    pub(super) fn respond(
        &mut self,
        response: &FileContentsResponse<'_>,
        now: Instant,
    ) -> Vec<FileContentsRequest> {
        self.expire(now);
        let Some(batch) = &mut self.batch else {
            return Vec::new();
        };
        let Some(range) = batch.outstanding.remove(&response.stream_id()) else {
            tracing::debug!(
                stream_id = response.stream_id(),
                "Ignored stale clipboard file response"
            );
            return Vec::new();
        };
        // IronRDP 0.5 has no public failure accessor. Its derived equality includes
        // the private status bit; the successful comparison borrows data, allocating nothing.
        let failed = response
            != &FileContentsResponse::new_data_response(response.stream_id(), response.data());
        if failed {
            Self::fail(batch, range.index, "peer_error");
        } else if range.size_request {
            let file = &mut batch.files[range.index];
            match response.data_as_size() {
                Ok(size)
                    if size <= self.max_file_size
                        && file
                            .descriptor
                            .file_size
                            .is_none_or(|declared| declared == size)
                        && size.saturating_sub(file.size)
                            <= MAX_INCOMING_BYTES
                                .min(MAX_SESSION_BYTES.saturating_sub(self.stored_bytes))
                                .saturating_sub(batch.reserved_bytes) =>
                {
                    batch.reserved_bytes += size - file.size;
                    file.size = size;
                    match tempfile::NamedTempFile::new_in(batch.directory.path()) {
                        Ok(staging) => {
                            if size == 0 {
                                file.completed = Some(staging.into_temp_path());
                            } else {
                                file.staging = Some(staging);
                            }
                            file.state = if size == 0 {
                                State::Complete
                            } else {
                                State::Downloading
                            };
                        }
                        Err(_) => Self::fail(batch, range.index, "staging_error"),
                    }
                }
                _ => Self::fail(batch, range.index, "invalid_size"),
            }
        } else {
            let data = response.data();
            if data.is_empty()
                || data.len() > range.length as usize
                || (data.len() < range.length as usize && range.followups == 0)
            {
                Self::fail(batch, range.index, "invalid_range");
            } else {
                let file = &mut batch.files[range.index];
                let result = (|| -> std::io::Result<()> {
                    let staging = file
                        .staging
                        .as_mut()
                        .ok_or_else(|| std::io::Error::other("Missing clipboard staging file"))?;
                    staging.seek(SeekFrom::Start(range.position))?;
                    staging.write_all(data)
                })();
                if result.is_err() {
                    Self::fail(batch, range.index, "staging_error");
                } else {
                    file.received += data.len() as u64;
                    if file.received == file.size {
                        file.state = State::Complete;
                        // Completed files retain only their private path, not a
                        // descriptor, while other files in the batch download.
                        file.completed =
                            file.staging.take().map(|staging| staging.into_temp_path());
                    }
                    if data.len() < range.length as usize {
                        // Keep the original deadline: dribbling bytes cannot keep a range alive.
                        batch.retry.push_front(Range {
                            position: range.position + data.len() as u64,
                            length: range.length - data.len() as u32,
                            followups: range.followups - 1,
                            ..range
                        });
                    }
                }
            }
        }
        self.requests(now)
    }

    pub(super) fn complete(&self) -> bool {
        self.batch.as_ref().is_some_and(|batch| {
            batch
                .files
                .iter()
                .all(|file| matches!(file.state, State::Complete | State::Failed))
        })
    }

    pub(super) fn prepare(&mut self) -> Option<PreparedIncoming> {
        if !self.complete() {
            return None;
        }
        let batch = self.batch.take()?;
        let mut paths = Vec::new();
        let mut bytes = 0;
        for file in batch.files {
            if file.state != State::Complete {
                continue;
            }
            let Some(staging) = file.completed else {
                continue;
            };
            let path = batch.directory.path().join(file.descriptor.name);
            match staging.persist_noclobber(&path) {
                Ok(_) => {
                    bytes += file.size;
                    paths.push(path);
                }
                Err(_) => tracing::warn!("Cannot prepare staged clipboard file"),
            }
        }
        if paths.is_empty() {
            return None;
        }
        Some(PreparedIncoming {
            paths,
            bytes,
            directory: batch.directory,
        })
    }

    pub(super) fn retain(&mut self, prepared: PreparedIncoming) -> Vec<PathBuf> {
        self.stored_bytes += prepared.bytes;
        self.stored_files += prepared.paths.len();
        let _ = prepared.directory.keep();
        tracing::info!(
            count = prepared.paths.len(),
            "Published incoming clipboard file batch"
        );
        prepared.paths
    }

    #[cfg(test)]
    fn finish(&mut self) -> Vec<PathBuf> {
        match self.prepare() {
            Some(prepared) => self.retain(prepared),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
