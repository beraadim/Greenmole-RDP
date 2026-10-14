//! Output deadlines must remain distinguishable from native TCP timeouts.

use super::*;
use crate::gfx::ConnectionOutcome;
use ironrdp_tokio::{split_tokio_framed, TokioFramed};

fn assert_deadline(error: io::Error, expected: WriteDeadlineExceeded) {
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<WriteDeadlineExceeded>(),
        Some(&expected)
    );
    assert_eq!(
        super::super::connection_control::error_outcome(
            &anyhow::Error::new(error).context("channel output"),
            ConnectionOutcome::ProtocolError,
        ),
        ConnectionOutcome::WriteTimeout
    );
}

#[tokio::test(start_paused = true)]
async fn expired_drain_and_admission_budgets_poison_output_with_typed_deadlines() {
    for drain in [true, false] {
        let (stream, _peer) = tokio::io::duplex(64);
        let (_, mut native) = split_tokio_framed(TokioFramed::new(stream));
        let mut writer = SharedWriter::new(&mut native, Duration::from_millis(25));
        if drain {
            // An admitted message that cannot finish must not hold a resize
            // barrier indefinitely, even before any socket write is pending.
            let _permit = writer.try_begin_message().unwrap();
            assert_deadline(
                writer.write_barrier(b"deactivate").await.unwrap_err(),
                WriteDeadlineExceeded::Drain,
            );
        } else {
            // A queued resize also bounds how long a new channel message can
            // wait for admission. No packet bytes have been written yet.
            let _exclusive = writer.messages.clone().write_owned().await;
            let output = ChannelOutput {
                channel_id: 1005,
                data: Vec::new(),
            };
            assert_deadline(
                writer.write_channel(&output).await.unwrap_err(),
                WriteDeadlineExceeded::Admission,
            );
        }
        assert_eq!(
            writer.write_all(b"late").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}

struct NativeTimeoutWriter;

impl FramedWrite for NativeTimeoutWriter {
    type WriteAllFut<'write> = std::future::Ready<io::Result<()>>;

    fn write_all<'a>(&'a mut self, _buf: &'a [u8]) -> Self::WriteAllFut<'a> {
        // Even text identical to our output deadline must not override its
        // untyped native origin. OS timeout errors can surface during a write.
        std::future::ready(Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "RDP socket write deadline exceeded",
        )))
    }
}

#[tokio::test]
async fn native_timeout_during_write_remains_transport_timeout_and_poisons_output() {
    let mut native = NativeTimeoutWriter;
    let mut writer = SharedWriter::new(&mut native, Duration::from_secs(10));
    let error = writer.write_all(b"frame").await.unwrap_err();
    assert_eq!(
        super::super::connection_control::error_outcome(
            &anyhow::Error::new(error).context("display output"),
            ConnectionOutcome::ProtocolError,
        ),
        ConnectionOutcome::TransportTimeout
    );
    assert_eq!(
        writer.write_all(b"late").await.unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}
