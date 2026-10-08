use core::mem;

use ironrdp_connector::{
    encode_x224_packet, general_err, reason_err, ConnectorError, ConnectorErrorExt as _, ConnectorResult, DesktopSize,
    Sequence, State, Written,
};
use ironrdp_core::{decode, WriteBuf};
use ironrdp_pdu as pdu;
use ironrdp_pdu::nego::SecurityProtocol;
use ironrdp_pdu::x224::X224;
use ironrdp_svc::{StaticChannelSet, SvcServerProcessor};
use pdu::rdp::capability_sets::CapabilitySet;
use pdu::rdp::client_info::Credentials;
use pdu::rdp::headers::ShareControlPdu;
use pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode, ServerSetErrorInfoPdu};
use pdu::rdp::server_license::{LicensePdu, LicensingErrorMessage};
use pdu::{gcc, mcs, nego, rdp};
use tracing::{debug, warn};

use super::channel_connection::ChannelConnectionSequence;
use super::finalization::FinalizationSequence;
use crate::util::{self, wrap_share_data};

const IO_CHANNEL_ID: u16 = 1003;
const USER_CHANNEL_ID: u16 = 1002;

/// TLS-only connections must provide the configured username and password.
/// Keep domain handling unchanged: clients may qualify a local account with
/// different computer/domain names. An empty password is never a bypass.
fn non_hybrid_credentials_match(configured: Option<&Credentials>, supplied: &Credentials) -> bool {
    configured.is_some_and(|expected| expected.username == supplied.username && expected.password == supplied.password)
}

pub struct Acceptor {
    pub(crate) state: AcceptorState,
    security: SecurityProtocol,
    io_channel_id: u16,
    user_channel_id: u16,
    desktop_size: DesktopSize,
    server_capabilities: Vec<CapabilitySet>,
    static_channels: StaticChannelSet,
    saved_for_reactivation: AcceptorState,
    pub(crate) creds: Option<Credentials>,
    reactivation: bool,
    client_name: String,
    keyboard_layout: u32,
    client_desktop_size: Option<DesktopSize>,
    deferred_channel_pdus: Vec<Vec<u8>>,
}

#[derive(Debug)]
pub struct AcceptorResult {
    pub static_channels: StaticChannelSet,
    pub capabilities: Vec<CapabilitySet>,
    pub input_events: Vec<Vec<u8>>,
    pub user_channel_id: u16,
    pub io_channel_id: u16,
    pub reactivation: bool,
    pub client_name: String,
    pub keyboard_layout: u32,
    /// Original size from Client Core Data. Confirm Active may already echo
    /// the server's Demand Active size, especially with FreeRDP clients.
    pub client_desktop_size: Option<DesktopSize>,
}

impl Acceptor {
    pub fn new(
        security: SecurityProtocol,
        desktop_size: DesktopSize,
        capabilities: Vec<CapabilitySet>,
        creds: Option<Credentials>,
    ) -> Self {
        Self {
            security,
            state: AcceptorState::InitiationWaitRequest,
            user_channel_id: USER_CHANNEL_ID,
            io_channel_id: IO_CHANNEL_ID,
            desktop_size,
            server_capabilities: capabilities,
            static_channels: StaticChannelSet::new(),
            saved_for_reactivation: Default::default(),
            creds,
            reactivation: false,
            client_name: String::new(),
            keyboard_layout: 0,
            client_desktop_size: None,
            deferred_channel_pdus: Vec::new(),
        }
    }

    pub fn new_deactivation_reactivation(
        mut consumed: Acceptor,
        static_channels: StaticChannelSet,
        desktop_size: DesktopSize,
    ) -> ConnectorResult<Self> {
        let AcceptorState::CapabilitiesSendServer {
            early_capability,
            channels,
        } = consumed.saved_for_reactivation
        else {
            return Err(general_err!("invalid acceptor state"));
        };

        for cap in consumed.server_capabilities.iter_mut() {
            if let CapabilitySet::Bitmap(cap) = cap {
                cap.desktop_width = desktop_size.width;
                cap.desktop_height = desktop_size.height;
            }
        }
        let state = AcceptorState::CapabilitiesSendServer {
            early_capability,
            channels: channels.clone(),
        };
        let saved_for_reactivation = AcceptorState::CapabilitiesSendServer {
            early_capability,
            channels,
        };
        Ok(Self {
            security: consumed.security,
            state,
            user_channel_id: consumed.user_channel_id,
            io_channel_id: consumed.io_channel_id,
            desktop_size,
            server_capabilities: consumed.server_capabilities,
            static_channels,
            saved_for_reactivation,
            creds: consumed.creds,
            reactivation: true,
            client_name: consumed.client_name,
            keyboard_layout: consumed.keyboard_layout,
            client_desktop_size: consumed.client_desktop_size,
            deferred_channel_pdus: Vec::new(),
        })
    }

    pub fn attach_static_channel<T>(&mut self, channel: T)
    where
        T: SvcServerProcessor + 'static,
    {
        self.static_channels.insert(channel);
    }

    pub fn reached_security_upgrade(&self) -> Option<SecurityProtocol> {
        match self.state {
            AcceptorState::SecurityUpgrade { .. } => Some(self.security),
            _ => None,
        }
    }

    /// # Panics
    ///
    /// Panics if state is not [AcceptorState::SecurityUpgrade].
    pub fn mark_security_upgrade_as_done(&mut self) {
        assert!(self.reached_security_upgrade().is_some());
        self.step(&[], &mut WriteBuf::new()).expect("transition to next state");
        debug_assert!(self.reached_security_upgrade().is_none());
    }

    pub fn should_perform_credssp(&self) -> bool {
        matches!(self.state, AcceptorState::Credssp { .. })
    }

    /// # Panics
    ///
    /// Panics if state is not [AcceptorState::Credssp].
    pub fn mark_credssp_as_done(&mut self) {
        assert!(self.should_perform_credssp());
        let res = self.step(&[], &mut WriteBuf::new()).expect("transition to next state");
        debug_assert!(!self.should_perform_credssp());
        assert_eq!(res, Written::Nothing);
    }

    pub fn get_result(&mut self) -> Option<AcceptorResult> {
        match mem::take(&mut self.state) {
            AcceptorState::Accepted {
                channels: _channels, // TODO: what about ChannelDef?
                client_capabilities,
                input_events,
            } => Some(AcceptorResult {
                static_channels: mem::take(&mut self.static_channels),
                capabilities: client_capabilities,
                input_events: {
                    let mut deferred = mem::take(&mut self.deferred_channel_pdus);
                    deferred.extend(input_events);
                    deferred
                },
                user_channel_id: self.user_channel_id,
                io_channel_id: self.io_channel_id,
                reactivation: self.reactivation,
                client_name: self.client_name.clone(),
                keyboard_layout: self.keyboard_layout,
                client_desktop_size: self.client_desktop_size,
            }),
            previous_state => {
                self.state = previous_state;
                None
            }
        }
    }
}

#[derive(Default, Debug)]
pub enum AcceptorState {
    #[default]
    Consumed,

    InitiationWaitRequest,
    InitiationSendConfirm {
        requested_protocol: SecurityProtocol,
    },
    SecurityUpgrade {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
    },
    Credssp {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
    },
    BasicSettingsWaitInitial {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
    },
    BasicSettingsSendResponse {
        requested_protocol: SecurityProtocol,
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, Option<gcc::ChannelDef>)>,
    },
    ChannelConnection {
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
        connection: ChannelConnectionSequence,
    },
    RdpSecurityCommencement {
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    SecureSettingsExchange {
        protocol: SecurityProtocol,
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    LicensingExchange {
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    CapabilitiesSendServer {
        early_capability: Option<gcc::ClientEarlyCapabilityFlags>,
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    MonitorLayoutSend {
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    CapabilitiesWaitConfirm {
        channels: Vec<(u16, gcc::ChannelDef)>,
    },
    ConnectionFinalization {
        finalization: FinalizationSequence,
        channels: Vec<(u16, gcc::ChannelDef)>,
        client_capabilities: Vec<CapabilitySet>,
    },
    Accepted {
        channels: Vec<(u16, gcc::ChannelDef)>,
        client_capabilities: Vec<CapabilitySet>,
        input_events: Vec<Vec<u8>>,
    },
}

impl State for AcceptorState {
    fn name(&self) -> &'static str {
        match self {
            Self::Consumed => "Consumed",
            Self::InitiationWaitRequest => "InitiationWaitRequest",
            Self::InitiationSendConfirm { .. } => "InitiationSendConfirm",
            Self::SecurityUpgrade { .. } => "SecurityUpgrade",
            Self::Credssp { .. } => "Credssp",
            Self::BasicSettingsWaitInitial { .. } => "BasicSettingsWaitInitial",
            Self::BasicSettingsSendResponse { .. } => "BasicSettingsSendResponse",
            Self::ChannelConnection { .. } => "ChannelConnection",
            Self::RdpSecurityCommencement { .. } => "RdpSecurityCommencement",
            Self::SecureSettingsExchange { .. } => "SecureSettingsExchange",
            Self::LicensingExchange { .. } => "LicensingExchange",
            Self::CapabilitiesSendServer { .. } => "CapabilitiesSendServer",
            Self::MonitorLayoutSend { .. } => "MonitorLayoutSend",
            Self::CapabilitiesWaitConfirm { .. } => "CapabilitiesWaitConfirm",
            Self::ConnectionFinalization { .. } => "ConnectionFinalization",
            Self::Accepted { .. } => "Connected",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl Sequence for Acceptor {
    fn next_pdu_hint(&self) -> Option<&dyn pdu::PduHint> {
        match &self.state {
            AcceptorState::Consumed => None,
            AcceptorState::InitiationWaitRequest => Some(&pdu::X224_HINT),
            AcceptorState::InitiationSendConfirm { .. } => None,
            AcceptorState::SecurityUpgrade { .. } => None,
            AcceptorState::Credssp { .. } => None,
            AcceptorState::BasicSettingsWaitInitial { .. } => Some(&pdu::X224_HINT),
            AcceptorState::BasicSettingsSendResponse { .. } => None,
            AcceptorState::ChannelConnection { connection, .. } => connection.next_pdu_hint(),
            AcceptorState::RdpSecurityCommencement { .. } => None,
            AcceptorState::SecureSettingsExchange { .. } => Some(&pdu::X224_HINT),
            AcceptorState::LicensingExchange { .. } => None,
            AcceptorState::CapabilitiesSendServer { .. } => None,
            AcceptorState::MonitorLayoutSend { .. } => None,
            AcceptorState::CapabilitiesWaitConfirm { .. } => Some(&pdu::X224_HINT),
            AcceptorState::ConnectionFinalization { finalization, .. } => finalization.next_pdu_hint(),
            AcceptorState::Accepted { .. } => None,
        }
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn step(&mut self, input: &[u8], output: &mut WriteBuf) -> ConnectorResult<Written> {
        let prev_state = mem::take(&mut self.state);

        let (written, next_state) = match prev_state {
            AcceptorState::InitiationWaitRequest => {
                let connection_request = decode::<X224<nego::ConnectionRequest>>(input)
                    .map_err(ConnectorError::decode)
                    .map(|p| p.0)?;

                debug!(message = ?connection_request, "Received");

                (
                    Written::Nothing,
                    AcceptorState::InitiationSendConfirm {
                        requested_protocol: connection_request.protocol,
                    },
                )
            }

            AcceptorState::InitiationSendConfirm { requested_protocol } => {
                let protocols = requested_protocol & self.security;
                let protocol = if protocols.intersects(SecurityProtocol::HYBRID_EX) {
                    SecurityProtocol::HYBRID_EX
                } else if protocols.intersects(SecurityProtocol::HYBRID) {
                    SecurityProtocol::HYBRID
                } else if protocols.intersects(SecurityProtocol::SSL) {
                    SecurityProtocol::SSL
                } else if self.security.is_empty() {
                    SecurityProtocol::empty()
                } else {
                    return Err(ConnectorError::general("failed to negotiate security protocol"));
                };
                let connection_confirm = nego::ConnectionConfirm::Response {
                    flags: nego::ResponseFlags::empty(),
                    protocol,
                };

                debug!(message = ?connection_confirm, "Send");

                let written =
                    ironrdp_core::encode_buf(&X224(connection_confirm), output).map_err(ConnectorError::encode)?;

                (
                    Written::from_size(written)?,
                    AcceptorState::SecurityUpgrade {
                        requested_protocol,
                        protocol,
                    },
                )
            }

            AcceptorState::SecurityUpgrade {
                requested_protocol,
                protocol,
            } => {
                debug!(?requested_protocol);
                let next_state = if protocol.intersects(SecurityProtocol::HYBRID | SecurityProtocol::HYBRID_EX) {
                    AcceptorState::Credssp {
                        requested_protocol,
                        protocol,
                    }
                } else {
                    AcceptorState::BasicSettingsWaitInitial {
                        requested_protocol,
                        protocol,
                    }
                };
                (Written::Nothing, next_state)
            }

            AcceptorState::Credssp {
                requested_protocol,
                protocol,
            } => (
                Written::Nothing,
                AcceptorState::BasicSettingsWaitInitial {
                    requested_protocol,
                    protocol,
                },
            ),

            AcceptorState::BasicSettingsWaitInitial {
                requested_protocol,
                protocol,
            } => {
                let x224_payload = decode::<X224<pdu::x224::X224Data<'_>>>(input)
                    .map_err(ConnectorError::decode)
                    .map(|p| p.0)?;
                let settings_initial =
                    decode::<mcs::ConnectInitial>(x224_payload.data.as_ref()).map_err(ConnectorError::decode)?;

                debug!(message = ?settings_initial, "Received");

                let gcc_blocks = settings_initial.conference_create_request.into_gcc_blocks();
                self.client_name = gcc_blocks.core.client_name.clone();
                self.keyboard_layout = gcc_blocks.core.keyboard_layout;
                self.client_desktop_size = Some(DesktopSize {
                    width: gcc_blocks.core.desktop_width,
                    height: gcc_blocks.core.desktop_height,
                });
                let early_capability = gcc_blocks.core.optional_data.early_capability_flags;

                let joined: Vec<_> = gcc_blocks
                    .network
                    .map(|network| {
                        network
                            .channels
                            .into_iter()
                            .map(|c| {
                                self.static_channels
                                    .get_by_channel_name(&c.name)
                                    .map(|(type_id, _)| (type_id, c))
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                #[expect(clippy::arithmetic_side_effects)] // IO channel ID is not big enough for overflowing.
                let channels = joined
                    .into_iter()
                    .enumerate()
                    .map(|(i, channel)| {
                        let channel_id = u16::try_from(i).expect("always in the range") + self.io_channel_id + 1;
                        if let Some((type_id, c)) = channel {
                            self.static_channels.attach_channel_id(type_id, channel_id);
                            (channel_id, Some(c))
                        } else {
                            (channel_id, None)
                        }
                    })
                    .collect();

                (
                    Written::Nothing,
                    AcceptorState::BasicSettingsSendResponse {
                        requested_protocol,
                        protocol,
                        early_capability,
                        channels,
                    },
                )
            }

            AcceptorState::BasicSettingsSendResponse {
                requested_protocol,
                protocol,
                early_capability,
                channels,
            } => {
                let channel_ids: Vec<u16> = channels.iter().map(|&(i, _)| i).collect();

                let skip_channel_join = early_capability
                    .is_some_and(|client| client.contains(gcc::ClientEarlyCapabilityFlags::SUPPORT_SKIP_CHANNELJOIN));

                let server_blocks = create_gcc_blocks(
                    self.io_channel_id,
                    channel_ids.clone(),
                    requested_protocol,
                    skip_channel_join,
                );

                let settings_response = mcs::ConnectResponse {
                    conference_create_response: gcc::ConferenceCreateResponse::new(self.user_channel_id, server_blocks)
                        .map_err(ConnectorError::decode)?,
                    called_connect_id: 1,
                    domain_parameters: mcs::DomainParameters::target(),
                };

                debug!(message = ?settings_response, "Send");

                let written = encode_x224_packet(&settings_response, output)?;
                let channels = channels.into_iter().filter_map(|(i, c)| c.map(|c| (i, c))).collect();

                (
                    Written::from_size(written)?,
                    AcceptorState::ChannelConnection {
                        protocol,
                        early_capability,
                        channels,
                        connection: if skip_channel_join {
                            ChannelConnectionSequence::skip_channel_join(self.user_channel_id)
                        } else {
                            ChannelConnectionSequence::new(self.user_channel_id, self.io_channel_id, channel_ids)
                        },
                    },
                )
            }

            AcceptorState::ChannelConnection {
                protocol,
                early_capability,
                channels,
                mut connection,
            } => {
                let written = connection.step(input, output)?;
                let state = if connection.is_done() {
                    AcceptorState::RdpSecurityCommencement {
                        protocol,
                        early_capability,
                        channels,
                    }
                } else {
                    AcceptorState::ChannelConnection {
                        protocol,
                        early_capability,
                        channels,
                        connection,
                    }
                };

                (written, state)
            }

            AcceptorState::RdpSecurityCommencement {
                protocol,
                early_capability,
                channels,
                ..
            } => (
                Written::Nothing,
                AcceptorState::SecureSettingsExchange {
                    protocol,
                    early_capability,
                    channels,
                },
            ),

            AcceptorState::SecureSettingsExchange {
                protocol,
                early_capability,
                channels,
            } => {
                let data: X224<mcs::SendDataRequest<'_>> = decode(input).map_err(ConnectorError::decode)?;
                let data = data.0;
                let client_info: rdp::ClientInfoPdu =
                    decode(data.user_data.as_ref()).map_err(ConnectorError::decode)?;

                // ClientInfoPdu contains plaintext credentials; never format it.
                debug!(?protocol, "Received client info");

                if !protocol.intersects(SecurityProtocol::HYBRID | SecurityProtocol::HYBRID_EX) {
                    let creds = client_info.client_info.credentials;

                    debug!(
                        credentials_configured = self.creds.is_some(),
                        protocol = ?protocol,
                        "Checking non-hybrid credentials"
                    );

                    // Some clients initially send an empty password. Reject that
                    // attempt unless it actually matches the configured account;
                    // prompting behavior must never bypass authentication.
                    if !non_hybrid_credentials_match(self.creds.as_ref(), &creds) {
                        warn!(?protocol, "Credential mismatch; rejecting connection");
                        let info = ServerSetErrorInfoPdu(ErrorInfo::ProtocolIndependentCode(
                            ProtocolIndependentCode::ServerDeniedConnection,
                        ));

                        debug!(message = ?info, "Send");

                        util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &info, output)?;

                        return Err(ConnectorError::general("invalid credentials"));
                    }
                    debug!("Credentials accepted");
                }

                (
                    Written::Nothing,
                    AcceptorState::LicensingExchange {
                        early_capability,
                        channels,
                    },
                )
            }

            AcceptorState::LicensingExchange {
                early_capability,
                channels,
            } => {
                let license: LicensePdu = LicensingErrorMessage::new_valid_client()
                    .map_err(ConnectorError::encode)?
                    .into();

                debug!(message = ?license, "Send");

                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &license, output)?;

                self.saved_for_reactivation = AcceptorState::CapabilitiesSendServer {
                    early_capability,
                    channels: channels.clone(),
                };

                (
                    Written::from_size(written)?,
                    AcceptorState::CapabilitiesSendServer {
                        early_capability,
                        channels,
                    },
                )
            }

            AcceptorState::CapabilitiesSendServer {
                early_capability,
                channels,
            } => {
                let demand_active = rdp::headers::ShareControlHeader {
                    share_id: 0,
                    pdu_source: self.io_channel_id,
                    share_control_pdu: ShareControlPdu::ServerDemandActive(rdp::capability_sets::ServerDemandActive {
                        pdu: rdp::capability_sets::DemandActive {
                            source_descriptor: "".into(),
                            capability_sets: self.server_capabilities.clone(),
                        },
                    }),
                };

                debug!(message = ?demand_active, "Send");

                let written = util::encode_send_data_indication(
                    self.user_channel_id,
                    self.io_channel_id,
                    &demand_active,
                    output,
                )?;

                let layout_flag = gcc::ClientEarlyCapabilityFlags::SUPPORT_MONITOR_LAYOUT_PDU;
                let next_state = if early_capability.is_some_and(|c| c.contains(layout_flag)) {
                    AcceptorState::MonitorLayoutSend { channels }
                } else {
                    AcceptorState::CapabilitiesWaitConfirm { channels }
                };

                (Written::from_size(written)?, next_state)
            }

            AcceptorState::MonitorLayoutSend { channels } => {
                let monitor_layout =
                    rdp::headers::ShareDataPdu::MonitorLayout(rdp::finalization_messages::MonitorLayoutPdu {
                        monitors: vec![gcc::Monitor {
                            left: 0,
                            top: 0,
                            // TS_MONITOR_DEF uses inclusive lower-right
                            // coordinates, unlike framebuffer dimensions.
                            right: i32::from(self.desktop_size.width) - 1,
                            bottom: i32::from(self.desktop_size.height) - 1,
                            flags: gcc::MonitorFlags::PRIMARY,
                        }],
                    });

                debug!(message = ?monitor_layout, "Send");

                let share_data = wrap_share_data(monitor_layout, self.io_channel_id);

                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (
                    Written::from_size(written)?,
                    AcceptorState::CapabilitiesWaitConfirm { channels },
                )
            }

            AcceptorState::CapabilitiesWaitConfirm { ref channels } => {
                let message = decode::<X224<mcs::McsMessage<'_>>>(input)
                    .map_err(ConnectorError::decode)
                    .map(|p| p.0);
                let message = match message {
                    Ok(msg) => msg,
                    Err(e) => {
                        if self.reactivation {
                            debug!("Dropping unexpected PDU during reactivation");
                            self.state = prev_state;
                            return Ok(Written::Nothing);
                        } else {
                            return Err(e);
                        }
                    }
                };
                match message {
                    mcs::McsMessage::SendDataRequest(data) => {
                        if data.channel_id != self.io_channel_id {
                            // DVC responses and browser resize requests can
                            // arrive while Demand Active is in flight. Keep
                            // them for the existing channel processors after
                            // activation rather than interpreting them as a
                            // Confirm Active PDU and dropping them.
                            super::finalization::defer_input_pdu(&mut self.deferred_channel_pdus, input)?;
                            self.state = prev_state;
                            return Ok(Written::Nothing);
                        }
                        let capabilities_confirm = decode::<rdp::headers::ShareControlHeader>(data.user_data.as_ref())
                            .map_err(ConnectorError::decode);
                        let capabilities_confirm = match capabilities_confirm {
                            Ok(capabilities_confirm) => capabilities_confirm,
                            Err(e) => {
                                if self.reactivation {
                                    debug!("Dropping unexpected PDU during reactivation");
                                    self.state = prev_state;
                                    return Ok(Written::Nothing);
                                } else {
                                    return Err(e);
                                }
                            }
                        };

                        debug!(message = ?capabilities_confirm, "Received");

                        let ShareControlPdu::ClientConfirmActive(confirm) = capabilities_confirm.share_control_pdu
                        else {
                            return Err(ConnectorError::general("expected client confirm active"));
                        };

                        (
                            Written::Nothing,
                            AcceptorState::ConnectionFinalization {
                                channels: channels.clone(),
                                finalization: FinalizationSequence::new(self.user_channel_id, self.io_channel_id),
                                client_capabilities: confirm.pdu.capability_sets,
                            },
                        )
                    }

                    mcs::McsMessage::DisconnectProviderUltimatum(ultimatum) => {
                        return Err(reason_err!("received disconnect ultimatum", "{:?}", ultimatum.reason))
                    }

                    _ => {
                        warn!(?message, "Unexpected MCS message received");

                        (Written::Nothing, prev_state)
                    }
                }
            }

            AcceptorState::ConnectionFinalization {
                mut finalization,
                channels,
                client_capabilities,
            } => {
                let written = finalization.step(input, output)?;

                let state = if finalization.is_done() {
                    AcceptorState::Accepted {
                        channels,
                        client_capabilities,
                        input_events: finalization.into_input_events(),
                    }
                } else {
                    AcceptorState::ConnectionFinalization {
                        finalization,
                        channels,
                        client_capabilities,
                    }
                };

                (written, state)
            }

            _ => unreachable!(),
        };

        self.state = next_state;
        Ok(written)
    }
}

fn create_gcc_blocks(
    io_channel: u16,
    channel_ids: Vec<u16>,
    requested: SecurityProtocol,
    skip_channel_join: bool,
) -> gcc::ServerGccBlocks {
    gcc::ServerGccBlocks {
        core: gcc::ServerCoreData {
            version: gcc::RdpVersion::V5_PLUS,
            optional_data: gcc::ServerCoreOptionalData {
                client_requested_protocols: Some(requested),
                early_capability_flags: skip_channel_join
                    .then_some(gcc::ServerEarlyCapabilityFlags::SKIP_CHANNELJOIN_SUPPORTED),
            },
        },
        security: gcc::ServerSecurityData::no_security(),
        network: gcc::ServerNetworkData {
            channel_ids,
            io_channel,
        },
        message_channel: None,
        multi_transport_channel: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_channel_response_is_replayed_after_confirm_active() {
        let mut acceptor = Acceptor::new(
            SecurityProtocol::SSL,
            DesktopSize {
                width: 1920,
                height: 1080,
            },
            Vec::new(),
            None,
        );
        acceptor.reactivation = true;
        acceptor.state = AcceptorState::CapabilitiesWaitConfirm { channels: Vec::new() };
        // SVC header plus a DRDYNVC capability response, received while the
        // server is waiting for the resize's Confirm Active message.
        let packet = ironrdp_core::encode_vec(&X224(mcs::SendDataRequest {
            initiator_id: USER_CHANNEL_ID,
            channel_id: IO_CHANNEL_ID + 1,
            user_data: vec![4, 0, 0, 0, 3, 0, 0, 0, 0x50, 0, 1, 0].into(),
        }))
        .unwrap();
        assert_eq!(acceptor.step(&packet, &mut WriteBuf::new()).unwrap(), Written::Nothing);
        assert!(matches!(acceptor.state, AcceptorState::CapabilitiesWaitConfirm { .. }));
        acceptor.state = AcceptorState::Accepted {
            channels: Vec::new(),
            client_capabilities: Vec::new(),
            input_events: vec![vec![0x04, 0x02]],
        };
        let result = acceptor.get_result().unwrap();
        assert_eq!(result.input_events, [packet, vec![0x04, 0x02]]);
    }

    #[test]
    fn initial_gcc_desktop_size_survives_acceptance_and_reactivation() {
        // This is the requested browser size, before FreeRDP overwrites its
        // Confirm Active dimensions with the server's Demand Active size.
        let requested = DesktopSize {
            width: 1537,
            height: 865,
        };
        let mut acceptor = Acceptor::new(
            SecurityProtocol::SSL,
            DesktopSize {
                width: 1920,
                height: 1080,
            },
            Vec::new(),
            None,
        );
        acceptor.state = AcceptorState::BasicSettingsWaitInitial {
            requested_protocol: SecurityProtocol::SSL,
            protocol: SecurityProtocol::SSL,
        };
        let initial = mcs::ConnectInitial::with_gcc_blocks(gcc::ClientGccBlocks {
            core: gcc::ClientCoreData {
                version: gcc::RdpVersion::V5_PLUS,
                desktop_width: requested.width,
                desktop_height: requested.height,
                color_depth: gcc::ColorDepth::Bpp8,
                sec_access_sequence: gcc::SecureAccessSequence::Del,
                keyboard_layout: 0x0409,
                client_build: 1,
                client_name: "viewport-test".into(),
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
        acceptor.step(packet.filled(), &mut WriteBuf::new()).unwrap();
        acceptor.saved_for_reactivation = AcceptorState::CapabilitiesSendServer {
            early_capability: None,
            channels: Vec::new(),
        };
        acceptor.state = AcceptorState::Accepted {
            channels: Vec::new(),
            client_capabilities: Vec::new(),
            input_events: Vec::new(),
        };
        let result = acceptor.get_result().unwrap();
        assert_eq!(result.client_desktop_size, Some(requested));
        let reactivated = Acceptor::new_deactivation_reactivation(
            acceptor,
            result.static_channels,
            DesktopSize {
                width: 1280,
                height: 720,
            },
        )
        .unwrap();
        assert_eq!(reactivated.client_desktop_size, Some(requested));
        assert!(reactivated.reactivation);
    }

    #[test]
    fn advertised_monitor_layout_matches_framebuffer_dimensions() {
        for size in [
            DesktopSize {
                width: 1536,
                height: 864,
            },
            DesktopSize {
                width: 3840,
                height: 2160,
            },
        ] {
            let mut acceptor = Acceptor::new(SecurityProtocol::SSL, size, Vec::new(), None);
            acceptor.state = AcceptorState::MonitorLayoutSend { channels: Vec::new() };
            let mut packet = WriteBuf::new();
            acceptor.step(&[], &mut packet).unwrap();
            let X224(mcs::McsMessage::SendDataIndication(data)) = decode(packet.filled()).unwrap() else {
                panic!("expected monitor layout indication")
            };
            let header: rdp::headers::ShareControlHeader = decode(&data.user_data).unwrap();
            let ShareControlPdu::Data(data) = header.share_control_pdu else {
                panic!("expected share data")
            };
            let rdp::headers::ShareDataPdu::MonitorLayout(layout) = data.share_data_pdu else {
                panic!("expected monitor layout")
            };
            assert_eq!(layout.monitors.len(), 1);
            let monitor = &layout.monitors[0];
            assert_eq!(monitor.right - monitor.left + 1, i32::from(size.width));
            assert_eq!(monitor.bottom - monitor.top + 1, i32::from(size.height));
        }
    }

    fn credentials(username: &str, password: &str, domain: Option<&str>) -> Credentials {
        Credentials {
            username: username.to_owned(),
            password: password.to_owned(),
            domain: domain.map(str::to_owned),
        }
    }

    #[test]
    fn empty_client_password_cannot_bypass_configured_credentials() {
        let configured = credentials("remote-user", "configured-secret", None);
        for username in ["remote-user", "another-user", ""] {
            let supplied = credentials(username, "", None);
            assert!(!non_hybrid_credentials_match(Some(&configured), &supplied));
        }
    }

    #[test]
    fn both_username_and_password_must_match_exactly() {
        let configured = credentials("remote-user", "configured-secret", None);
        for (username, password) in [
            ("another-user", "configured-secret"),
            ("REMOTE-USER", "configured-secret"),
            ("remote-user", "wrong-secret"),
            ("remote-user", "Configured-secret"),
            ("remote-user", "configured-secret "),
        ] {
            let supplied = credentials(username, password, None);
            assert!(!non_hybrid_credentials_match(Some(&configured), &supplied));
        }
        let supplied = credentials("remote-user", "configured-secret", None);
        assert!(non_hybrid_credentials_match(Some(&configured), &supplied));
    }

    #[test]
    fn domain_qualification_does_not_change_existing_account_matching() {
        let configured = credentials("remote-user", "configured-secret", Some("SERVER"));
        for domain in [None, Some("SERVER"), Some("CLIENT")] {
            let supplied = credentials("remote-user", "configured-secret", domain);
            assert!(non_hybrid_credentials_match(Some(&configured), &supplied));
        }
    }

    #[test]
    fn missing_server_credentials_still_reject_all_clients() {
        for supplied in [
            credentials("remote-user", "configured-secret", None),
            credentials("remote-user", "", None),
            credentials("", "", None),
        ] {
            assert!(!non_hybrid_credentials_match(None, &supplied));
        }
    }

    #[test]
    fn explicit_empty_password_still_requires_the_configured_username() {
        let configured = credentials("remote-user", "", None);
        assert!(non_hybrid_credentials_match(
            Some(&configured),
            &credentials("remote-user", "", None)
        ));
        assert!(!non_hybrid_credentials_match(
            Some(&configured),
            &credentials("another-user", "", None)
        ));
    }
}
