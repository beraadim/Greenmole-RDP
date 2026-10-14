//! Fair writes between complete transport packets, with a fixed deadline.
//! Each static channel retains logical-message order while other channels can
//! write between bounded batches. Cancellation of an unfinished message poisons
//! every clone, including cancellation between its transport packets.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ironrdp_tokio::FramedWrite;
use tokio::sync::{Mutex, OwnedRwLockReadGuard, RwLock};

pub(super) const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_QUANTUM: usize = 16 * 1024;

/// Distinguish our fixed output budget from an operating-system timeout.
/// Keep this typed payload inside `io::Error` across framed/TLS error contexts;
/// matching error text would mislabel a dead peer as an output deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriteDeadlineExceeded {
    Drain,
    Admission,
    Socket,
}

impl std::fmt::Display for WriteDeadlineExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Drain => "RDP output drain deadline exceeded",
            Self::Admission => "RDP message admission deadline exceeded",
            Self::Socket => "RDP socket write deadline exceeded",
        })
    }
}

impl std::error::Error for WriteDeadlineExceeded {}

impl From<WriteDeadlineExceeded> for io::Error {
    fn from(deadline: WriteDeadlineExceeded) -> Self {
        Self::new(io::ErrorKind::TimedOut, deadline)
    }
}

/// Encoded under the server state lock, written after releasing that lock.
pub(super) struct ChannelOutput {
    pub(super) channel_id: u16,
    pub(super) data: Vec<u8>,
}

pub(super) struct SharedWriter<'w, W: FramedWrite> {
    writer: Rc<Mutex<&'w mut W>>,
    poisoned: Rc<Cell<bool>>,
    channels: Rc<RefCell<HashMap<u16, Rc<Mutex<()>>>>>,
    messages: Arc<RwLock<()>>,
    timeout: Duration,
}

/// Reserve logical-message progress before mutating a protocol processor.
/// Passing this permit to the writer avoids taking a second read lock behind
/// a queued resize writer, which would deadlock the admitted message.
pub(super) struct MessagePermit {
    _guard: OwnedRwLockReadGuard<()>,
    deadline: tokio::time::Instant,
}

impl<W: FramedWrite> Clone for SharedWriter<'_, W> {
    fn clone(&self) -> Self {
        Self {
            writer: self.writer.clone(),
            poisoned: self.poisoned.clone(),
            channels: self.channels.clone(),
            messages: self.messages.clone(),
            timeout: self.timeout,
        }
    }
}

struct PartialWriteGuard<'a> {
    poisoned: &'a Cell<bool>,
    complete: bool,
}

impl Drop for PartialWriteGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.poisoned.set(true);
        }
    }
}

impl<W: FramedWrite> FramedWrite for SharedWriter<'_, W> {
    type WriteAllFut<'write>
        = core::pin::Pin<Box<dyn core::future::Future<Output = io::Result<()>> + 'write>>
    where
        Self: 'write;

    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> Self::WriteAllFut<'a> {
        self.write_all_if(buf, || true)
    }
}

impl<W: FramedWrite> SharedWriter<'_, W> {
    pub(super) fn try_begin_message(&self) -> Option<MessagePermit> {
        self.messages
            .clone()
            .try_read_owned()
            .ok()
            .map(|guard| MessagePermit {
                _guard: guard,
                deadline: tokio::time::Instant::now() + self.timeout,
            })
    }

    /// Reactivation keeps the same TCP stream and channel reassembly state.
    /// Drain admitted messages before Deactivate All, and prevent new messages
    /// from starting while its producer returns the reactivation run state.
    pub(super) async fn write_barrier(&mut self, data: &[u8]) -> io::Result<()> {
        let messages = self.messages.clone();
        let result = tokio::time::timeout(self.timeout, async {
            let _exclusive = messages.write().await;
            self.write_all(data).await
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                self.poisoned.set(true);
                Err(WriteDeadlineExceeded::Drain.into())
            }
        }
    }

    pub(super) async fn write_channel(&mut self, output: &ChannelOutput) -> io::Result<()> {
        self.write_channel_if(output, || true).await
    }

    /// Hold the channel gate for the entire encoded message: SVC fragments,
    /// DVC fragments and legacy audio's WaveInfo/WaveData must retain order.
    /// Socket fairness uses only complete TPKT boundaries. There are no queued
    /// copies here; the session's three bounded producers retain their buffers.
    pub(super) async fn write_channel_if(
        &mut self,
        output: &ChannelOutput,
        admit: impl Fn() -> bool,
    ) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let guard =
            match tokio::time::timeout_at(deadline, self.messages.clone().read_owned()).await {
                Ok(guard) => guard,
                Err(_) => {
                    self.poisoned.set(true);
                    return Err(WriteDeadlineExceeded::Admission.into());
                }
            };
        let permit = MessagePermit {
            _guard: guard,
            deadline,
        };
        self.write_admitted_channel(output, permit, admit).await
    }

    pub(super) async fn write_admitted_channel(
        &mut self,
        output: &ChannelOutput,
        permit: MessagePermit,
        admit: impl Fn() -> bool,
    ) -> io::Result<()> {
        if output.data.is_empty() {
            return Ok(());
        }
        validate_channel_packets(&output.data, output.channel_id)?;
        let channel = self
            .channels
            .borrow_mut()
            .entry(output.channel_id)
            .or_insert_with(|| Rc::new(Mutex::new(())))
            .clone();
        let result = tokio::time::timeout_at(permit.deadline, async {
            let _message = permit;
            let _channel = channel.lock().await;
            let mut remaining = output.data.as_slice();
            let mut progress = None;
            while !remaining.is_empty() {
                let length = packet_batch_length(remaining);
                {
                    let mut writer = self.writer.lock().await;
                    self.check_healthy()?;
                    if progress.is_none() {
                        // Audio may expire behind either gate, but after the
                        // first byte we must send every part of the wave.
                        if !admit() {
                            return Ok(());
                        }
                        progress = Some(PartialWriteGuard {
                            poisoned: &self.poisoned,
                            complete: false,
                        });
                    }
                    writer.write_all(&remaining[..length]).await?;
                }
                remaining = &remaining[length..];
                if !remaining.is_empty() {
                    // Also yield when a writable socket completes immediately;
                    // otherwise a large frame monopolizes this local executor.
                    tokio::task::yield_now().await;
                }
            }
            if let Some(progress) = progress.as_mut() {
                progress.complete = true;
            }
            Ok(())
        })
        .await;
        match result {
            Ok(result) => result,
            Err(_) => {
                self.poisoned.set(true);
                Err(WriteDeadlineExceeded::Socket.into())
            }
        }
    }

    fn check_healthy(&self) -> io::Result<()> {
        if self.poisoned.get() {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "RDP output stream is poisoned",
            ))
        } else {
            Ok(())
        }
    }

    fn write_all_if<'a>(
        &'a mut self,
        buf: &'a [u8],
        admit: impl Fn() -> bool + 'a,
    ) -> <Self as FramedWrite>::WriteAllFut<'a> {
        Box::pin(async move {
            let result = tokio::time::timeout(self.timeout, async {
                let mut writer = self.writer.lock().await;
                self.check_healthy()?;
                if !admit() {
                    return Ok(());
                }
                let mut guard = PartialWriteGuard {
                    poisoned: &self.poisoned,
                    complete: false,
                };
                writer.write_all(buf).await?;
                guard.complete = true;
                Ok(())
            })
            .await;
            match result {
                Ok(result) => result,
                Err(_) => {
                    self.poisoned.set(true);
                    Err(WriteDeadlineExceeded::Socket.into())
                }
            }
        })
    }
}

impl<'a, W: FramedWrite> SharedWriter<'a, W> {
    pub(super) fn new(writer: &'a mut W, timeout: Duration) -> Self {
        Self {
            writer: Rc::new(Mutex::new(writer)),
            poisoned: Rc::new(Cell::new(false)),
            channels: Rc::new(RefCell::new(HashMap::new())),
            messages: Arc::new(RwLock::new(())),
            timeout,
        }
    }
}

/// Validate the entire internally encoded batch before the first byte. The
/// channel key must describe every PDU, otherwise same-channel fragments could
/// be interleaved despite their gate. No index or per-packet copy is allocated.
fn validate_channel_packets(mut data: &[u8], channel_id: u16) -> io::Result<()> {
    use ironrdp_core::decode;
    use ironrdp_pdu::{mcs::SendDataIndication, x224::X224};

    while !data.is_empty() {
        if data.len() < 4 || data[0] != 3 || data[1] != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid output TPKT header",
            ));
        }
        let length = usize::from(u16::from_be_bytes([data[2], data[3]]));
        if length < 4 || length > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid output TPKT length",
            ));
        }
        let packet = decode::<X224<SendDataIndication<'_>>>(&data[..length])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        if packet.0.channel_id != channel_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "output channel mismatch",
            ));
        }
        data = &data[length..];
    }
    Ok(())
}

/// Called only after validation. A single PDU larger than the quantum is kept
/// intact; TPKT's 16-bit length bounds that exceptional batch to 65,535 bytes.
fn packet_batch_length(data: &[u8]) -> usize {
    let mut end = 0;
    while end < data.len() {
        let length = usize::from(u16::from_be_bytes([data[end + 2], data[end + 3]]));
        if end > 0 && end + length > WRITE_QUANTUM {
            break;
        }
        end += length;
    }
    end
}

#[cfg(test)]
#[path = "output_timeout_tests.rs"]
mod timeout_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use core::future::Future as _;
    use ironrdp_core::{decode, encode_vec};
    use ironrdp_pdu::{mcs::SendDataIndication, x224::X224};
    use ironrdp_tokio::{split_tokio_framed, TokioFramed};
    use tokio::io::AsyncReadExt as _;

    fn channel_output(channel_id: u16, markers: &[u8], payload_length: usize) -> ChannelOutput {
        let mut data = Vec::new();
        for &marker in markers {
            data.extend(
                encode_vec(&X224(SendDataIndication {
                    initiator_id: 1002,
                    channel_id,
                    user_data: vec![marker; payload_length].into(),
                }))
                .unwrap(),
            );
        }
        ChannelOutput { channel_id, data }
    }

    fn wire_markers(mut bytes: &[u8]) -> Vec<(u16, u8)> {
        let mut markers = Vec::new();
        while !bytes.is_empty() {
            let length = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
            let packet: X224<SendDataIndication<'_>> = decode(&bytes[..length]).unwrap();
            markers.push((packet.0.channel_id, packet.0.user_data[0]));
            bytes = &bytes[length..];
        }
        markers
    }

    #[tokio::test(start_paused = true)]
    async fn audio_expiring_behind_socket_lock_writes_nothing_and_keeps_stream_usable() {
        let (sender, mut receiver) = super::super::event_queue::channel();
        let now = std::time::Instant::now();
        sender
            .send_audio_wave(b"stale".to_vec(), 0, Duration::from_millis(20), now)
            .unwrap();
        let (event, admission) = receiver.data.recv().await.unwrap().into_parts();
        let super::super::ServerEvent::Rdpsnd(ironrdp_rdpsnd::server::RdpsndServerMessage::Wave(
            data,
            _,
        )) = event
        else {
            panic!("missing audio");
        };
        let clock = Cell::new(now);
        let checks = Cell::new(0);
        let (stream, mut peer) = tokio::io::duplex(32);
        let (_, mut native_writer) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut native_writer, Duration::from_secs(10));
        let serialization = writer.writer.clone();
        let display_lock = serialization.lock().await;
        let output = channel_output(1005, &[data[0]], 4);
        let (result, ()) = tokio::join!(
            writer.write_channel_if(&output, || {
                checks.set(checks.get() + 1);
                admission.is_deliverable_at(clock.get())
            }),
            async {
                tokio::task::yield_now().await;
                assert_eq!(
                    checks.get(),
                    0,
                    "freshness must be checked after lock acquisition"
                );
                assert_eq!(sender.audio_stats().snapshot().queued_ms, 20.0);
                clock.set(now + Duration::from_millis(201));
                drop(display_lock);
            }
        );
        result.unwrap();
        assert_eq!(checks.get(), 1);
        assert_eq!(sender.audio_stats().snapshot().stale_frames, 1);
        drop(admission);
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 0.0);
        let mut bytes = [0; 5];
        assert!(
            tokio::time::timeout(Duration::from_millis(1), peer.read(&mut bytes))
                .await
                .is_err()
        );
        writer.write_all(b"fresh").await.unwrap();
        peer.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"fresh");
    }

    #[tokio::test(start_paused = true)]
    async fn throttled_peer_allows_other_channels_between_ordered_message_batches() {
        let first = channel_output(1005, &(0..12).collect::<Vec<_>>(), 4096);
        let same_channel = channel_output(1005, &[99], 16);
        let other_channel = channel_output(1006, &[88], 16);
        let length = first.data.len() + same_channel.data.len() + other_channel.data.len();
        let (stream, mut peer) = tokio::io::duplex(64);
        let (_, mut native) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut native, Duration::from_secs(10));
        let mut same = writer.clone();
        let mut other = writer.clone();
        let (a, b, c, bytes) = tokio::join!(
            biased;
            writer.write_channel(&first),
            same.write_channel(&same_channel),
            other.write_channel(&other_channel),
            async {
                let mut bytes = vec![0; length];
                let mut offset = 0;
                while offset < length {
                    let end = (offset + 257).min(length);
                    offset += peer.read(&mut bytes[offset..end]).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                bytes
            },
        );
        a.unwrap();
        b.unwrap();
        c.unwrap();
        let markers = wire_markers(&bytes);
        assert_eq!(markers[0], (1005, 0));
        assert!(
            markers
                .iter()
                .position(|entry| *entry == (1006, 88))
                .unwrap()
                < 12,
            "another channel must write before the graphics message finishes"
        );
        assert_eq!(
            markers
                .iter()
                .filter_map(|&(channel, marker)| (channel == 1005).then_some(marker))
                .collect::<Vec<_>>(),
            (0..12).chain([99]).collect::<Vec<_>>()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn admitted_wave_is_finished_even_if_it_expires_between_batches() {
        let output = channel_output(1006, &[1, 2, 3, 4, 5, 6], 4096);
        let (stream, mut peer) = tokio::io::duplex(32);
        let (_, mut native) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut native, Duration::from_secs(10));
        let checks = Cell::new(0);
        let (result, bytes) = tokio::join!(
            writer.write_channel_if(&output, || {
                checks.set(checks.get() + 1);
                checks.get() == 1
            }),
            async {
                let mut bytes = vec![0; output.data.len()];
                peer.read_exact(&mut bytes).await.unwrap();
                bytes
            }
        );
        result.unwrap();
        assert_eq!(bytes, output.data);
        assert_eq!(
            checks.get(),
            1,
            "admission applies to the complete logical wave"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reactivation_barrier_drains_admitted_channel_messages_before_deactivation() {
        let output = channel_output(1005, &[1, 2, 3, 4, 5, 6], 4096);
        let deactivate = channel_output(1003, &[99], 16);
        let (stream, mut peer) = tokio::io::duplex(32);
        let (_, mut native) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut native, Duration::from_secs(10));
        let mut barrier = writer.clone();
        // Reserve before processing a protocol event, then let the resize
        // writer queue before that event reaches the socket. Reacquiring a
        // reader inside write_admitted_channel would deadlock this ordering.
        let permit = writer.try_begin_message().unwrap();
        let (sent, drained, bytes) = tokio::join!(
            biased;
            barrier.write_barrier(&deactivate.data),
            writer.write_admitted_channel(&output, permit, || true),
            async {
                let mut bytes = vec![0; output.data.len() + deactivate.data.len()];
                peer.read_exact(&mut bytes).await.unwrap();
                bytes
            },
        );
        sent.unwrap();
        drained.unwrap();
        assert_eq!(&bytes[..output.data.len()], output.data.as_slice());
        assert_eq!(&bytes[output.data.len()..], deactivate.data.as_slice());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_between_complete_packets_still_poisons_unfinished_message() {
        let output = channel_output(1005, &[1, 2, 3, 4, 5, 6], 4096);
        let first_batch = packet_batch_length(&output.data);
        let (stream, mut peer) = tokio::io::duplex(first_batch);
        let (_, mut native) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut native, Duration::from_secs(10));
        // The first poll writes exactly one complete batch, then yields before
        // acquiring the socket for the second batch. Drop it at that boundary.
        {
            let mut pending = Box::pin(writer.write_channel(&output));
            std::future::poll_fn(|context| {
                assert!(pending.as_mut().poll(context).is_pending());
                core::task::Poll::Ready(())
            })
            .await;
        }
        let mut written = vec![0; first_batch];
        peer.read_exact(&mut written).await.unwrap();
        assert_eq!(written, output.data[..first_batch]);
        assert_eq!(
            writer.write_all(b"next").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn invalid_packet_tail_or_channel_is_rejected_before_any_output() {
        let (stream, mut peer) = tokio::io::duplex(128);
        let (_, mut native) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut native, Duration::from_secs(1));
        let mut output = channel_output(1005, &[7], 16);
        output.data.push(3);
        assert_eq!(
            writer.write_channel(&output).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        output.data.pop();
        output.channel_id = 1006;
        assert_eq!(
            writer.write_channel(&output).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        writer.write_all(b"ok").await.unwrap();
        let mut bytes = [0; 2];
        peer.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ok");
    }

    #[tokio::test(start_paused = true)]
    async fn nonreading_peer_times_out_after_partial_write_and_poisons_clones() {
        let (stream, mut peer) = tokio::io::duplex(4);
        let (_, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut writer, Duration::from_millis(25));
        let mut other = writer.clone();
        let error = writer.write_all(b"abcdefgh").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            error
                .get_ref()
                .unwrap()
                .downcast_ref::<WriteDeadlineExceeded>(),
            Some(&WriteDeadlineExceeded::Socket)
        );
        assert_eq!(
            super::super::connection_control::error_outcome(
                &anyhow::Error::new(error).context("display output"),
                crate::gfx::ConnectionOutcome::ProtocolError,
            ),
            crate::gfx::ConnectionOutcome::WriteTimeout
        );
        let mut prefix = [0; 4];
        peer.read_exact(&mut prefix).await.unwrap();
        assert_eq!(&prefix, b"abcd");
        assert_eq!(
            other.write_all(b"next").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1), peer.read(&mut prefix))
                .await
                .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_of_partial_write_also_poisons_stream() {
        let (stream, mut peer) = tokio::io::duplex(1);
        let (_, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut writer, Duration::from_secs(10));
        assert!(
            tokio::time::timeout(Duration::from_millis(1), writer.write_all(b"abc"))
                .await
                .is_err()
        );
        let mut prefix = [0];
        peer.read_exact(&mut prefix).await.unwrap();
        assert_eq!(
            writer.write_all(b"d").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn complete_writes_leave_stream_usable() {
        let (stream, mut peer) = tokio::io::duplex(8);
        let (_, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut writer, Duration::from_secs(1));
        writer.write_all(b"abcd").await.unwrap();
        writer.write_all(b"efgh").await.unwrap();
        let mut bytes = [0; 8];
        peer.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"abcdefgh");
    }
}
