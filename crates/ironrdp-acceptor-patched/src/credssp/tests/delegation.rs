use super::*;

fn seal(context: &mut sspi::credssp::SspiContext, plaintext: &[u8]) -> Vec<u8> {
    use sspi::{BufferType, EncryptionFlags, SecurityBufferRef, Sspi as _};
    let mut signature = [0; 1024];
    let mut data = plaintext.to_vec();
    let mut buffers = [
        SecurityBufferRef::token_buf(&mut signature),
        SecurityBufferRef::data_buf(&mut data),
    ];
    context
        .encrypt_message(EncryptionFlags::empty(), &mut buffers)
        .unwrap();
    let mut sealed = SecurityBufferRef::find_buffer(&buffers, BufferType::Token)
        .unwrap()
        .data()
        .to_vec();
    sealed.extend_from_slice(
        SecurityBufferRef::find_buffer(&buffers, BufferType::Data)
            .unwrap()
            .data(),
    );
    sealed
}

fn delegated_identity_handshake(username: &str, password: &str) -> bool {
    use sspi::credssp::{write_ts_credentials, SspiContext};
    use sspi::{
        BufferType, ClientRequestFlags, CredentialUse, CredentialsBuffers, DataRepresentation,
        Ntlm, SecurityBuffer, SecurityBufferRef, Sspi as _,
    };

    let configured = identity("remote-user", PASSWORD);
    let mut server = CredsspSequence::init(
        &configured,
        ServerName::new("synthetic-client"),
        PUBLIC_KEY.to_vec(),
        None,
    )
    .unwrap();
    let mut client = SspiContext::Ntlm(Ntlm::default());
    let credentials = Credentials::AuthIdentity(configured.clone());
    let mut handle = client
        .acquire_credentials_handle()
        .with_auth_data(&credentials)
        .with_credential_use(CredentialUse::Outbound)
        .execute(&mut client)
        .unwrap()
        .credentials_handle;
    let mut input = [SecurityBuffer::new(Vec::new(), BufferType::Token)];
    let mut output = [SecurityBuffer::new(Vec::new(), BufferType::Token)];
    for step in 0..2 {
        let mut builder = client
            .initialize_security_context()
            .with_credentials_handle(&mut handle)
            .with_context_requirements(
                ClientRequestFlags::MUTUAL_AUTH
                    | ClientRequestFlags::USE_SESSION_KEY
                    | ClientRequestFlags::INTEGRITY
                    | ClientRequestFlags::CONFIDENTIALITY,
            )
            .with_target_data_representation(DataRepresentation::Native)
            .with_target_name("TERMSRV/127.0.0.1")
            .with_input(&mut input)
            .with_output(&mut output);
        client
            .initialize_security_context_sync(&mut builder)
            .unwrap();
        // Version 2 is used only by this fixture to exercise the public NTLM
        // sealing API without copying production CredSSP hashing code.
        let request = TsRequest {
            version: 2,
            nego_tokens: Some(std::mem::take(&mut output[0].buffer)),
            pub_key_auth: (step == 1).then(|| seal(&mut client, PUBLIC_KEY)),
            ..TsRequest::default()
        };
        let (finished, response) = server_step(&mut server, request).unwrap();
        assert!(
            !finished,
            "binding alone must never authenticate the session"
        );
        let response = response.unwrap();
        if step == 0 {
            input[0].buffer = response.nego_tokens.unwrap();
        } else {
            let mut binding = response.pub_key_auth.unwrap();
            let (signature, data) = binding.split_at_mut(16);
            let mut buffers = [
                SecurityBufferRef::token_buf(signature),
                SecurityBufferRef::data_buf(data),
            ];
            client.decrypt_message(&mut buffers).unwrap();
            let echoed = SecurityBufferRef::find_buffer(&buffers, BufferType::Data)
                .unwrap()
                .data();
            let mut expected = PUBLIC_KEY.to_vec();
            expected[0] += 1;
            assert_eq!(echoed, expected);
        }
    }
    let delegated =
        CredentialsBuffers::try_from(Credentials::AuthIdentity(identity(username, password)))
            .unwrap();
    let plaintext = write_ts_credentials(&delegated, CredSspMode::WithCredentials).unwrap();
    let request = TsRequest {
        version: 2,
        auth_info: Some(seal(&mut client, &plaintext)),
        ..TsRequest::default()
    };
    matches!(server_step(&mut server, request), Ok((true, None)))
}

#[test]
fn valid_ntlm_and_binding_cannot_delegate_different_credentials() {
    assert!(delegated_identity_handshake("remote-user", PASSWORD));
    assert!(delegated_identity_handshake(
        "CLIENT-PC\\remote-user",
        PASSWORD
    ));
    assert!(!delegated_identity_handshake("different-user", PASSWORD));
    assert!(!delegated_identity_handshake(
        "remote-user@other.test",
        PASSWORD
    ));
    assert!(!delegated_identity_handshake("", PASSWORD));
    assert!(!delegated_identity_handshake("remote-user", ""));
    assert!(!delegated_identity_handshake(
        "remote-user",
        "different-password"
    ));
}

#[test]
fn delegated_credentials_before_binding_cannot_finish_authentication() {
    let configured = identity("remote-user", PASSWORD);
    let mut server = CredsspSequence::init(
        &configured,
        ServerName::new("synthetic-client"),
        PUBLIC_KEY.to_vec(),
        None,
    )
    .unwrap();
    assert!(server_step(
        &mut server,
        TsRequest {
            auth_info: Some(vec![0; 32]),
            ..TsRequest::default()
        }
    )
    .is_err());
}
