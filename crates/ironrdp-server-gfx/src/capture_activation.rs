//! Keep session controls and peer closure observable while native capture starts.

use std::future::Future;

use anyhow::{bail, Context as _, Result};
use bytes::BytesMut;
use ironrdp_async::Framed;
use ironrdp_core::decode;
use ironrdp_pdu::input::fast_path::{FastPathInput, FastPathInputEvent};
use ironrdp_pdu::input::InputEvent;
use ironrdp_pdu::mcs;
use ironrdp_pdu::rdp::headers::{ShareControlHeader, ShareControlPdu, ShareDataPdu};
use ironrdp_pdu::x224::X224;
use ironrdp_pdu::Action;
use ironrdp_tokio::FramedRead;

use super::{error_outcome, wait_for_idle_timeout, ConnectionOutcome, RdpServer, ServerEvent};

const MAX_PENDING_PDUS: usize = 128;
const MAX_PENDING_BYTES: usize = 1024 * 1024;

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
        future: impl Future<Output = Result<T>>,
    ) -> Result<Option<(T, Vec<(Action, BytesMut)>)>> {
        let receiver = self.ev_receiver.clone();
        let mut receiver = receiver.lock().await;
        let sender = self.ev_sender.clone();
        let deadline = tokio::time::Instant::now()
            .checked_add(self.handshake_timeout)
            .context("Capture activation timeout exceeds the clock's supported range")?;
        let deadline = tokio::time::sleep_until(deadline);
        let idle = wait_for_idle_timeout(self.input_activity.subscribe(), self.idle_timeout);
        tokio::pin!(future, deadline, idle);
        let mut pending = Vec::new();
        let mut pending_bytes = 0usize;
        loop {
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
                pdu = reader.read_pdu() => {
                    let (action, bytes) = pdu.map_err(|error| {
                        let error = anyhow::Error::from(error);
                        self.record_outcome(error_outcome(&error, ConnectionOutcome::ProtocolError));
                        error
                    })?;
                    // Do not dispatch input against a stale capture transform.
                    // The fixed deadline and byte/count caps prevent a peer from
                    // filling memory while the operating system is unresponsive.
                    if pending.len() >= MAX_PENDING_PDUS || bytes.len() > MAX_PENDING_BYTES - pending_bytes {
                        self.record_outcome(ConnectionOutcome::ProtocolError);
                        bail!("too much input while capture is starting");
                    }
                    if is_user_input(&action, &bytes, io_channel_id) {
                        self.input_activity.send_replace(tokio::time::Instant::now());
                    }
                    pending_bytes += bytes.len();
                    pending.push((action, bytes));
                },
            }
        }
    }
}

#[cfg(test)]
#[path = "capture_activation/tests.rs"]
mod tests;
