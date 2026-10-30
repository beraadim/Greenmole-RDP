//! Synthetic wire credentials and in-memory streams only: no listener, native
//! desktop, real credentials, or operating-system authentication is involved.
use super::*;
use ironrdp_connector::{encode_x224_packet, Sequence as _};
use ironrdp_core::WriteBuf;
use ironrdp_pdu::gcc;
use ironrdp_pdu::nego::SecurityProtocol;
use ironrdp_pdu::rdp::client_info::{
    AddressFamily, ClientInfo, ClientInfoFlags, CompressionType, ExtendedClientInfo,
    ExtendedClientOptionalInfo,
};
use ironrdp_pdu::rdp::headers::{BasicSecurityHeader, BasicSecurityHeaderFlags};

fn authentication_test_server() -> RdpServer {
    RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_no_input()
        .with_no_display()
        .build()
}

const SIZE: DesktopSize = DesktopSize {
    width: 800,
    height: 600,
};

fn credentials(password: &str) -> Credentials {
    Credentials {
        username: "fixture-user".into(),
        password: password.into(),
        domain: None,
    }
}

fn negotiated_acceptor(protocol: SecurityProtocol) -> Acceptor {
    let mut acceptor = Acceptor::new(
        SecurityProtocol::SSL | SecurityProtocol::HYBRID,
        SIZE,
        Vec::new(),
        Some(credentials("fixture-correct")),
    );
    let request = encode_vec(&X224(nego::ConnectionRequest {
        nego_data: None,
        flags: nego::RequestFlags::empty(),
        protocol,
    }))
    .unwrap();
    acceptor.step(&request, &mut WriteBuf::new()).unwrap();
    let mut response = WriteBuf::new();
    acceptor.step(&[], &mut response).unwrap();
    let X224(nego::ConnectionConfirm::Response {
        protocol: selected, ..
    }) = decode(response.filled()).unwrap()
    else {
        panic!("expected negotiation response");
    };
    assert_eq!(selected, protocol);
    assert!(acceptor.reached_security_upgrade().is_some());
    // The fixture starts after a TLS upgrade; no real TLS socket is required to
    // exercise the negotiated CredSSP state or the actual ClientInfo verifier.
    acceptor.mark_security_upgrade_as_done();
    acceptor
}

fn advance_to_client_info(acceptor: &mut Acceptor) {
    let initial = mcs::ConnectInitial::with_gcc_blocks(gcc::ClientGccBlocks {
        core: gcc::ClientCoreData {
            version: gcc::RdpVersion::V5_PLUS,
            desktop_width: SIZE.width,
            desktop_height: SIZE.height,
            color_depth: gcc::ColorDepth::Bpp8,
            sec_access_sequence: gcc::SecureAccessSequence::Del,
            keyboard_layout: 0x0409,
            client_build: 1,
            client_name: "authentication-fixture".into(),
            keyboard_type: gcc::KeyboardType::IbmEnhanced,
            keyboard_subtype: 0,
            keyboard_functional_keys_count: 12,
            ime_file_name: String::new(),
            optional_data: gcc::ClientCoreOptionalData::default(),
        },
        security: gcc::ClientSecurityData {
            encryption_methods: gcc::EncryptionMethod::empty(),
            ext_encryption_methods: 0,
        },
        network: None,
        cluster: None,
        monitor: None,
        message_channel: None,
        multi_transport_channel: None,
        monitor_extended: None,
    })
    .unwrap();
    let mut packet = WriteBuf::new();
    encode_x224_packet(&initial, &mut packet).unwrap();
    acceptor
        .step(packet.filled(), &mut WriteBuf::new())
        .unwrap();
    acceptor.step(&[], &mut WriteBuf::new()).unwrap();
    for packet in [
        encode_vec(&X224(mcs::ErectDomainPdu {
            sub_height: 0,
            sub_interval: 0,
        }))
        .unwrap(),
        encode_vec(&X224(mcs::AttachUserRequest)).unwrap(),
    ] {
        acceptor.step(&packet, &mut WriteBuf::new()).unwrap();
    }
    acceptor.step(&[], &mut WriteBuf::new()).unwrap();
    for channel_id in [1002, 1003] {
        let packet = encode_vec(&X224(mcs::ChannelJoinRequest {
            initiator_id: 1002,
            channel_id,
        }))
        .unwrap();
        acceptor.step(&packet, &mut WriteBuf::new()).unwrap();
        acceptor.step(&[], &mut WriteBuf::new()).unwrap();
    }
    acceptor.step(&[], &mut WriteBuf::new()).unwrap();
    assert_eq!(acceptor.state().name(), "SecureSettingsExchange");
}

fn client_info_packet(password: &str) -> Vec<u8> {
    let info = rdp::ClientInfoPdu {
        security_header: BasicSecurityHeader {
            flags: BasicSecurityHeaderFlags::INFO_PKT,
        },
        client_info: ClientInfo {
            credentials: credentials(password),
            code_page: 0,
            flags: ClientInfoFlags::UNICODE,
            compression_type: CompressionType::K8,
            alternate_shell: String::new(),
            work_dir: String::new(),
            extra_info: ExtendedClientInfo {
                address_family: AddressFamily::INET,
                address: "127.0.0.1".into(),
                dir: String::new(),
                optional_data: ExtendedClientOptionalInfo::default(),
            },
        },
    };
    encode_vec(&X224(SendDataRequest {
        initiator_id: 1002,
        channel_id: 1003,
        user_data: encode_vec(&info).unwrap().into(),
    }))
    .unwrap()
}

async fn tls_credssp_noop(server: &mut RdpServer, acceptor: &mut Acceptor) {
    let (stream, _peer) = tokio::io::duplex(64);
    let mut framed = TokioFramed::new(stream);
    let negotiated_nla = acceptor.should_perform_credssp();
    assert!(!negotiated_nla);
    server
        .await_credssp_authentication(negotiated_nla, async {
            ironrdp_acceptor::accept_credssp(
                &mut framed,
                acceptor,
                &mut ironrdp_tokio::reqwest::ReqwestNetworkClient::new(),
                "authentication-fixture".into(),
                Vec::new(),
                None,
            )
            .await
            .map_err(Into::into)
        })
        .await
        .unwrap();
    assert!(
        !server.connection_authenticated,
        "a TLS-only CredSSP no-op is not authentication"
    );
}

#[tokio::test(start_paused = true)]
async fn tls_fallback_bad_password_retains_authentication_failures_and_triggers_backoff() {
    let mut server = authentication_test_server();
    let peer = "127.0.0.1".parse().unwrap();
    for failure in 0..6 {
        let mut acceptor = negotiated_acceptor(SecurityProtocol::SSL);
        tls_credssp_noop(&mut server, &mut acceptor).await;
        advance_to_client_info(&mut acceptor);
        let packet = client_info_packet(if failure == 0 { "" } else { "fixture-wrong" });
        let result = server
            .await_connection_stage("RDP activation", async {
                acceptor
                    .step(&packet, &mut WriteBuf::new())
                    .map(|_| ())
                    .map_err(Into::into)
            })
            .await;
        assert!(result.is_err());
        server.record_authentication_result(peer, &result);
        assert!(!server.connection_authenticated);
        assert_eq!(
            server.gfx_state.lock().unwrap().connection_outcome,
            Some(ConnectionOutcome::AuthenticationFailed)
        );
    }
    assert_eq!(
        server
            .admission
            .retry_after(peer, tokio::time::Instant::now()),
        Some(Duration::from_millis(250))
    );
}

struct RejectAfterAuthentication;

impl RdpServerInputHandler for RejectAfterAuthentication {
    fn prepare_connection(&mut self) -> Result<()> {
        bail!("fixture post-authentication rejection")
    }
    fn keyboard(&mut self, _: crate::KeyboardEvent) {}
    fn mouse(&mut self, _: crate::MouseEvent) {}
}

#[tokio::test(start_paused = true)]
async fn tls_fallback_valid_password_waits_for_client_acceptance_then_clears_prior_failures() {
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_no_security()
        .with_input_handler(RejectAfterAuthentication)
        .with_no_display()
        .build();
    let peer = "127.0.0.1".parse().unwrap();
    for _ in 0..6 {
        server.admission.failed(peer, tokio::time::Instant::now());
    }
    assert!(server
        .admission
        .retry_after(peer, tokio::time::Instant::now())
        .is_some());
    let mut acceptor = negotiated_acceptor(SecurityProtocol::SSL);
    tls_credssp_noop(&mut server, &mut acceptor).await;
    advance_to_client_info(&mut acceptor);
    acceptor
        .step(&client_info_packet("fixture-correct"), &mut WriteBuf::new())
        .unwrap();
    assert_eq!(acceptor.state().name(), "LicensingExchange");
    assert!(!server.connection_authenticated);
    // ClientInfo validation above is real. Supply the later activation result
    // directly so this state test does not need display/capability negotiation.
    let activation = AcceptorResult {
        static_channels: StaticChannelSet::new(),
        capabilities: Vec::new(),
        input_events: Vec::new(),
        user_channel_id: 1002,
        io_channel_id: 1003,
        reactivation: false,
        client_name: "authentication-fixture".into(),
        keyboard_layout: 0x0409,
        keyboard_subtype: 0,
        client_desktop_size: None,
    };
    let (stream, _peer) = tokio::io::duplex(64);
    let (mut reader, mut writer) = split_tokio_framed(TokioFramed::new(stream));
    let result = server
        .client_accepted(&mut reader, &mut writer, activation, SIZE)
        .await
        .map(|_| ());
    assert!(
        result.is_err(),
        "the fixture deliberately stops before desktop initialization"
    );
    assert!(server.connection_authenticated);
    server.record_authentication_result(peer, &result);
    assert!(server
        .admission
        .retry_after(peer, tokio::time::Instant::now())
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn negotiated_nla_only_authenticates_after_success_and_preserves_failure_accounting() {
    for success in [false, true] {
        let mut server = authentication_test_server();
        let peer = "127.0.0.1".parse().unwrap();
        for _ in 0..5 {
            server.admission.failed(peer, tokio::time::Instant::now());
        }
        let mut acceptor = negotiated_acceptor(SecurityProtocol::HYBRID);
        let negotiated_nla = acceptor.should_perform_credssp();
        assert!(negotiated_nla);
        // Cryptographic CredSSP interoperability is tested in the acceptor;
        // here deterministic completion isolates the server's admission policy.
        let result = server
            .await_credssp_authentication(negotiated_nla, async {
                if !success {
                    bail!("fixture failed NLA proof");
                }
                acceptor.mark_credssp_as_done();
                Ok(())
            })
            .await;
        assert_eq!(server.connection_authenticated, success);
        server.record_authentication_result(peer, &result);
        assert_eq!(
            server
                .admission
                .retry_after(peer, tokio::time::Instant::now())
                .is_none(),
            success
        );
    }
}
