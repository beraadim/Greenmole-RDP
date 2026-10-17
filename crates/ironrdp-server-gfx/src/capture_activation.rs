//! Keep session controls and peer closure observable while native capture starts.

use std::future::Future;

use anyhow::{bail, Context as _, Result};
use ironrdp_async::Framed;
use ironrdp_core::decode;
use ironrdp_pdu::input::fast_path::{FastPathInput, FastPathInputEvent};
use ironrdp_pdu::input::InputEvent;
use ironrdp_pdu::mcs;
use ironrdp_pdu::rdp::headers::{ShareControlHeader, ShareControlPdu, ShareDataPdu};
use ironrdp_pdu::x224::X224;
use ironrdp_pdu::Action;
use ironrdp_tokio::FramedRead;

use super::{
    error_outcome, wait_for_idle_timeout, ConnectionOutcome, PendingInput, RdpServer, ServerEvent,
};

/// Classify validated input without calling a handler or changing the native
/// desktop. Clipboard, graphics ACKs, QoE and malformed traffic must not extend
/// a user's idle allowance while capture is starting or being reactivated.
fn is_user_input(action: &Action, bytes: &[u8], io_channel_id: u16) -> bool {
    match action {
        Action::FastPath => decode::<FastPathInput>(bytes).is_ok_and(|input| {
            input
                .input_events()
                .iter()
                .any(|event| !matches!(event, FastPathInputEvent::QoeEvent(_)))
        }),
        Action::X224 => {
            let Ok(message) = decode::<X224<mcs::McsMessage<'_>>>(bytes) else {
                return false;
            };
            let mcs::McsMessage::SendDataRequest(data) = message.0 else {
                return false;
            };
            if data.channel_id != io_channel_id {
                return false;
            }
            let Ok(control) = decode::<ShareControlHeader>(data.user_data.as_ref()) else {
                return false;
            };
            let ShareControlPdu::Data(header) = control.share_control_pdu else {
                return false;
            };
            let ShareDataPdu::Input(input) = header.share_data_pdu else {
                return false;
            };
            input
                .0
                .iter()
                .any(|event| !matches!(event, InputEvent::Unused(_)))
        }
    }
}

impl RdpServer {
    /// Native display implementations own cleanup after cancellation. Do not
    /// await that cleanup on this connection's control or socket task.
    pub(super) async fn await_capture_ready<T>(
        &mut self,
        reader: &mut Framed<impl FramedRead>,
        io_channel_id: u16,
        mut pending: PendingInput,
        future: impl Future<Output = Result<T>>,
    ) -> Result<Option<(T, PendingInput)>> {
        let receiver = self.ev_receiver.clone();
        let mut receiver = receiver.lock().await;
        let sender = self.ev_sender.clone();
        let deadline = tokio::time::Instant::now()
            .checked_add(self.handshake_timeout)
            .context("Capture activation timeout exceeds the clock's supported range")?;
        let deadline = tokio::time::sleep_until(deadline);
        let idle = wait_for_idle_timeout(self.input_activity.subscribe(), self.idle_timeout);
        tokio::pin!(future, deadline, idle);
        let mut backpressure_logged = false;
        loop {
            if !pending.can_read() && !backpressure_logged {
                tracing::info!(
                    pending_pdus = pending.len(),
                    pending_bytes = pending.bytes(),
                    "Capture startup input buffer full; pausing socket reads until capture is ready"
                );
                backpressure_logged = true;
            }
            tokio::select! {
                biased;
                event = receiver.controls.recv() => match event {
                    Some(ServerEvent::GetLocalAddr(reply)) => { let _ = reply.send(self.local_addr); }
                    Some(ServerEvent::Quit(_)) | None => {
                        self.quit_requested = true;
                        self.record_outcome(ConnectionOutcome::ServerShutdown);
                        return Ok(None);
                    }
                    Some(ServerEvent::SetCredentials(credentials)) => {
                        self.set_credentials(Some(credentials));
                        self.record_outcome(ConnectionOutcome::CredentialsChanged);
                        return Ok(None);
                    }
                    Some(ServerEvent::SetBitmapQuality(quality)) => {
                        self.apply_bitmap_quality(quality);
                    }
                    Some(_) => unreachable!("only server controls use this lane"),
                },
                reason = sender.failure() => {
                    self.record_outcome(ConnectionOutcome::OutputOverflow);
                    bail!("capture activation: {reason}");
                },
                _ = &mut deadline => {
                    self.record_outcome(ConnectionOutcome::CaptureError);
                    bail!("capture activation timed out");
                },
                _ = &mut idle => {
                    self.record_outcome(ConnectionOutcome::IdleTimeout);
                    return Ok(None);
                },
                result = &mut future => {
                    if result.is_err() { self.record_outcome(ConnectionOutcome::CaptureError); }
                    return result.map(|ready| Some((ready, pending))).context("capture activation failed");
                },
                pdu = reader.read_pdu(), if pending.can_read() => {
                    let (action, bytes) = pdu.map_err(|error| {
                        let error = anyhow::Error::from(error);
                        self.record_outcome(error_outcome(&error, ConnectionOutcome::ProtocolError));
                        error
                    })?;
                    // Do not dispatch input against a stale capture transform.
                    // Stop reading at the shared replay budget rather than
                    // disconnecting on ordinary mouse bursts during activation.
                    // Controls, idle expiry and the fixed deadline remain live;
                    // peer EOF is observed after readiness if the queue is full.
                    if is_user_input(&action, &bytes, io_channel_id) {
                        self.input_activity.send_replace(tokio::time::Instant::now());
                    }
                    pending.push(action, bytes).map_err(|error| {
                        self.record_outcome(ConnectionOutcome::ProtocolError);
                        error
                    })?;
                },
            }
        }
    }
}

#[cfg(test)]
#[path = "capture_activation/tests.rs"]
mod tests;
