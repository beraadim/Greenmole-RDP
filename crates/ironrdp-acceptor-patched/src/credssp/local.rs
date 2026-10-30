//! Local-password NLA using upstream SSPI's maintained SPNEGO/CredSSP state
//! machines. This boundary does not parse, strip or regenerate inner NTLM tokens.

use std::fmt;
use std::sync::Arc;

use ironrdp_connector::sspi;
use ironrdp_connector::sspi::credssp::{
    CredSspServer, CredentialsProxy, ServerMode, ServerState, TsRequest,
};
use ironrdp_connector::sspi::{AuthIdentity, NegotiateConfig, Username, UsernameParts};
use subtle::ConstantTimeEq;

use super::{CredsspProcessGenerator, CredsspProcessResult, MAX_CREDSSP_MESSAGE};

struct LocalCredentials {
    identity: Arc<AuthIdentity>,
}

fn authorized_username(configured: &Username, supplied: &Username) -> bool {
    if configured.account_name().is_empty() || supplied.account_name().is_empty() {
        return false;
    }
    match (configured.parts(), supplied.parts()) {
        (UsernameParts::DownLevelLogonName(local), UsernameParts::DownLevelLogonName(wire))
            if local.netbios_domain().is_none() =>
        {
            // A domainless configured account names this server's local user.
            // Windows may prefix that user with its own computer/domain or '.'.
            local.account_name() == wire.account_name()
        }
        // Qualified accounts and UPNs retain their complete identity. A UPN
        // suffix is never interpreted as a NetBIOS/local-machine qualifier.
        _ => configured == supplied,
    }
}

impl fmt::Debug for LocalCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalCredentials([redacted])")
    }
}

impl CredentialsProxy for LocalCredentials {
    type AuthenticationData = AuthIdentity;

    fn auth_data_by_user(&mut self, username: &Username) -> std::io::Result<AuthIdentity> {
        // Preserve the existing account policy and exact password verification.
        // The client domain participates in its NTLM proof, so keep that domain
        // after authorizing the configured local account name.
        if !authorized_username(&self.identity.username, username) {
            tracing::warn!(
                stage = "account_lookup",
                reason = "account_not_configured",
                identity_format = identity_format(username),
                account_matches = self.identity.username.account_name() == username.account_name(),
                "Local NLA account lookup rejected"
            );
            return Err(std::io::Error::other("account is not configured"));
        }
        let mut identity = self.identity.as_ref().clone();
        identity.username = username.clone();
        Ok(identity)
    }

    fn auth_data(&mut self) -> std::io::Result<Vec<AuthIdentity>> {
        Ok(vec![self.identity.as_ref().clone()])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenFormat {
    RawNtlm,
    Spnego,
}

impl TokenFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::RawNtlm => "raw_ntlm",
            Self::Spnego => "spnego",
        }
    }
}

fn identity_format(username: &Username) -> &'static str {
    match username.parts() {
        UsernameParts::UserPrincipalName(_) => "upn",
        UsernameParts::DownLevelLogonName(parts) if parts.netbios_domain().is_some() => {
            "qualified_down_level"
        }
        UsernameParts::DownLevelLogonName(_) => "bare_down_level",
    }
}

pub(super) struct LocalServer {
    credentials: Option<LocalCredentials>,
    public_key: Option<Vec<u8>>,
    client_computer_name: Option<String>,
    format: Option<TokenFormat>,
    server: Option<CredSspServer<LocalCredentials>>,
    verifier: Arc<AuthIdentity>,
}

impl fmt::Debug for LocalServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalServer")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

impl LocalServer {
    pub(super) fn new(
        credentials: &AuthIdentity,
        client_computer_name: String,
        public_key: Vec<u8>,
    ) -> Self {
        let identity = Arc::new(credentials.clone());
        Self {
            credentials: Some(LocalCredentials {
                identity: Arc::clone(&identity),
            }),
            public_key: Some(public_key),
            client_computer_name: Some(client_computer_name),
            format: None,
            server: None,
            verifier: identity,
        }
    }

    fn initialize(&mut self, request: &TsRequest) -> sspi::Result<()> {
        if self.server.is_some() {
            return Ok(());
        }
        let token = request
            .nego_tokens
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                sspi::Error::new(
                    sspi::ErrorKind::InvalidToken,
                    "initial CredSSP negotiation token is missing",
                )
            })?;
        // Guacamole sends raw NTLM; native Windows sends SPNEGO/NegTokenInit2.
        // Select once, before parsing. A failed parse/authentication never retries
        // another provider or changes token formats on an existing context.
        let format = if token.starts_with(b"NTLMSSP\0") {
            TokenFormat::RawNtlm
        } else {
            TokenFormat::Spnego
        };
        let mode = match format {
            TokenFormat::RawNtlm => ServerMode::Ntlm(Default::default()),
            TokenFormat::Spnego => ServerMode::Negotiate(
                NegotiateConfig::new(
                    Box::<sspi::ntlm::NtlmConfig>::default(),
                    Some("ntlm,!kerberos,!pku2u".to_owned()),
                    self.client_computer_name.take().unwrap_or_default(),
                )
                .with_local_account_domain_mapping(),
            ),
        };
        let credentials = self.credentials.take().ok_or_else(|| {
            sspi::Error::new(
                sspi::ErrorKind::OutOfSequence,
                "NLA credentials already consumed",
            )
        })?;
        let server = CredSspServer::new(
            self.public_key.take().unwrap_or_default(),
            credentials,
            mode,
        )?;
        self.format = Some(format);
        self.server = Some(server);
        tracing::info!(
            format = format.as_str(),
            "Local CredSSP authentication started"
        );
        Ok(())
    }

    pub(super) fn process(&mut self, request: TsRequest) -> CredsspProcessGenerator<'_> {
        if let Err(error) = self.initialize(&request) {
            return CredsspProcessGenerator::Failed(Some(CredsspProcessResult {
                response: None,
                finished: false,
                error: Some(safe_error(error)),
            }));
        }
        match self.server.as_mut() {
            Some(server) => CredsspProcessGenerator::Local(
                server.process(request),
                Arc::clone(&self.verifier),
                self.format.map(TokenFormat::as_str).unwrap_or("unknown"),
            ),
            None => CredsspProcessGenerator::Failed(Some(CredsspProcessResult {
                response: None,
                finished: false,
                error: Some(sspi::Error::new(
                    sspi::ErrorKind::InternalError,
                    "NLA server was not initialized",
                )),
            })),
        }
    }
}

fn safe_error(error: sspi::Error) -> sspi::Error {
    // Upstream error details can include tokens or account names. Preserve a
    // useful typed category without passing those values into application logs.
    sspi::Error::new(
        error.error_type,
        format!(
            "local CredSSP authentication failed ({:?})",
            error.error_type
        ),
    )
}

pub(super) fn process_result(
    result: Result<ServerState, sspi::credssp::ServerError>,
    verifier: &AuthIdentity,
    format: &str,
) -> CredsspProcessResult {
    let (request, finished, error) = match result {
        Ok(ServerState::ReplyNeeded(request)) => (Some(request), false, None),
        Ok(ServerState::Finished(identity)) => {
            // A valid outer NTLM context must not authorize different credentials
            // in the final encrypted delegation. Compare secrets without a
            // content-dependent early exit; domain qualification follows the
            // same local-account policy as the initial credential lookup.
            let password_matches = identity
                .password
                .as_ref()
                .as_bytes()
                .ct_eq(verifier.password.as_ref().as_bytes());
            if !authorized_username(&verifier.username, &identity.username)
                || !bool::from(password_matches)
            {
                return CredsspProcessResult {
                    response: None,
                    finished: false,
                    error: Some(sspi::Error::new(
                        sspi::ErrorKind::LogonDenied,
                        "delegated credentials do not match the configured local account",
                    )),
                };
            }
            (None, true, None)
        }
        Err(error) => {
            // Fixed categories and the selected format distinguish raw NTLM
            // lookup failures from SPNEGO without formatting upstream errors.
            tracing::warn!(
                stage = "credssp",
                format,
                category = ?error.error.error_type,
                "Local NLA authentication rejected"
            );
            (
                error.ts_request.map(|request| *request),
                false,
                Some(safe_error(error.error)),
            )
        }
    };
    let response = request
        .map(|request| {
            let length = usize::from(request.buffer_len().map_err(safe_error)?);
            if length > MAX_CREDSSP_MESSAGE {
                return Err(sspi::Error::new(
                    sspi::ErrorKind::InvalidToken,
                    "CredSSP response exceeds size limit",
                ));
            }
            let mut bytes = Vec::with_capacity(length);
            request.encode_ts_request(&mut bytes).map_err(|_| {
                sspi::Error::new(
                    sspi::ErrorKind::InvalidToken,
                    "Could not encode CredSSP response",
                )
            })?;
            Ok(bytes)
        })
        .transpose();
    match response {
        Ok(response) => CredsspProcessResult {
            response,
            finished,
            error,
        },
        Err(error) => CredsspProcessResult {
            response: None,
            finished: false,
            error: Some(error),
        },
    }
}
