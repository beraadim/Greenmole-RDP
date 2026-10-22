//! Explicit state acknowledgments never request geometry from a disabled screen.

use super::*;

impl DisplayHelper {
    pub(crate) fn set_display_enabled(
        display_id: u32,
        identity: &IdentityLease,
        enabled: bool,
    ) -> Result<()> {
        let (mut selector, proof) = Self::spawn_with_identity(Some(identity))?;
        selector.display_id = Some(display_id);
        write_message(
            selector
                .input
                .as_mut()
                .context("Selector input is closed")?,
            &Request::SetDisplayEnabled {
                display_id,
                identity: proof.expect("requested identity proof"),
                enabled,
            },
        )?;
        let reply = selector.receive_reply(Duration::from_millis(2500), true)?;
        match reply {
            Reply::DisplayEnabled {
                display_id: observed,
                enabled: actual,
            } if observed == display_id && actual == enabled => {
                selector.usable = false;
                Ok(())
            }
            Reply::Error { message } => bail!("Display activation: {message}"),
            _ => bail!("Unexpected display activation acknowledgment"),
        }
    }

    pub(crate) fn is_parked(&self) -> bool {
        self.parked
    }

    pub(crate) fn park_remote_session(&mut self) -> Result<()> {
        transition::observed(Phase::RemoteEnd, Details::display(self.id()), || {
            self.park_remote_session_inner()
        })
    }

    fn park_remote_session_inner(&mut self) -> Result<()> {
        if self.parked {
            return Ok(());
        }
        ensure!(self.usable, "Display helper is no longer available");
        let mut refused = false;
        let result = (|| {
            write_message(
                self.input
                    .as_mut()
                    .context("Display helper input is closed")?,
                &Request::ParkRemoteSession {},
            )?;
            match self.receive_reply(SESSION_TIMEOUT, false)? {
                Reply::Parked {
                    display_id,
                    windows,
                } if display_id == self.id() => {
                    log_window_status(windows);
                    self.parked = true;
                    tracing::info!(
                        display_id,
                        pid = self.child.id(),
                        "Retained virtual display disabled between remote sessions"
                    );
                    Ok(())
                }
                Reply::ParkRefused {
                    display_id,
                    message,
                    windows,
                } if display_id == self.id() => {
                    // A valid refusal proves the local layout was restored and
                    // the display is still active. Keep the stream available
                    // for ordinary End/Shutdown and exact owner retirement.
                    log_window_status(windows);
                    refused = true;
                    bail!("Retained display parking declined: {message}")
                }
                Reply::Error { message } => bail!("Display parking: {message}"),
                _ => bail!("Unexpected retained display parking acknowledgment"),
            }
        })();
        if result.is_err() && !refused {
            self.abort();
        }
        result
    }
}
