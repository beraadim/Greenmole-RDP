//! Serialized writes with a fixed deadline. A cancelled/failed partial write
//! poisons every clone: appending another PDU could corrupt the byte stream.

use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::time::Duration;

use ironrdp_tokio::FramedWrite;
use tokio::sync::Mutex;

pub(super) const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct SharedWriter<'w, W: FramedWrite> {
    writer: Rc<Mutex<&'w mut W>>,
    poisoned: Rc<Cell<bool>>,
    timeout: Duration,
}

pub(super) struct AdmittedWriter<'a, 'w, W: FramedWrite, F> {
    writer: &'a mut SharedWriter<'w, W>,
    admit: F,
}

impl<W: FramedWrite, F: Fn() -> bool> FramedWrite for AdmittedWriter<'_, '_, W, F> {
    type WriteAllFut<'write>
        = core::pin::Pin<Box<dyn core::future::Future<Output = io::Result<()>> + 'write>>
    where
        Self: 'write;

    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> Self::WriteAllFut<'a> {
        self.writer.write_all_if(buf, &self.admit)
    }
}

impl<W: FramedWrite> Clone for SharedWriter<'_, W> {
    fn clone(&self) -> Self {
        Self {
            writer: self.writer.clone(),
            poisoned: self.poisoned.clone(),
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

impl<'w, W: FramedWrite> SharedWriter<'w, W> {
    /// Check freshness after acquiring the socket lock, immediately before the
    /// first byte. Expiring an unstarted PCM packet does not poison the stream.
    pub(super) fn with_admission<F: Fn() -> bool>(
        &mut self,
        admit: F,
    ) -> AdmittedWriter<'_, 'w, W, F> {
        AdmittedWriter {
            writer: self,
            admit,
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
                if self.poisoned.get() {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "RDP output stream is poisoned",
                    ));
                }
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
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "RDP socket write deadline exceeded",
                    ))
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
            timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_tokio::{split_tokio_framed, TokioFramed};
    use tokio::io::AsyncReadExt as _;

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
        let mut admitted = writer.with_admission(|| {
            checks.set(checks.get() + 1);
            admission.is_deliverable_at(clock.get())
        });
        let (result, ()) = tokio::join!(admitted.write_all(&data), async {
            tokio::task::yield_now().await;
            assert_eq!(
                checks.get(),
                0,
                "freshness must be checked after lock acquisition"
            );
            assert_eq!(sender.audio_stats().snapshot().queued_ms, 20.0);
            clock.set(now + Duration::from_millis(201));
            drop(display_lock);
        });
        result.unwrap();
        drop(admitted);
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
    async fn nonreading_peer_times_out_after_partial_write_and_poisons_clones() {
        let (stream, mut peer) = tokio::io::duplex(4);
        let (_, mut writer) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut writer, Duration::from_millis(25));
        let mut other = writer.clone();
        let error = writer.write_all(b"abcdefgh").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
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
