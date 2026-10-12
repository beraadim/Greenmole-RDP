//! Dedicated clipboard owner plus independent timeout/change-count monitor.
//! Native file I/O cannot be cancelled safely; teardown never joins a blocked
//! worker. A process-wide admission limit bounds retired, stuck workers.

use crate::{
    pasteboard::PasteboardBridge,
    publication::{PublicationState, RemoteOffer},
    work_queue::WorkQueue,
    worker::ClipboardWorker,
};
use ironrdp_cliprdr::{
    backend::{ClipboardMessage, CliprdrBackend},
    pdu::*,
};
use ironrdp_server::{ServerEvent, ServerEventTx};
use objc2::rc::autoreleasepool;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::RecvTimeoutError,
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const JOB_CAPACITY: usize = 32;
const JOB_BYTES: usize = 32 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_WORKERS: usize = 2;
static WORKERS: AtomicUsize = AtomicUsize::new(0);

pub(crate) enum Command {
    RequestFormatList,
    RemoteCopy(RemoteOffer),
    FormatRequest(FormatDataRequest),
    FormatResponse(OwnedFormatDataResponse),
    FileRequest(FileContentsRequest),
    FileResponse(OwnedFileContentsResponse),
    Lock(LockDataId),
    Unlock(LockDataId),
    #[cfg(test)]
    HoldForTest(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>),
}

struct WorkerSlot;
impl WorkerSlot {
    fn acquire() -> Option<Self> {
        WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_WORKERS).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for WorkerSlot {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

fn fail(sender: &ServerEventTx, shared: &PublicationState, reason: &'static str) {
    shared.stop();
    tracing::warn!(
        reason,
        "Clipboard connection failed; reconnect to reset clipboard state"
    );
    // A producer error invalidates this session's event sender and wakes its
    // failure watcher. It does not stop the listening server.
    let _ = sender.send(ServerEvent::Clipboard(ClipboardMessage::Error(Box::new(
        std::io::Error::other(reason),
    ))));
}

pub(crate) struct ClipboardRuntime {
    queue: WorkQueue<Command>,
    shared: Arc<PublicationState>,
    sender: ServerEventTx,
    pasteboard: PasteboardBridge,
    file_transfer: bool,
    worker: Option<JoinHandle<()>>,
    monitor: Option<JoinHandle<()>>,
}

impl ClipboardRuntime {
    pub fn new(
        sender: ServerEventTx,
        temp_dir: PathBuf,
        max_file_size: u64,
        file_transfer: bool,
        shared: Arc<PublicationState>,
        pasteboard: PasteboardBridge,
    ) -> Self {
        let (queue, receiver) = WorkQueue::new(JOB_CAPACITY, JOB_BYTES);
        let worker_sender = sender.clone();
        let worker_shared = shared.clone();
        let worker_pb = pasteboard.clone();
        let slot = WorkerSlot::acquire();
        let worker = if let Some(slot) = slot {
            thread::Builder::new()
                .name("rdp-clipboard-io".into())
                .spawn(move || {
                    let _slot = slot;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        worker_shared.begin_work(Instant::now());
                        let mut worker = ClipboardWorker::new(
                            worker_sender.clone(),
                            temp_dir,
                            max_file_size,
                            file_transfer,
                            worker_shared.clone(),
                            worker_pb,
                        );
                        worker_shared.finish_work();
                        while worker_shared.is_active() && !worker_sender.is_closed() {
                            match receiver.recv_timeout(POLL_INTERVAL) {
                                Ok(item) => {
                                    if !worker_shared.is_active() || worker_sender.is_closed() {
                                        break;
                                    }
                                    worker_shared.begin_work(Instant::now());
                                    // Keep the byte permit until the operation and
                                    // its output have completed, not merely dequeue.
                                    // Drain Cocoa temporaries after every job, including
                                    // image conversion and multi-file URL publication.
                                    autoreleasepool(|_| {
                                        match item.value {
                                            Command::RequestFormatList => {
                                                worker.on_request_format_list()
                                            }
                                            Command::RemoteCopy(offer) => worker.remote_copy(offer),
                                            Command::FormatRequest(request) => {
                                                worker.on_format_data_request(request)
                                            }
                                            Command::FormatResponse(response) => {
                                                worker.on_format_data_response(response)
                                            }
                                            Command::FileRequest(request) => {
                                                worker.on_file_contents_request(request)
                                            }
                                            Command::FileResponse(response) => {
                                                worker.on_file_contents_response(response)
                                            }
                                            Command::Lock(id) => worker.on_lock(id),
                                            Command::Unlock(id) => worker.on_unlock(id),
                                            #[cfg(test)]
                                            Command::HoldForTest(started, release) => {
                                                let _ = started.send(());
                                                let _ = release.recv();
                                            }
                                        }
                                        worker.tick();
                                    });
                                    worker_shared.finish_work();
                                }
                                Err(RecvTimeoutError::Timeout) => {
                                    worker_shared.begin_work(Instant::now());
                                    autoreleasepool(|_| worker.tick());
                                    worker_shared.finish_work();
                                }
                                Err(RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        // Includes destructor file cleanup in the watchdog budget.
                        worker_shared.begin_work(Instant::now());
                        autoreleasepool(|_| drop(worker));
                        worker_shared.finish_work();
                    }));
                    if result.is_err() {
                        fail(&worker_sender, &worker_shared, "clipboard worker panicked");
                    }
                })
                .ok()
        } else {
            None
        };
        if worker.is_none() {
            fail(
                &sender,
                &shared,
                "clipboard worker unavailable (previous storage operation still blocked)",
            );
        }
        Self {
            queue,
            shared,
            sender,
            pasteboard,
            file_transfer,
            worker,
            monitor: None,
        }
    }

    pub fn enqueue(&self, bytes: usize, make: impl FnOnce() -> Command) {
        if !self.shared.is_active() || self.sender.is_closed() {
            return;
        }
        if !self.queue.try_push(bytes.saturating_add(256), make) {
            // Sending a rejection response here would overtake accepted jobs,
            // corrupting untagged FormatDataResponse ordering. Fail closed.
            fail(
                &self.sender,
                &self.shared,
                "clipboard worker queue capacity exceeded",
            );
        }
    }

    pub fn start_monitor(&mut self) {
        if self.monitor.is_some() || !self.shared.is_active() {
            return;
        }
        let shared = self.shared.clone();
        let sender = self.sender.clone();
        let pb = self.pasteboard.clone();
        let file_transfer = self.file_transfer;
        self.monitor = thread::Builder::new()
            .name("rdp-clipboard-monitor".into())
            .spawn(move || {
                while shared.is_active() && !sender.is_closed() {
                    thread::park_timeout(POLL_INTERVAL);
                    if !shared.is_active() || sender.is_closed() {
                        break;
                    }
                    let keep_running = autoreleasepool(|_| {
                        if shared.expire(Instant::now()) {
                            fail(
                                &sender,
                                &shared,
                                "clipboard request or storage operation timed out",
                            );
                            return false;
                        }
                        if shared.local_copy_changed(&pb) {
                            let formats = ClipboardWorker::build_format_list(&pb, file_transfer);
                            // Empty copies also revoke the previous offer.
                            if sender
                                .send(ServerEvent::Clipboard(ClipboardMessage::SendInitiateCopy(
                                    formats,
                                )))
                                .is_err()
                            {
                                return false;
                            }
                        }
                        true
                    });
                    if !keep_running {
                        break;
                    }
                }
            })
            .ok();
        if self.monitor.is_none() {
            fail(
                &self.sender,
                &self.shared,
                "cannot start clipboard timeout monitor",
            );
        } else {
            tracing::info!(
                jobs = JOB_CAPACITY,
                bytes = JOB_BYTES,
                "Clipboard worker and timeout monitor started"
            );
        }
    }
}

impl Drop for ClipboardRuntime {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(handle) = self.monitor.take() {
            handle.thread().unpark();
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        if let Some(handle) = self.worker.take() {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_panic_still_fails_session_and_allows_runtime_drop() {
        let pb = PasteboardBridge::isolated();
        let shared = Arc::new(PublicationState::new(pb.change_count()));
        let token = shared.remote_copy(&pb);
        let (sender, mut receiver) = ServerEvent::create_channel();
        let (queue, _jobs) = WorkQueue::new(1, 256);
        let runtime = ClipboardRuntime {
            queue,
            shared: shared.clone(),
            sender: sender.clone(),
            pasteboard: pb.clone(),
            file_transfer: false,
            worker: None,
            monitor: None,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            shared.publish(token, &pb, |_| panic!("native publication failed"));
        }));
        assert!(result.is_err());
        // This is the same recovery path used by the worker's catch_unwind.
        // Poison recovery must not panic before notifying the session watcher.
        fail(&sender, &shared, "clipboard worker panicked");
        assert!(sender.is_closed());
        assert!(!shared.is_active());
        assert!(
            receiver.try_recv().is_err(),
            "failure must not stop the listener"
        );
        drop(runtime);
    }

    #[test]
    fn worker_completes_requests_in_order_and_failed_admission_closes_only_session() {
        let directory = tempfile::tempdir().unwrap();
        let pb = PasteboardBridge::isolated();
        pb.write_string("worker text");
        let shared = Arc::new(PublicationState::new(pb.change_count()));
        let (sender, mut receiver) = ServerEvent::create_channel();
        let runtime = ClipboardRuntime::new(
            sender.clone(),
            directory.path().to_owned(),
            1024,
            false,
            shared.clone(),
            pb,
        );
        runtime.enqueue(0, || {
            Command::FormatRequest(FormatDataRequest {
                format: ClipboardFormatId::CF_UNICODETEXT,
            })
        });
        runtime.enqueue(0, || {
            Command::FormatRequest(FormatDataRequest {
                format: ClipboardFormatId::new(999),
            })
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut responses = Vec::new();
        while responses.len() < 2 && Instant::now() < deadline {
            if let Ok(ServerEvent::Clipboard(ClipboardMessage::SendFormatData(response))) =
                receiver.try_recv()
            {
                responses.push(response);
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].to_unicode_string().unwrap(), "worker text");
        assert!(responses[1].is_error());
        runtime.enqueue(JOB_BYTES, || panic!("rejected job must not allocate"));
        assert!(sender.is_closed());
        assert!(!shared.is_active());
        assert!(
            receiver.try_recv().is_err(),
            "overflow must not enqueue server Quit"
        );
    }

    #[test]
    fn blocked_storage_does_not_block_watchdog_or_runtime_drop() {
        let directory = tempfile::tempdir().unwrap();
        let pb = PasteboardBridge::isolated();
        let shared = Arc::new(PublicationState::new(pb.change_count()));
        let (sender, _receiver) = ServerEvent::create_channel();
        let mut runtime = ClipboardRuntime::new(
            sender.clone(),
            directory.path().to_owned(),
            1024,
            false,
            shared.clone(),
            pb,
        );
        runtime.start_monitor();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        runtime.enqueue(0, || Command::HoldForTest(started_tx, release_rx));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Advance the injected start timestamp while the worker remains blocked;
        // the independent monitor must fail this session without an I/O result.
        shared.begin_work(Instant::now() - crate::publication::FORMAT_REQUEST_TIMEOUT);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !sender.is_closed() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let disconnected = sender.is_closed();
        let start = Instant::now();
        drop(runtime);
        let elapsed = start.elapsed();
        let _ = release_tx.send(());
        assert!(disconnected, "watchdog cannot depend on worker progress");
        assert!(
            elapsed < Duration::from_secs(1),
            "drop joined a blocked worker"
        );
    }
}
