/// Stable, content-free connection outcomes suitable for exported diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionOutcome {
    ClientDisconnect,
    PeerClosed,
    PeerReset,
    PeerEof,
    ProtocolError,
    TransportError,
    TransportTimeout,
    WriteTimeout,
    IdleTimeout,
    ServerShutdown,
    CredentialsChanged,
    HandshakeTimeout,
    HandshakeRejected,
    AuthenticationFailed,
    ActivationFailed,
    AdmissionThrottled,
    CaptureEnded,
    CaptureError,
    OutputOverflow,
}

/// Only an explicit successful client PDU is a deliberate disconnect. Internal
/// queue closure is a failure even if its dispatch future completed normally.
/// Platform security policy relies on this distinction when restoring lock state.
pub(crate) fn terminal_fallback(source: &str, succeeded: bool, quit: bool) -> ConnectionOutcome {
    match source {
        "control" if quit => ConnectionOutcome::ServerShutdown,
        "control" => ConnectionOutcome::CredentialsChanged,
        "output-queue" => ConnectionOutcome::OutputOverflow,
        "idle-timeout" => ConnectionOutcome::IdleTimeout,
        "display" if succeeded => ConnectionOutcome::CaptureEnded,
        "display" => ConnectionOutcome::CaptureError,
        "pdu" if succeeded => ConnectionOutcome::ClientDisconnect,
        _ => ConnectionOutcome::ProtocolError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_successful_peer_shutdown_is_deliberate() {
        assert_eq!(
            terminal_fallback("pdu", true, false),
            ConnectionOutcome::ClientDisconnect
        );
        for source in [
            "events",
            "display",
            "control",
            "output-queue",
            "idle-timeout",
            "unknown",
        ] {
            for success in [true, false] {
                assert_ne!(
                    terminal_fallback(source, success, false),
                    ConnectionOutcome::ClientDisconnect
                );
            }
        }
        assert_ne!(
            terminal_fallback("pdu", false, false),
            ConnectionOutcome::ClientDisconnect
        );
        assert_eq!(
            terminal_fallback("control", true, true),
            ConnectionOutcome::ServerShutdown
        );
    }
}

impl ConnectionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientDisconnect => "client_disconnect",
            Self::PeerClosed => "peer_closed",
            Self::PeerReset => "peer_reset",
            Self::PeerEof => "peer_eof",
            Self::ProtocolError => "protocol_error",
            Self::TransportError => "transport_error",
            Self::TransportTimeout => "transport_timeout",
            Self::WriteTimeout => "write_timeout",
            Self::IdleTimeout => "idle_timeout",
            Self::ServerShutdown => "server_shutdown",
            Self::CredentialsChanged => "credentials_changed",
            Self::HandshakeTimeout => "handshake_timeout",
            Self::HandshakeRejected => "handshake_rejected",
            Self::AuthenticationFailed => "authentication_failed",
            Self::ActivationFailed => "activation_failed",
            Self::AdmissionThrottled => "admission_throttled",
            Self::CaptureEnded => "capture_ended",
            Self::CaptureError => "capture_error",
            Self::OutputOverflow => "output_overflow",
        }
    }
}
