use ironrdp_async::NetworkClient;
use ironrdp_connector::sspi::credssp::{
    CredSspServer, CredentialsProxy, ServerError, ServerMode, ServerState, TsRequest,
};
use ironrdp_connector::sspi::generator::{Generator, GeneratorState};
use ironrdp_connector::sspi::negotiate::ProtocolConfig;
use ironrdp_connector::sspi::{
    self, AuthIdentity, KerberosServerConfig, NegotiateConfig, NetworkRequest, Username,
};
use ironrdp_connector::{
    custom_err, general_err, ConnectorError, ConnectorErrorKind, ConnectorResult, ServerName,
    Written,
};
use ironrdp_core::{other_err, WriteBuf};
use ironrdp_pdu::PduHint;
use tracing::debug;

mod local;
#[cfg(test)]
mod tests;

const MAX_CREDSSP_MESSAGE: usize = 60 * 1024;

#[derive(Debug)]
pub(crate) enum CredsspState {
    Ongoing,
    Finished,
    ServerError(sspi::Error),
}

#[derive(Clone, Copy, Debug)]
struct CredsspTsRequestHint;

const CREDSSP_TS_REQUEST_HINT: CredsspTsRequestHint = CredsspTsRequestHint;

impl PduHint for CredsspTsRequestHint {
    fn find_size(&self, bytes: &[u8]) -> ironrdp_core::DecodeResult<Option<(bool, usize)>> {
        match TsRequest::read_length(bytes) {
            Ok(length) if length <= MAX_CREDSSP_MESSAGE => Ok(Some((true, length))),
            Ok(_) => Err(other_err!(
                "CredsspTsRequestHint",
                "CredSSP message exceeds receive limit"
            )),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(other_err!("CredsspTsRequestHint", source: e)),
        }
    }
}

pub enum CredsspRequest {
    Kerberos(TsRequest),
    Local(TsRequest),
}

pub enum CredsspProcessGenerator<'a> {
    Kerberos(
        Generator<'a, NetworkRequest, sspi::Result<Vec<u8>>, Result<ServerState, ServerError>>,
    ),
    Local(
        Generator<'a, NetworkRequest, sspi::Result<Vec<u8>>, Result<ServerState, ServerError>>,
        std::sync::Arc<AuthIdentity>,
    ),
    Failed(Option<CredsspProcessResult>),
}

pub struct CredsspProcessResult {
    response: Option<Vec<u8>>,
    finished: bool,
    error: Option<sspi::Error>,
}

#[derive(Debug)]
enum CredentialServer<'a> {
    Kerberos(CredSspServer<CredentialsProxyImpl<'a>>),
    Local(local::LocalServer),
}

#[derive(Debug)]
pub struct CredsspSequence<'a> {
    server: CredentialServer<'a>,
    state: CredsspState,
}

#[derive(Debug)]
struct CredentialsProxyImpl<'a> {
    credentials: &'a AuthIdentity,
}

impl<'a> CredentialsProxyImpl<'a> {
    fn new(credentials: &'a AuthIdentity) -> Self {
        Self { credentials }
    }
}

impl CredentialsProxy for CredentialsProxyImpl<'_> {
    type AuthenticationData = AuthIdentity;

    fn auth_data_by_user(
        &mut self,
        username: &Username,
    ) -> std::io::Result<Self::AuthenticationData> {
        if username.account_name() != self.credentials.username.account_name() {
            return Err(std::io::Error::other("invalid username"));
        }

        let mut data = self.credentials.clone();
        // keep the original user/domain
        data.username = username.clone();
        Ok(data)
    }

    fn auth_data(&mut self) -> std::io::Result<Vec<Self::AuthenticationData>> {
        Ok(vec![self.credentials.clone()])
    }
}

pub(crate) async fn resolve_generator(
    generator: &mut CredsspProcessGenerator<'_>,
    network_client: &mut impl NetworkClient,
) -> CredsspProcessResult {
    match generator {
        CredsspProcessGenerator::Kerberos(generator) => {
            let mut state = generator.start();
            let result = loop {
                match state {
                    GeneratorState::Suspended(request) => {
                        let response =
                            network_client
                                .send(&request)
                                .await
                                .map_err(|err| ServerError {
                                    ts_request: None,
                                    error: sspi::Error::new(sspi::ErrorKind::InternalError, err),
                                });
                        match response {
                            Ok(response) => state = generator.resume(Ok(response)),
                            Err(error) => break Err(error),
                        }
                    }
                    GeneratorState::Completed(result) => break result,
                }
            };
            kerberos_result(result)
        }
        CredsspProcessGenerator::Local(generator, verifier) => {
            // The application caps upstream authentication tracing independently
            // of RUST_LOG. Local NTLM never needs network I/O: a yield is a
            // safe unsupported-operation failure, not an outbound request.
            let result = generator.resolve_to_result();
            local::process_result(result, verifier)
        }
        CredsspProcessGenerator::Failed(result) => {
            result.take().unwrap_or_else(|| CredsspProcessResult {
                response: None,
                finished: false,
                error: Some(sspi::Error::new(
                    sspi::ErrorKind::OutOfSequence,
                    "CredSSP result already consumed",
                )),
            })
        }
    }
}

fn kerberos_result(result: Result<ServerState, ServerError>) -> CredsspProcessResult {
    let (response, finished, error) = match result {
        Ok(ServerState::ReplyNeeded(request)) => (Some(request), false, None),
        Ok(ServerState::Finished(_)) => (None, true, None),
        Err(error) => (
            error.ts_request.map(|request| *request),
            false,
            Some(error.error),
        ),
    };
    let response = response
        .map(|request| {
            let mut bytes = Vec::new();
            request.encode_ts_request(&mut bytes).map_err(|_| {
                sspi::Error::new(
                    sspi::ErrorKind::InvalidToken,
                    "Could not encode CredSSP response",
                )
            })?;
            if bytes.len() > MAX_CREDSSP_MESSAGE {
                return Err(sspi::Error::new(
                    sspi::ErrorKind::InvalidToken,
                    "CredSSP response exceeds size limit",
                ));
            }
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

impl<'a> CredsspSequence<'a> {
    pub fn next_pdu_hint(&self) -> ConnectorResult<Option<&dyn PduHint>> {
        match &self.state {
            CredsspState::Ongoing => Ok(Some(&CREDSSP_TS_REQUEST_HINT)),
            CredsspState::Finished => Ok(None),
            CredsspState::ServerError(err) => Err(custom_err!("Credssp server error", err.clone())),
        }
    }

    pub fn init(
        creds: &'a AuthIdentity,
        client_computer_name: ServerName,
        public_key: Vec<u8>,
        krb_config: Option<KerberosServerConfig>,
    ) -> ConnectorResult<Self> {
        let client_computer_name = client_computer_name.into_inner();
        let server = if let Some(krb_config) = krb_config {
            // Keep explicitly configured Kerberos and its network client API.
            // Local NTLM initializes lazily to select Guacamole's raw tokens or
            // native Windows SPNEGO before starting the upstream state machine.
            let config: Box<dyn ProtocolConfig> = Box::new(krb_config);
            let server = CredSspServer::new(
                public_key,
                CredentialsProxyImpl::new(creds),
                ServerMode::Negotiate(NegotiateConfig::new(config, None, client_computer_name)),
            )
            .map_err(|error| ConnectorError::new("CredSSP", ConnectorErrorKind::Credssp(error)))?;
            CredentialServer::Kerberos(server)
        } else {
            CredentialServer::Local(local::LocalServer::new(
                creds,
                client_computer_name,
                public_key,
            ))
        };
        Ok(Self {
            server,
            state: CredsspState::Ongoing,
        })
    }

    /// Returns Some(ts_request) when a TS request is received from client,
    pub fn decode_client_message(
        &mut self,
        input: &[u8],
    ) -> ConnectorResult<Option<CredsspRequest>> {
        match self.state {
            CredsspState::Ongoing => {
                if input.len() > MAX_CREDSSP_MESSAGE {
                    return Err(general_err!("CredSSP message exceeds receive limit"));
                }
                let message = match &self.server {
                    CredentialServer::Kerberos(_) => CredsspRequest::Kerberos(
                        TsRequest::from_buffer(input)
                            .map_err(|error| custom_err!("TsRequest", error))?,
                    ),
                    CredentialServer::Local(_) => CredsspRequest::Local(
                        TsRequest::from_buffer(input)
                            .map_err(|_| general_err!("invalid CredSSP request"))?,
                    ),
                };
                debug!(bytes = input.len(), "CredSSP request received");
                Ok(Some(message))
            }
            _ => Err(general_err!(
                "attempted to feed client request to CredSSP sequence in an unexpected state"
            )),
        }
    }

    pub fn process_ts_request(&mut self, request: CredsspRequest) -> CredsspProcessGenerator<'_> {
        match (&mut self.server, request) {
            (CredentialServer::Kerberos(server), CredsspRequest::Kerberos(request)) => {
                CredsspProcessGenerator::Kerberos(server.process(request))
            }
            (CredentialServer::Local(server), CredsspRequest::Local(request)) => {
                server.process(request)
            }
            _ => CredsspProcessGenerator::Failed(Some(CredsspProcessResult {
                response: None,
                finished: false,
                error: Some(sspi::Error::new(
                    sspi::ErrorKind::OutOfSequence,
                    "CredSSP backend changed during authentication",
                )),
            })),
        }
    }

    pub fn handle_process_result(
        &mut self,
        result: CredsspProcessResult,
        output: &mut WriteBuf,
    ) -> ConnectorResult<Written> {
        self.state = if let Some(error) = result.error {
            CredsspState::ServerError(error)
        } else if result.finished {
            CredsspState::Finished
        } else {
            CredsspState::Ongoing
        };
        if let Some(response) = result.response {
            debug!(bytes = response.len(), "CredSSP response sent");
            output
                .unfilled_to(response.len())
                .copy_from_slice(&response);
            output.advance(response.len());
            Ok(Written::from_size(response.len())?)
        } else {
            Ok(Written::Nothing)
        }
    }
}
