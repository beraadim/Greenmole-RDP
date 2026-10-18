//! Small bounded protocol shared with the display-owner child process.

use anyhow::{bail, ensure, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

use crate::DisplayGeometry;

const MAX_MESSAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Initialize {
        width: u16,
        height: u16,
    },
    Geometry {},
    BeginRemoteSession {},
    EndRemoteSession {},
    PrepareCapture {
        width: u16,
        height: u16,
    },
    SelectMode {
        display_id: u32,
        serial: u32,
        width: u16,
        height: u16,
    },
    ConfigureLayout {
        display_id: u32,
        serial: u32,
        primary: u32,
        displays: Vec<crate::layout::Placement>,
    },
    Shutdown {},
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<crate::session_windows::PhaseReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Reply {
    Geometry {
        geometry: DisplayGeometry,
        #[serde(default)]
        windows: WindowStatus,
    },
    Stopped {},
    Error {
        message: String,
    },
}

pub(crate) fn read_message<T: DeserializeOwned>(input: &mut impl BufRead) -> Result<Option<T>> {
    let mut line = Vec::new();
    loop {
        let available = input
            .fill_buf()
            .context("Could not read display helper pipe")?;
        if available.is_empty() {
            ensure!(
                line.is_empty(),
                "Display helper pipe closed during a message"
            );
            return Ok(None);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        ensure!(
            line.len() + consumed <= MAX_MESSAGE_BYTES,
            "Display helper message exceeds {MAX_MESSAGE_BYTES} bytes"
        );
        line.extend_from_slice(&available[..consumed]);
        input.consume(consumed);
        if newline.is_some() {
            return serde_json::from_slice(&line)
                .context("Invalid display helper message")
                .map(Some);
        }
    }
}

pub(crate) fn write_message(output: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let mut encoded =
        serde_json::to_vec(message).context("Could not encode display helper message")?;
    if encoded.len() + 1 > MAX_MESSAGE_BYTES {
        bail!("Display helper message exceeds {MAX_MESSAGE_BYTES} bytes");
    }
    encoded.push(b'\n');
    output
        .write_all(&encoded)
        .context("Could not write display helper pipe")?;
    output
        .flush()
        .context("Could not flush display helper pipe")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn fragmented_messages_round_trip_and_eof_is_distinct() {
        let mut bytes = Vec::new();
        write_message(
            &mut bytes,
            &Request::PrepareCapture {
                width: 1928,
                height: 1088,
            },
        )
        .unwrap();
        let mut reader = BufReader::with_capacity(3, Cursor::new(bytes));
        assert!(matches!(
            read_message::<Request>(&mut reader).unwrap(),
            Some(Request::PrepareCapture {
                width: 1928,
                height: 1088
            })
        ));
        assert!(read_message::<Request>(&mut reader).unwrap().is_none());
    }

    #[test]
    fn reply_preserves_native_geometry_and_negative_origins() {
        let geometry = DisplayGeometry {
            display_id: 17,
            origin_x: -1928.0,
            origin_y: 40.0,
            logical_width: 1928.0,
            logical_height: 1088.0,
            pixel_width: 1928,
            pixel_height: 1088,
        };
        let mut bytes = Vec::new();
        write_message(
            &mut bytes,
            &Reply::Geometry {
                geometry,
                windows: WindowStatus::default(),
            },
        )
        .unwrap();
        match read_message::<Reply>(&mut Cursor::new(bytes))
            .unwrap()
            .unwrap()
        {
            Reply::Geometry {
                geometry: observed, ..
            } => assert_eq!(observed, geometry),
            other => panic!("unexpected reply: {other:?}"),
        }
    }

    #[test]
    fn oversized_unterminated_and_unknown_messages_are_rejected() {
        for bytes in [
            vec![b'x'; MAX_MESSAGE_BYTES + 1],
            br#"{"operation":"geometry"}"#.to_vec(),
            b"{\"operation\":\"execute\"}\n".to_vec(),
            b"{\"operation\":\"geometry\",\"extra\":true}\n".to_vec(),
        ] {
            assert!(read_message::<Request>(&mut Cursor::new(bytes)).is_err());
        }
    }
}
