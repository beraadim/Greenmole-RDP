//! Fixed-size, versioned anonymous-pipe frames. No unbounded allocation, strings,
//! paths or credentials are accepted. A partial frame followed by EOF is fatal.

use crate::policy::LockState;
use anyhow::{bail, ensure, Context, Result};
use std::io::{Read, Write};

pub(crate) const FRAME_BYTES: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Message {
    Start { remote_pid: u32, tag: i64 },
    Heartbeat,
    Finish { force_lock: bool },
    Ready { initial: LockState },
    Alive,
    Finished { locked: bool },
    Fault { code: u8 },
}

impl Message {
    pub(crate) fn encode(self) -> [u8; FRAME_BYTES] {
        let mut bytes = [0u8; FRAME_BYTES];
        bytes[..4].copy_from_slice(b"MRPG");
        bytes[4] = 1;
        match self {
            Self::Start { remote_pid, tag } => {
                bytes[5] = 1;
                bytes[8..12].copy_from_slice(&remote_pid.to_le_bytes());
                bytes[12..20].copy_from_slice(&tag.to_le_bytes());
            }
            Self::Heartbeat => bytes[5] = 2,
            Self::Finish { force_lock } => {
                bytes[5] = 3;
                bytes[20] = u8::from(force_lock);
            }
            Self::Ready { initial } => {
                bytes[5] = 4;
                bytes[20] = match initial {
                    LockState::Locked => 1,
                    LockState::Unlocked => 2,
                    LockState::Unknown => 3,
                };
            }
            Self::Alive => bytes[5] = 5,
            Self::Finished { locked } => {
                bytes[5] = 6;
                bytes[20] = u8::from(locked);
            }
            Self::Fault { code } => {
                bytes[5] = 7;
                bytes[20] = code;
            }
        }
        bytes
    }

    pub(crate) fn decode(bytes: &[u8; FRAME_BYTES]) -> Result<Self> {
        ensure!(
            &bytes[..4] == b"MRPG"
                && bytes[4] == 1
                && bytes[6..8] == [0, 0]
                && bytes[21..] == [0, 0, 0],
            "Invalid privacy protocol header"
        );
        let message = match bytes[5] {
            1 => {
                let remote_pid = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
                let tag = i64::from_le_bytes(bytes[12..20].try_into().unwrap());
                ensure!(
                    remote_pid > 1 && tag != 0 && bytes[20] == 0,
                    "Invalid privacy routing identity"
                );
                Self::Start { remote_pid, tag }
            }
            kind => {
                ensure!(
                    bytes[8..20] == [0; 12],
                    "Unexpected privacy protocol payload"
                );
                match (kind, bytes[20]) {
                    (2, 0) => Self::Heartbeat,
                    (3, 0..=1) => Self::Finish {
                        force_lock: bytes[20] != 0,
                    },
                    (4, state @ 1..=3) => Self::Ready {
                        initial: match state {
                            1 => LockState::Locked,
                            2 => LockState::Unlocked,
                            _ => LockState::Unknown,
                        },
                    },
                    (5, 0) => Self::Alive,
                    (6, 0..=1) => Self::Finished {
                        locked: bytes[20] != 0,
                    },
                    (7, code @ 1..=8) => Self::Fault { code },
                    _ => bail!("Unknown privacy protocol message"),
                }
            }
        };
        Ok(message)
    }
}

pub(crate) fn read_message(reader: &mut impl Read) -> Result<Option<Message>> {
    let mut bytes = [0u8; FRAME_BYTES];
    let mut offset = 0;
    while offset < bytes.len() {
        match reader.read(&mut bytes[offset..]) {
            Ok(0) if offset == 0 => return Ok(None),
            Ok(0) => bail!("Truncated privacy protocol frame"),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("Could not read privacy protocol"),
        }
    }
    Message::decode(&bytes).map(Some)
}

pub(crate) fn write_message(writer: &mut impl Write, message: Message) -> Result<()> {
    writer
        .write_all(&message.encode())
        .context("Could not write privacy protocol")?;
    writer.flush().context("Could not flush privacy protocol")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn all_frames_round_trip_without_unbounded_payloads() {
        let messages = [
            Message::Start {
                remote_pid: 42,
                tag: i64::MIN,
            },
            Message::Heartbeat,
            Message::Finish { force_lock: true },
            Message::Ready {
                initial: LockState::Unknown,
            },
            Message::Alive,
            Message::Finished { locked: false },
            Message::Fault { code: 7 },
        ];
        let mut bytes = vec![];
        for message in messages {
            write_message(&mut bytes, message).unwrap();
        }
        assert_eq!(bytes.len(), messages.len() * FRAME_BYTES);
        let mut input = Cursor::new(bytes);
        for message in messages {
            assert_eq!(read_message(&mut input).unwrap(), Some(message));
        }
        assert!(read_message(&mut input).unwrap().is_none());
    }

    #[test]
    fn rejects_partial_unknown_reserved_and_untrusted_identity_frames() {
        let valid = Message::Heartbeat.encode();
        for length in 1..FRAME_BYTES {
            assert!(read_message(&mut &valid[..length]).is_err());
        }
        for index in [0, 4, 5, 6, 8, 12, 20, 21, 23] {
            let mut invalid = valid;
            invalid[index] = 255;
            assert!(Message::decode(&invalid).is_err(), "byte {index}");
        }
        assert!(Message::decode(
            &Message::Start {
                remote_pid: 1,
                tag: 1
            }
            .encode()
        )
        .is_err());
        assert!(Message::decode(
            &Message::Start {
                remote_pid: 42,
                tag: 0
            }
            .encode()
        )
        .is_err());
    }

    // This fixture intentionally executes only pure protocol/policy code. It
    // cannot create an event tap, panel, or lock the development workstation.
    #[test]
    fn subprocess_fixture() {
        if std::env::var_os("MACRDP_PRIVACY_PROTOCOL_FIXTURE").is_none() {
            return;
        }
        let reason = match read_message(&mut std::io::stdin().lock()) {
            Ok(None) => crate::policy::EndReason::PipeClosed,
            Err(_) => crate::policy::EndReason::Malformed,
            _ => panic!("fixture expected EOF or malformed input"),
        };
        let (mut recovery, actions) = crate::policy::Recovery::begin(LockState::Unknown, reason);
        assert_eq!(
            actions,
            [
                crate::policy::Action::InhibitRemote,
                crate::policy::Action::RequestLock,
                crate::policy::Action::TerminateWorker
            ]
        );
        assert_eq!(recovery.poll(false), crate::policy::Action::RetainBarriers);
        assert_eq!(recovery.poll(true), crate::policy::Action::ReleaseBarriers);
        println!("privacy-fixture-policy-passed");
    }

    #[test]
    fn anonymous_pipe_subprocess_eof_and_partial_frame_recover_without_native_effects() {
        use std::process::{Command, Stdio};
        for bytes in [vec![], vec![b'M', b'R', b'P']] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "protocol::tests::subprocess_fixture",
                    "--nocapture",
                ])
                .env("MACRDP_PRIVACY_PROTOCOL_FIXTURE", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&bytes).unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("privacy-fixture-policy-passed")
            );
        }
    }
}
