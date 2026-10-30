use std::future::Future;
use std::task::{Context, Poll, Waker};

use ironrdp_connector::{sspi, ServerName};
use ironrdp_core::WriteBuf;
use sspi::credssp::{ClientMode, ClientState, CredSspClient, CredSspMode, TsRequest};
use sspi::{AuthIdentity, Credentials, NegotiateConfig, Username};

use super::{resolve_generator, CredsspProcessGenerator, CredsspProcessResult, CredsspSequence};

const PASSWORD: &str = "synthetic-NLA-fixture-password";
const PUBLIC_KEY: &[u8] = b"synthetic TLS subjectPublicKey bytes for binding tests";

struct NoNetwork;
impl ironrdp_async::NetworkClient for NoNetwork {
    async fn send(
        &mut self,
        _: &sspi::NetworkRequest,
    ) -> ironrdp_connector::ConnectorResult<Vec<u8>> {
        panic!("local-password NLA must never contact an external credential service");
    }
}

fn resolve(generator: &mut CredsspProcessGenerator<'_>) -> CredsspProcessResult {
    let mut network = NoNetwork;
    let future = resolve_generator(generator, &mut network);
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("local NLA unexpectedly yielded asynchronously"),
    }
}

fn identity(username: &str, password: &str) -> AuthIdentity {
    AuthIdentity {
        username: Username::parse(username).unwrap(),
        password: password.to_owned().into(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutation {
    None,
    NtlmMic,
    NtlmMicAndTargetInfoFlag,
    SpnegoMic,
    MissingSpnegoMic,
    PublicKey,
    Nonce,
    TokenFormat,
}

// Test-only DER navigation: locate SPNEGO NegTokenResp fields without altering
// any production parser or implementing a second authentication mechanism.
fn tlv(bytes: &[u8], start: usize) -> Option<(u8, usize, usize)> {
    let tag = *bytes.get(start)?;
    let first = *bytes.get(start + 1)?;
    let (length, offset) = if first < 128 {
        (usize::from(first), start + 2)
    } else {
        let width = usize::from(first & 127);
        if width == 0 || width > 4 {
            return None;
        }
        let mut length = 0usize;
        for byte in bytes.get(start + 2..start + 2 + width)? {
            length = length.checked_mul(256)?.checked_add(usize::from(*byte))?;
        }
        (length, start + 2 + width)
    };
    let end = offset.checked_add(length)?;
    (end <= bytes.len()).then_some((tag, offset, end))
}

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut bytes = vec![tag];
    if content.len() < 128 {
        bytes.push(u8::try_from(content.len()).unwrap());
    } else if content.len() <= 255 {
        bytes.extend_from_slice(&[0x81, u8::try_from(content.len()).unwrap()]);
    } else {
        bytes.push(0x82);
        bytes.extend_from_slice(&u16::try_from(content.len()).unwrap().to_be_bytes());
    }
    bytes.extend_from_slice(content);
    bytes
}

fn spnego_field(bytes: &[u8], selector: u8) -> Option<(usize, usize, usize, usize)> {
    let (tag, outer, outer_end) = tlv(bytes, 0)?;
    if tag != 0xa1 || outer_end != bytes.len() {
        return None;
    }
    let (tag, sequence, sequence_end) = tlv(bytes, outer)?;
    if tag != 0x30 {
        return None;
    }
    let mut offset = sequence;
    while offset < sequence_end {
        let (tag, value, end) = tlv(bytes, offset)?;
        if tag == selector {
            return Some((sequence, offset, value, end));
        }
        offset = end;
    }
    None
}

fn mutate(request: &mut TsRequest, mutation: Mutation, wrapped: bool) -> bool {
    if mutation == Mutation::PublicKey {
        if let Some(binding) = request.pub_key_auth.as_mut() {
            if let Some(byte) = binding.last_mut() {
                *byte ^= 1;
                return true;
            }
        }
    }
    if mutation == Mutation::Nonce && request.pub_key_auth.is_some() {
        if let Some(nonce) = request.client_nonce.as_mut() {
            nonce[0] ^= 1;
            return true;
        }
    }
    let Some(token) = request.nego_tokens.as_mut() else {
        return false;
    };
    if matches!(
        mutation,
        Mutation::NtlmMic | Mutation::NtlmMicAndTargetInfoFlag
    ) {
        if let Some(start) = token
            .windows(12)
            .position(|bytes| &bytes[..8] == b"NTLMSSP\0" && bytes[8..12] == 3u32.to_le_bytes())
        {
            let mut flags = u32::from_le_bytes(token[start + 60..start + 64].try_into().unwrap());
            let mic = start + if flags & 0x0200_0000 != 0 { 72 } else { 64 };
            token[mic] ^= 1;
            if mutation == Mutation::NtlmMicAndTargetInfoFlag {
                flags &= !0x0080_0000;
                token[start + 60..start + 64].copy_from_slice(&flags.to_le_bytes());
            }
            return true;
        }
    }
    if matches!(mutation, Mutation::SpnegoMic | Mutation::MissingSpnegoMic) {
        if let Some((sequence, start, value, end)) = spnego_field(token, 0xa3) {
            if mutation == Mutation::SpnegoMic {
                let (tag, mic, mic_end) = tlv(token, value).unwrap();
                assert_eq!(tag, 4);
                assert_eq!(mic_end, end);
                token[mic] ^= 1;
            } else {
                let (_, _, sequence_end) = tlv(token, tlv(token, 0).unwrap().1).unwrap();
                let mut content = token[sequence..start].to_vec();
                content.extend_from_slice(&token[end..sequence_end]);
                *token = der(0xa1, &der(0x30, &content));
            }
            return true;
        }
    }
    if mutation == Mutation::TokenFormat {
        if wrapped {
            if let Some((_, _, value, _)) = spnego_field(token, 0xa2) {
                let (_, inner, end) = tlv(token, value).unwrap();
                if token[inner..end].starts_with(b"NTLMSSP\0") {
                    *token = token[inner..end].to_vec();
                    return true;
                }
            }
        } else if token.starts_with(b"NTLMSSP\0") && token[8..12] == 3u32.to_le_bytes() {
            *token = der(0xa1, &der(0x30, &der(0xa2, &der(4, token))));
            return true;
        }
    }
    false
}

fn server_step(
    server: &mut CredsspSequence<'_>,
    request: TsRequest,
) -> Result<(bool, Option<TsRequest>), sspi::ErrorKind> {
    let mut wire = Vec::new();
    request.encode_ts_request(&mut wire).unwrap();
    let decoded = server.decode_client_message(&wire).unwrap().unwrap();
    let result = {
        let mut generator = server.process_ts_request(decoded);
        resolve(&mut generator)
    };
    let finished = result.finished;
    let error = result.error.as_ref().map(|error| error.error_type);
    let mut output = WriteBuf::new();
    server.handle_process_result(result, &mut output).unwrap();
    if let Some(error) = error {
        assert!(
            server.next_pdu_hint().is_err(),
            "failed context must stay failed"
        );
        return Err(error);
    }
    let response =
        (!output.filled().is_empty()).then(|| TsRequest::from_buffer(output.filled()).unwrap());
    Ok((finished, response))
}

fn handshake(wrapped: bool, username: &str, password: &str, mutation: Mutation) -> bool {
    handshake_configured("remote-user", wrapped, username, password, mutation)
}

fn handshake_configured(
    configured_username: &str,
    wrapped: bool,
    username: &str,
    password: &str,
    mutation: Mutation,
) -> bool {
    handshake_protocol(
        configured_username,
        wrapped,
        false,
        username,
        password,
        mutation,
    )
}

fn handshake_protocol(
    configured_username: &str,
    wrapped: bool,
    prefer_kerberos: bool,
    username: &str,
    password: &str,
    mutation: Mutation,
) -> bool {
    let configured = identity(configured_username, PASSWORD);
    let mut server = CredsspSequence::init(
        &configured,
        ServerName::new("synthetic-client"),
        PUBLIC_KEY.to_vec(),
        None,
    )
    .unwrap();
    let mode = if prefer_kerberos {
        ClientMode::Negotiate(NegotiateConfig::new(
            Box::new(sspi::KerberosConfig {
                kdc_resolution: sspi::KdcResolution::KdcUrl(None),
                client_computer_name: "synthetic-client".to_owned(),
            }),
            Some("kerberos,ntlm,!pku2u".to_owned()),
            "synthetic-client".to_owned(),
        ))
    } else if wrapped {
        ClientMode::Negotiate(NegotiateConfig::new(
            Box::<sspi::ntlm::NtlmConfig>::default(),
            Some("ntlm,!kerberos,!pku2u".to_owned()),
            "synthetic-client".to_owned(),
        ))
    } else {
        ClientMode::Ntlm(Default::default())
    };
    let mut client = CredSspClient::new(
        PUBLIC_KEY.to_vec(),
        Credentials::AuthIdentity(identity(username, password)),
        CredSspMode::WithCredentials,
        mode,
        if prefer_kerberos {
            "TERMSRV/synthetic-server.example.test"
        } else {
            "TERMSRV/127.0.0.1"
        }
        .to_owned(),
    )
    .unwrap();
    let mut reply = TsRequest::default();
    let mut changed = false;
    for step in 0..12 {
        let outgoing = match client.process(reply).resolve_to_result() {
            Ok(ClientState::ReplyNeeded(request) | ClientState::FinalMessage(request)) => request,
            Err(_) => {
                assert!(mutation == Mutation::None || changed);
                return false;
            }
        };
        let mut outgoing = outgoing;
        if prefer_kerberos && step == 0 {
            // The upstream client emits a local U2U TGT request first. This
            // advertises Kerberos before NTLM without contacting any KDC, then
            // exercises selection of a nonpreferred mechanism and its MIC leg.
            const MS_KERBEROS_OID: &[u8] = &[6, 9, 42, 134, 72, 130, 247, 18, 1, 2, 2];
            assert!(outgoing
                .nego_tokens
                .as_ref()
                .unwrap()
                .windows(MS_KERBEROS_OID.len())
                .any(|part| part == MS_KERBEROS_OID));
        }
        if !changed {
            changed = mutate(&mut outgoing, mutation, wrapped);
        }
        match server_step(&mut server, outgoing) {
            Ok((true, _)) => {
                assert!(mutation == Mutation::None || changed);
                return true;
            }
            Ok((false, Some(response))) => reply = response,
            Ok((false, None)) => panic!("incomplete NLA requires a response"),
            Err(_) => {
                assert!(mutation == Mutation::None || changed);
                return false;
            }
        }
    }
    panic!("NLA handshake exceeded bounded test legs");
}

#[test]
fn spnego_can_select_ntlm_after_unsupported_preferred_kerberos() {
    assert!(handshake_protocol(
        "remote-user",
        true,
        true,
        "CLIENT-PC\\remote-user",
        PASSWORD,
        Mutation::None,
    ));
    for mutation in [
        Mutation::SpnegoMic,
        Mutation::MissingSpnegoMic,
        Mutation::PublicKey,
    ] {
        assert!(
            !handshake_protocol(
                "remote-user",
                true,
                true,
                "CLIENT-PC\\remote-user",
                PASSWORD,
                mutation,
            ),
            "{mutation:?}"
        );
    }
}

#[test]
fn complete_raw_ntlm_and_spnego_handshakes_authenticate_only_configured_account() {
    for wrapped in [false, true] {
        for username in ["remote-user", ".\\remote-user", "CLIENT-PC\\remote-user"] {
            assert!(
                handshake(wrapped, username, PASSWORD, Mutation::None),
                "wrapped={wrapped}, username form={username}"
            );
        }
        assert!(!handshake(
            wrapped,
            "another-user",
            PASSWORD,
            Mutation::None
        ));
        assert!(!handshake(
            wrapped,
            "remote-user",
            "synthetic-wrong-password",
            Mutation::None
        ));
    }
}

#[test]
fn upn_is_not_treated_as_a_local_downlevel_domain_alias() {
    // UPN realms are a separate identity form; local-machine qualification must
    // not silently equate them to the configured domainless local account.
    for wrapped in [false, true] {
        assert!(!handshake(
            wrapped,
            "remote-user@example.test",
            PASSWORD,
            Mutation::None
        ));
        for (configured, supplied) in [
            ("SERVER\\remote-user", "OTHER\\remote-user"),
            ("SERVER\\remote-user", "remote-user"),
            ("SERVER\\remote-user", "remote-user@server.test"),
            ("remote-user@server.test", "remote-user@other.test"),
            ("remote-user@server.test", "SERVER\\remote-user"),
        ] {
            assert!(
                !handshake_configured(configured, wrapped, supplied, PASSWORD, Mutation::None),
                "wrapped={wrapped}, configured={configured}, supplied={supplied}"
            );
        }
        assert!(handshake_configured(
            "SERVER\\remote-user",
            wrapped,
            "SERVER\\remote-user",
            PASSWORD,
            Mutation::None
        ));
    }
}

#[test]
fn tampered_ntlm_mic_public_key_nonce_and_midstream_formats_are_rejected() {
    for wrapped in [false, true] {
        for mutation in [
            Mutation::NtlmMic,
            Mutation::PublicKey,
            Mutation::Nonce,
            Mutation::TokenFormat,
        ] {
            assert!(
                !handshake(wrapped, "remote-user", PASSWORD, mutation),
                "wrapped={wrapped} mutation={mutation:?}"
            );
        }
    }
}

#[test]
fn mandatory_spnego_mic_cannot_be_omitted_or_tampered() {
    for mutation in [Mutation::SpnegoMic, Mutation::MissingSpnegoMic] {
        assert!(
            !handshake(true, "remote-user", PASSWORD, mutation),
            "{mutation:?}"
        );
    }
}

#[test]
fn authenticate_flags_cannot_bypass_ntlm_mic_validation() {
    for wrapped in [false, true] {
        assert!(
            !handshake(
                wrapped,
                "remote-user",
                PASSWORD,
                Mutation::NtlmMicAndTargetInfoFlag
            ),
            "wrapped={wrapped}"
        );
    }
}

#[test]
fn request_size_limit_applies_before_parsing_or_reassembly() {
    use ironrdp_pdu::PduHint as _;

    let configured = identity("remote-user", PASSWORD);
    let mut server = CredsspSequence::init(
        &configured,
        ServerName::new("synthetic-client"),
        PUBLIC_KEY.to_vec(),
        None,
    )
    .unwrap();
    assert!(server
        .decode_client_message(&vec![0; super::MAX_CREDSSP_MESSAGE + 1])
        .is_err());
    let hint = super::CredsspTsRequestHint;
    assert!(hint.find_size(&[0x30, 0x82, 0xf0, 0x01]).is_err());
    assert_eq!(hint.find_size(&[0x30, 0x82]).unwrap(), None);
}

mod delegation;
