//! Opt-in, bounded benchmark output containing metrics rather than credentials or input.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use macrdp_core::Metrics;

const MAX_BYTES: u64 = 64 * 1024 * 1024;

pub struct MetricsRecorder {
    file: File,
    written: u64,
}

impl MetricsRecorder {
    pub fn create(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self {
            file: options
                .open(path)
                .context("Create benchmark metrics file (must not already exist)")?,
            written: 0,
        })
    }

    pub fn record(&mut self, metrics: &Metrics) -> Result<()> {
        let mut line = serde_json::to_vec(&serde_json::json!({
            "type": "metrics", "schema_version": 1,
            "build_version": env!("CARGO_PKG_VERSION"),
            "timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
            "metrics": metrics,
        }))?;
        line.push(b'\n');
        anyhow::ensure!(
            self.written.saturating_add(line.len() as u64) <= MAX_BYTES,
            "Benchmark metrics reached the 64 MiB limit; recording stopped"
        );
        self.file.write_all(&line)?;
        self.file.flush()?;
        self.written += line.len() as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_is_live_readable_private_and_refuses_to_replace_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("metrics.jsonl");
        let mut recorder = MetricsRecorder::create(&path).unwrap();
        recorder
            .record(&Metrics {
                fps: 60,
                bitrate_kbps: 2000,
                ..Default::default()
            })
            .unwrap();
        let line = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["metrics"]["fps"], 60);
        assert_eq!(value["type"], "metrics");
        assert!(value["timestamp_ms"].as_u64().unwrap() > 0);
        assert!(MetricsRecorder::create(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), line);
        recorder.written = MAX_BYTES;
        assert!(recorder.record(&Metrics::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), line);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
