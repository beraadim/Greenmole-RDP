//! Bounded asynchronous JSONL diagnostics. Producers never wait for filesystem I/O.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const QUEUE_CAPACITY: usize = 512;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
static LOG_WORKER: OnceLock<LogWorker> = OnceLock::new();

pub struct LogBridgeLayer;
impl LogBridgeLayer {
    pub fn new() -> Self {
        Self
    }
}

enum LogCommand {
    Record(Vec<u8>),
    Flush(mpsc::SyncSender<io::Result<()>>),
}

struct LogWorker {
    path: PathBuf,
    sender: mpsc::SyncSender<LogCommand>,
    dropped: Arc<AtomicU64>,
}

impl LogWorker {
    fn start(path: PathBuf) -> io::Result<Self> {
        let mut log = RotatingLog::open(path.clone(), MAX_FILE_BYTES)?;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let worker_dropped = Arc::clone(&dropped);
        std::thread::Builder::new().name("macrdp-log-writer".into()).spawn(move || {
            let mut reported_error = false;
            while let Ok(command) = receiver.recv() {
                let count = worker_dropped.swap(0, Ordering::Relaxed);
                if count > 0 {
                    let record = serde_json::json!({
                        "level": "warn", "message": "Diagnostic queue overflow; records dropped",
                        "timestamp": chrono::Utc::now().to_rfc3339(), "fields": {"dropped_records": count}
                    });
                    if let Ok(mut line) = serde_json::to_vec(&record) {
                        line.push(b'\n');
                        if let Err(error) = log.append(&line) { eprintln!("Unable to record diagnostic loss: {error}"); }
                    }
                }
                let result = match command {
                    LogCommand::Record(line) => log.append(&line),
                    LogCommand::Flush(reply) => { let _ = reply.send(log.flush()); continue; }
                };
                if let Err(error) = result {
                    if !reported_error { eprintln!("Unable to write Greenmole RDP diagnostics: {error}"); }
                    reported_error = true;
                } else { reported_error = false; }
            }
            let _ = log.flush();
        })?;
        Ok(Self {
            path,
            sender,
            dropped,
        })
    }

    fn record(&self, line: Vec<u8>) {
        if line.len() > MAX_RECORD_BYTES || self.sender.try_send(LogCommand::Record(line)).is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self) -> io::Result<()> {
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        let (reply, result) = mpsc::sync_channel(1);
        let mut command = LogCommand::Flush(reply);
        loop {
            match self.sender.try_send(command) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    command = returned;
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Log flush queue timeout",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Log writer stopped",
                    ))
                }
            }
        }
        result
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?
    }
}

/// Three bounded files in total: active file, `.1`, and `.2`.
struct RotatingLog {
    path: PathBuf,
    file: Option<File>,
    bytes: u64,
    limit: u64,
}

fn archive_path(path: &Path, index: u8) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

impl RotatingLog {
    fn open(path: PathBuf, limit: u64) -> io::Result<Self> {
        let (file, bytes) = reopen_bounded_log(&path, limit)?;
        Ok(Self {
            path,
            file: Some(file),
            bytes,
            limit,
        })
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_ref() {
            file.sync_data()?;
        }
        self.file.take();
        let oldest = archive_path(&self.path, 2);
        match std::fs::remove_file(&oldest) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let previous = archive_path(&self.path, 1);
        match std::fs::rename(&previous, &oldest) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        std::fs::rename(&self.path, previous)?;
        self.file = Some(open_log_file(&self.path)?);
        self.bytes = 0;
        Ok(())
    }

    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        if line.len() as u64 > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Log record exceeds file limit",
            ));
        }
        if self.file.is_none() {
            let (file, bytes) = reopen_bounded_log(&self.path, self.limit)?;
            self.bytes = bytes;
            self.file = Some(file);
        }
        if self.bytes > 0 && self.bytes.saturating_add(line.len() as u64) > self.limit {
            self.rotate()?;
        }
        // Write to the OS cache; flush_logs checkpoints durability after queued records.
        let file = self.file.as_mut().unwrap();
        if let Err(error) = file.write_all(line) {
            // Remove an incomplete JSON record if a full-disk or I/O failure interrupted it.
            file.set_len(self.bytes)?;
            return Err(error);
        }
        self.bytes += line.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.sync_data()?;
        }
        Ok(())
    }
}

fn preferred_log_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("macrdp")
        .join("macrdp-logs.jsonl")
}

/// Initialize once without truncating prior diagnostics. Files are private to the user.
pub fn init_log_file() -> PathBuf {
    LOG_WORKER
        .get_or_init(|| {
            LogWorker::start(preferred_log_path()).unwrap_or_else(|error| {
                eprintln!("Unable to open preferred diagnostic log: {error}");
                let directory = tempfile::Builder::new()
                    .prefix("macrdp-logs-")
                    .tempdir()
                    .expect("Unable to create private diagnostic directory")
                    .keep();
                LogWorker::start(directory.join("macrdp-logs.jsonl"))
                    .expect("Unable to initialize diagnostic writer")
            })
        })
        .path
        .clone()
}

fn open_log_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Diagnostic log must be a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Bound legacy logs too, and discard an incomplete final record left by a crash
/// before appending. Read at most one file budget instead of loading the old log.
fn reopen_bounded_log(path: &Path, limit: u64) -> io::Result<(File, u64)> {
    let mut file = open_log_file(path)?;
    let bytes = file.metadata()?.len();
    if bytes == 0 {
        return Ok((file, 0));
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8];
    file.read_exact(&mut last)?;
    if bytes <= limit && last[0] == b'\n' {
        return Ok((file, bytes));
    }
    let offset = bytes.saturating_sub(limit);
    file.seek(SeekFrom::Start(offset.saturating_sub(1)))?;
    let mut tail = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut tail)?;
    let start = if offset > 0 {
        tail.iter()
            .position(|&byte| byte == b'\n')
            .map_or(tail.len(), |index| index + 1)
    } else {
        0
    };
    let end = tail
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(start, |index| (index + 1).max(start));
    let retained = &tail[start..end];
    crate::credentials::write_private_file(path, retained)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok((open_log_file(path)?, retained.len() as u64))
}

pub fn log_file_path() -> Option<&'static PathBuf> {
    LOG_WORKER.get().map(|worker| &worker.path)
}

/// Drain queued log records and request an OS checkpoint (bounded to two seconds).
pub fn flush_logs() -> io::Result<()> {
    LOG_WORKER.get().map_or(Ok(()), LogWorker::flush)
}

impl<S: Subscriber> Layer<S> for LogBridgeLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let Some(worker) = LOG_WORKER.get() else {
            return;
        };
        match serialize_event(event) {
            Ok(line) => worker.record(line),
            Err(_) => {
                worker.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub(crate) fn serialize_event(event: &tracing::Event<'_>) -> serde_json::Result<Vec<u8>> {
    let metadata = event.metadata();
    let mut visitor = MessageVisitor::default();
    event.record(&mut visitor);
    let mut message = visitor
        .message
        .unwrap_or_else(|| metadata.name().to_owned());
    // Keep the UI's readable message as well as typed fields for diagnosis.
    for (name, value) in &visitor.fields {
        message.push_str(&format!(" {name}={value}"));
    }
    let mut line = serde_json::to_vec(&serde_json::json!({
        "level": metadata.level().as_str().to_lowercase(), "message": message,
        "timestamp": chrono::Utc::now().to_rfc3339(), "target": metadata.target(),
        "fields": visitor.fields,
    }))?;
    line.push(b'\n');
    Ok(line)
}

/// Only the supervising application owns the rotating log. Child workers send
/// bounded records through their private pipe rather than racing its rotation.
pub(crate) fn record_forwarded_record(record: Vec<u8>) {
    if record.len() > MAX_RECORD_BYTES
        || record.last() != Some(&b'\n')
        || serde_json::from_slice::<Value>(&record).is_err()
    {
        return;
    }
    if let Some(worker) = LOG_WORKER.get() {
        worker.record(record);
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    fields: Map<String, Value>,
}
impl MessageVisitor {
    fn record(&mut self, field: &Field, value: Value) {
        if field.name() == "message" {
            self.message = Some(
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            );
        } else {
            self.fields.insert(field.name().to_string(), value);
        }
    }
}
impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let mut output = BoundedField(String::new());
        let _ = std::fmt::write(&mut output, format_args!("{value:?}"));
        self.record(field, Value::String(output.0));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        let mut output = BoundedField(String::new());
        let _ = std::fmt::Write::write_str(&mut output, value);
        self.record(field, Value::String(output.0));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record(field, value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record(field, value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field, value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record(field, Value::from(value));
    }
}

/// Do not allocate an attacker-sized string before checking the queue's record limit.
struct BoundedField(String);
impl std::fmt::Write for BoundedField {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        const LIMIT: usize = 8192;
        let remaining = LIMIT.saturating_sub(self.0.len());
        if value.len() <= remaining {
            self.0.push_str(value);
            return Ok(());
        }
        let mut end = remaining;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&value[..end]);
        Err(std::fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tracing_subscriber::prelude::*;

    #[test]
    fn reopening_appends_and_rotation_retains_exactly_three_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let mut log = RotatingLog::open(path.clone(), 6).unwrap();
        log.append(b"one\n").unwrap();
        drop(log);
        let mut log = RotatingLog::open(path.clone(), 6).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one\n");
        for line in [b"two\n", b"tri\n", b"for\n"] {
            log.append(line).unwrap();
        }
        log.flush().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"for\n");
        assert_eq!(std::fs::read(archive_path(&path, 1)).unwrap(), b"tri\n");
        assert_eq!(std::fs::read(archive_path(&path, 2)).unwrap(), b"two\n");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 3);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn records_escape_controls_and_retain_fields_regardless_of_visit_order() {
        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<Vec<u8>>>>);
        impl<S: Subscriber> Layer<S> for Capture {
            fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
                self.0.lock().unwrap().push(serialize_event(event).unwrap());
            }
        }
        let lines = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Capture(Arc::clone(&lines)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(before = 42u64, message = "line\n\t\r\0\\\"", after = true);
            tracing::info!(message = "other", before = 43u64);
        });
        let lines = lines.lock().unwrap();
        let first: Value = serde_json::from_slice(&lines[0]).unwrap();
        assert_eq!(lines[0].iter().filter(|&&b| b == b'\n').count(), 1);
        assert!(first["message"]
            .as_str()
            .unwrap()
            .starts_with("line\n\t\r\0\\\""));
        assert_eq!(first["fields"]["before"], 42);
        assert_eq!(first["fields"]["after"], true);
        let second: Value = serde_json::from_slice(&lines[1]).unwrap();
        assert_eq!(second["fields"]["before"], 43);
    }

    #[test]
    fn flush_checkpoints_all_enqueued_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        let worker = LogWorker::start(path.clone()).unwrap();
        worker.record(b"{\"message\":\"saved\"}\n".to_vec());
        worker.flush().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"{\"message\":\"saved\"}\n");
    }

    #[test]
    fn reopening_bounds_legacy_logs_and_removes_incomplete_last_record() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log.jsonl");
        std::fs::write(&path, b"old\none\ntwo\npartial").unwrap();
        let mut log = RotatingLog::open(path.clone(), 12).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two\n");
        log.append(b"new\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two\nnew\n");
    }
}
