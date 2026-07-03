use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

static LOG_FILE: OnceLock<std::sync::Mutex<File>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub struct LogBridgeLayer;

impl LogBridgeLayer {
    pub fn new() -> Self {
        Self
    }
}

fn preferred_log_path() -> PathBuf {
    if let Some(data_dir) = dirs::data_dir() {
        return data_dir.join("macrdp").join("macrdp-logs.jsonl");
    }

    std::env::temp_dir().join("macrdp-logs.jsonl")
}

/// Initialize the log file. Call once at startup.
pub fn init_log_file() -> PathBuf {
    let preferred = preferred_log_path();
    let fallback = std::env::temp_dir().join("macrdp-logs.jsonl");

    let (path, file) = match open_log_file(&preferred) {
        Ok(file) => (preferred, file),
        Err(preferred_err) => {
            eprintln!(
                "Failed to initialize preferred log file at {}: {preferred_err}",
                preferred.display()
            );
            let file = open_log_file(&fallback).expect("failed to create fallback log file");
            (fallback, file)
        }
    };

    let _ = LOG_FILE.set(std::sync::Mutex::new(file));
    let _ = LOG_PATH.set(path.clone());
    path
}

fn open_log_file(path: &PathBuf) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Get the log file path (for UI to read).
pub fn log_file_path() -> Option<&'static PathBuf> {
    LOG_PATH.get()
}

impl<S: Subscriber> Layer<S> for LogBridgeLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let Some(file) = LOG_FILE.get() else { return };
        let Ok(mut file) = file.try_lock() else {
            return;
        };

        let metadata = event.metadata();
        let level = metadata.level().as_str();

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let message = if visitor.message.is_empty() {
            metadata.name()
        } else {
            &visitor.message
        };

        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        // Write as JSON line (no serde needed, simple format)
        let _ = writeln!(
            file,
            r#"{{"level":"{}","message":"{}","timestamp":"{}"}}"#,
            level.to_lowercase(),
            message.replace('\\', "\\\\").replace('"', "\\\""),
            ts,
        );
        let _ = file.flush();
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else if self.message.is_empty() {
            self.message = format!("{} = {:?}", field.name(), value);
        } else {
            self.message
                .push_str(&format!(" {} = {:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else if self.message.is_empty() {
            self.message = format!("{} = {}", field.name(), value);
        } else {
            self.message
                .push_str(&format!(" {} = {}", field.name(), value));
        }
    }
}
