/// Stable, content-free connection outcomes suitable for exported diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionOutcome {
    ClientDisconnect,
    PeerClosed,
    PeerReset,
    PeerEof,
    ProtocolError,
    TransportError,
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

impl ConnectionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClientDisconnect => "client_disconnect",
            Self::PeerClosed => "peer_closed",
            Self::PeerReset => "peer_reset",
            Self::PeerEof => "peer_eof",
            Self::ProtocolError => "protocol_error",
            Self::TransportError => "transport_error",
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
