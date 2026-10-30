use super::*;
use crate::ntlm::{AuthenticateMessage, ChallengeMessage, NegotiateMessage};
use crate::{AuthIdentity, ErrorKind, Username};

// Published Microsoft MS-NLMP 4.2.4 NTLMv2 validation values, independent
// of this implementation's response generator. The response has no MIC.
// https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nlmp/125f7a94-933e-4023-a146-a449e49bf774
// https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-nlmp/54973495-20d2-49e8-9925-c399a403ed4a
const RESPONSE_KEY: [u8; 16] = [
    0x0c, 0x86, 0x8a, 0x40, 0x3b, 0xfd, 0x7a, 0x93, 0xa3, 0x00, 0x1e, 0xf2, 0x2e, 0xf0, 0x2e, 0x3f,
];
const SERVER_CHALLENGE: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
const SESSION_BASE_KEY: [u8; 16] = [
    0x8d, 0xe4, 0x0c, 0xca, 0xdb, 0xc1, 0x4a, 0x82, 0xf1, 0x5c, 0xb0, 0xad, 0x0d, 0xe9, 0x5c, 0xa3,
];
const NT_RESPONSE: [u8; 84] = [
    0x68, 0xcd, 0x0a, 0xb8, 0x51, 0xe5, 0x1c, 0x96, 0xaa, 0xbc, 0x92, 0x7b, 0xeb, 0xef, 0x6a, 0x1c, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa,
    0xaa, 0xaa, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x0c, 0x00, 0x44, 0x00, 0x6f, 0x00, 0x6d, 0x00, 0x61, 0x00, 0x69,
    0x00, 0x6e, 0x00, 0x01, 0x00, 0x0c, 0x00, 0x53, 0x00, 0x65, 0x00, 0x72, 0x00, 0x76, 0x00, 0x65, 0x00, 0x72, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn context(password: &str, response: &[u8]) -> Ntlm {
    let identity: AuthIdentityBuffers = AuthIdentity {
        username: Username::parse("Domain\\User").unwrap(),
        password: password.to_owned().into(),
    }
    .into();
    let mut context = Ntlm::new();
    context.identity = Some(identity.clone());
    context.allowed_identities = Some(vec![identity]);
    context.state = NtlmState::Completion;
    context.negotiate_message = Some(NegotiateMessage::new(vec![1, 2, 3]));
    context.challenge_message = Some(ChallengeMessage::new(
        vec![4, 5, 6],
        Vec::new(),
        SERVER_CHALLENGE,
        // Deliberately differs from the received blob timestamp. Verification
        // must use the authenticated original blob, never reconstruct it.
        u64::MAX,
    ));
    let mut wire = vec![0; 64];
    wire[20..22].copy_from_slice(&u16::try_from(response.len()).unwrap().to_le_bytes());
    wire[24..28].copy_from_slice(&64u32.to_le_bytes());
    wire.extend_from_slice(response);
    context.authenticate_message = Some(AuthenticateMessage::new(wire, None, None));
    context
}

#[test]
fn microsoft_ntlmv2_vector_derives_published_session_key() {
    assert_eq!(
        verify_ntlm_v2_response(&NT_RESPONSE, &SERVER_CHALLENGE, &RESPONSE_KEY).unwrap(),
        SESSION_BASE_KEY
    );
    let mut context = context("Password", &NT_RESPONSE);
    assert_eq!(complete_authenticate(&mut context).unwrap(), SecurityStatus::Ok);
    assert_eq!(context.session_key.unwrap(), SESSION_BASE_KEY);
}

#[test]
fn wrong_password_and_tampered_proof_are_rejected_without_mic() {
    let mut wrong_password = context("Wrong synthetic password", &NT_RESPONSE);
    assert_eq!(
        complete_authenticate(&mut wrong_password).unwrap_err().error_type,
        ErrorKind::LogonDenied
    );
    assert!(wrong_password.session_key.is_none());
    assert_eq!(wrong_password.state, NtlmState::Completion);

    let mut response = NT_RESPONSE;
    response[0] ^= 1;
    let mut tampered_proof = context("Password", &response);
    assert_eq!(
        complete_authenticate(&mut tampered_proof).unwrap_err().error_type,
        ErrorKind::LogonDenied
    );
    assert!(tampered_proof.session_key.is_none());
}

#[test]
fn every_received_response_byte_and_server_challenge_are_authenticated() {
    for index in 0..NT_RESPONSE.len() {
        let mut response = NT_RESPONSE;
        response[index] ^= 1;
        assert!(
            verify_ntlm_v2_response(&response, &SERVER_CHALLENGE, &RESPONSE_KEY).is_err(),
            "byte {index}"
        );
    }
    for index in 0..SERVER_CHALLENGE.len() {
        let mut challenge = SERVER_CHALLENGE;
        challenge[index] ^= 1;
        assert!(verify_ntlm_v2_response(&NT_RESPONSE, &challenge, &RESPONSE_KEY).is_err());
    }
}

#[test]
fn malformed_security_buffers_and_truncated_responses_fail_closed() {
    assert!(nt_challenge_response(&[]).is_err());
    let mut wire = vec![0; 64];
    wire[20..22].copy_from_slice(&84u16.to_le_bytes());
    wire[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(nt_challenge_response(&wire).is_err());
    wire[24..28].copy_from_slice(&64u32.to_le_bytes());
    wire.extend_from_slice(&NT_RESPONSE[..NT_RESPONSE.len() - 1]);
    assert!(nt_challenge_response(&wire).is_err());
    for length in 0..44 {
        assert!(verify_ntlm_v2_response(&NT_RESPONSE[..length], &SERVER_CHALLENGE, &RESPONSE_KEY).is_err());
    }
}
