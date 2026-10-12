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
        Box::pin(async move {
            let result = tokio::time::timeout(self.timeout, async {
                let mut writer = self.writer.lock().await;
                if self.poisoned.get() {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "RDP output stream is poisoned",
                    ));
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
