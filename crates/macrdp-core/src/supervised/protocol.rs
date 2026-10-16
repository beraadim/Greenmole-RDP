//! Private anonymous-pipe protocol. Credentials never enter argv, files or logs.

use std::io::{BufRead, Write};

use anyhow::{ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{callbacks::*, config::ServerConfig, native_watchdog, tls::TlsCertificateInfo};

pub(super) const MAX_MESSAGE_BYTES: usize = 128 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Initialization {
    pub config: ServerConfig,
    // This embedder-selected location is intentionally excluded from normal TOML.
    pub managed_tls_directory: Option<std::path::PathBuf>,
    pub baseline_display_ids: Vec<u32>,
    pub perf_enabled: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", deny_unknown_fields)]
pub(super) enum Command {
    Update {
        request: u64,
        update: ConfigUpdate,
    },
    Stop,
    MoveWindows {
        request: u64,
    },
    OwnWindows {
        request: u64,
        result: Result<usize, String>,
    },
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    pub status: ServerStatus,
    pub metrics: Metrics,
    pub input: InputDebugState,
    pub connection: Option<ConnectionInfo>,
    pub watchdog: native_watchdog::Snapshot,
    pub owned_display_ids: Vec<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub(super) enum Event {
    Ready {
        port: u16,
        certificate: TlsCertificateInfo,
    },
    Snapshot {
        snapshot: Box<Snapshot>,
    },
    Connection {
        value: ConnectionEvent,
    },
    Diagnostic {
        record: String,
    },
    Failed {
        error: String,
    },
    MoveTarget {
        request: u64,
        target: macrdp_display::DisplayGeometry,
    },
    MoveResult {
        request: u64,
        result: Result<macrdp_display::WindowMoveReport, String>,
    },
    Stopped,
    Updated {
        request: u64,
        result: Result<(), String>,
    },
    PerformanceSummary {
        report: String,
    },
}

pub(super) fn read<T: DeserializeOwned>(input: &mut impl BufRead) -> Result<Option<T>> {
    let mut bytes = Vec::new();
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            ensure!(bytes.is_empty(), "Truncated server helper message");
            return Ok(None);
        }
        let length = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |i| i + 1);
        ensure!(
            bytes.len() + length <= MAX_MESSAGE_BYTES,
            "Server helper message exceeds limit"
        );
        bytes.extend_from_slice(&available[..length]);
        input.consume(length);
        if bytes.last() == Some(&b'\n') {
            // serde's error text can include field values; never propagate it.
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| anyhow::anyhow!("Invalid server helper message"));
        }
    }
}

pub(super) fn write(value: &impl Serialize, output: &mut impl Write) -> Result<()> {
    let bytes = serde_json::to_vec(value).context("Cannot serialize server helper message")?;
    ensure!(
        bytes.len() + 1 <= MAX_MESSAGE_BYTES,
        "Server helper message exceeds limit"
    );
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn bounds_and_truncation_are_enforced_without_echoing_secrets() {
        let payload = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        assert!(read::<Command>(&mut BufReader::new(Cursor::new(payload))).is_err());
        let error = read::<Command>(&mut Cursor::new(b"{\"command\":\"private-secret\"}\n"))
            .err()
            .unwrap();
        assert!(!error.to_string().contains("private-secret"));
        assert!(read::<Command>(&mut Cursor::new(b"{\"command\":\"Stop\"}")).is_err());
    }

    #[test]
    fn multiple_messages_keep_their_boundaries() {
        let mut bytes = Vec::new();
        write(&Command::Stop, &mut bytes).unwrap();
        write(&Command::MoveWindows { request: 17 }, &mut bytes).unwrap();
        let mut input = Cursor::new(bytes);
        assert!(matches!(
            read::<Command>(&mut input).unwrap(),
            Some(Command::Stop)
        ));
        assert!(matches!(
            read::<Command>(&mut input).unwrap(),
            Some(Command::MoveWindows { request: 17 })
        ));
        assert!(read::<Command>(&mut input).unwrap().is_none());
    }
}
