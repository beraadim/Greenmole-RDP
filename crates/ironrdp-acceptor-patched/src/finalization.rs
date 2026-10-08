use ironrdp_connector::{ConnectorError, ConnectorErrorExt as _, ConnectorResult, Sequence, State, Written};
use ironrdp_core::WriteBuf;
use ironrdp_pdu::rdp;
use ironrdp_pdu::x224::X224;
use tracing::debug;

use crate::util::{self, wrap_share_data};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_channel_messages_do_not_advance_finalization() {
        let packet = ironrdp_core::encode_vec(&X224(ironrdp_pdu::mcs::SendDataRequest {
            initiator_id: 1002,
            channel_id: 1004,
            user_data: vec![4, 0, 0, 0, 3, 0, 0, 0, 0x50, 0, 1, 0].into(),
        }))
        .unwrap();
        for state in [
            FinalizationState::WaitSynchronize,
            FinalizationState::WaitControlCooperate,
            FinalizationState::WaitRequestControl,
            FinalizationState::WaitFontList,
        ] {
            let expected_state = state.name();
            let mut sequence = FinalizationSequence::new(1002, 1003);
            sequence.state = state;
            assert_eq!(sequence.step(&packet, &mut WriteBuf::new()).unwrap(), Written::Nothing);
            assert_eq!(sequence.state().name(), expected_state);
            assert_eq!(sequence.into_input_events(), [packet.clone()]);
        }
    }

    #[test]
    fn activation_backlog_is_bounded_by_both_count_and_bytes() {
        let mut backlog = Vec::new();
        for _ in 0..256 {
            defer_input_pdu(&mut backlog, &[1]).unwrap();
        }
        assert!(defer_input_pdu(&mut backlog, &[1]).is_err());
        let mut backlog = Vec::new();
        defer_input_pdu(&mut backlog, &vec![0; 1024 * 1024]).unwrap();
        assert!(defer_input_pdu(&mut backlog, &[1]).is_err());
    }
}

/// Bound activation backlog so an uncooperative authenticated client cannot
/// grow memory indefinitely while delaying the expected handshake messages.
pub(crate) fn defer_input_pdu(backlog: &mut Vec<Vec<u8>>, input: &[u8]) -> ConnectorResult<()> {
    const MAX_PDUS: usize = 256;
    const MAX_BYTES: usize = 1024 * 1024;
    if backlog.len() >= MAX_PDUS || backlog.iter().map(Vec::len).sum::<usize>().saturating_add(input.len()) > MAX_BYTES
    {
        return Err(ConnectorError::general("activation input backlog exceeded"));
    }
    backlog.push(input.to_vec());
    Ok(())
}

#[derive(Debug)]
pub struct FinalizationSequence {
    state: FinalizationState,
    user_channel_id: u16,
    io_channel_id: u16,

    input_events: Vec<Vec<u8>>,
}

#[derive(Default, Debug)]
pub enum FinalizationState {
    #[default]
    Consumed,

    WaitSynchronize,
    WaitControlCooperate,
    WaitRequestControl,
    WaitFontList,

    SendSynchronizeConfirm,
    SendControlCooperateConfirm,
    SendGrantedControlConfirm,
    SendFontMap,

    Finished,
}

impl State for FinalizationState {
    fn name(&self) -> &'static str {
        match self {
            Self::Consumed => "Consumed",
            Self::WaitSynchronize => "WaitSynchronize",
            Self::WaitControlCooperate => "WaitControlCooperate",
            Self::WaitRequestControl => "WaitRequestControl",
            Self::WaitFontList => "WaitFontList",
            Self::SendSynchronizeConfirm => "SendSynchronizeConfirm",
            Self::SendControlCooperateConfirm => "SendControlCooperateConfirm",
            Self::SendGrantedControlConfirm => "SendGrantedControlConfirm",
            Self::SendFontMap => "SendFontMap",
            Self::Finished => "Finished",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl Sequence for FinalizationSequence {
    fn next_pdu_hint(&self) -> Option<&dyn ironrdp_pdu::PduHint> {
        match &self.state {
            FinalizationState::Consumed => None,
            FinalizationState::WaitSynchronize => Some(&ironrdp_pdu::X224Hint),
            FinalizationState::WaitControlCooperate => Some(&ironrdp_pdu::X224Hint),
            FinalizationState::WaitRequestControl => Some(&ironrdp_pdu::X224Hint),
            FinalizationState::WaitFontList => Some(&ironrdp_pdu::RdpHint),
            FinalizationState::SendSynchronizeConfirm => None,
            FinalizationState::SendControlCooperateConfirm => None,
            FinalizationState::SendGrantedControlConfirm => None,
            FinalizationState::SendFontMap => None,
            FinalizationState::Finished => None,
        }
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn step(&mut self, input: &[u8], output: &mut WriteBuf) -> ConnectorResult<Written> {
        if !input.is_empty() {
            if let Ok(X224(ironrdp_pdu::mcs::McsMessage::SendDataRequest(data))) = ironrdp_core::decode(input) {
                if data.channel_id != self.io_channel_id {
                    // Dynamic channels survive reactivation. Their messages
                    // must not consume a Synchronize/Control/FontList step.
                    defer_input_pdu(&mut self.input_events, input)?;
                    return Ok(Written::Nothing);
                }
            }
        }
        let (written, next_state) = match core::mem::take(&mut self.state) {
            FinalizationState::WaitSynchronize => {
                let synchronize = decode_share_control(input);

                debug!(message = ?synchronize, "Received");

                (Written::Nothing, FinalizationState::WaitControlCooperate)
            }

            FinalizationState::WaitControlCooperate => {
                let cooperate = decode_share_control(input);

                debug!(message = ?cooperate, "Received");

                (Written::Nothing, FinalizationState::WaitRequestControl)
            }

            FinalizationState::WaitRequestControl => {
                let control = decode_share_control(input)?;

                debug!(message = ?control, "Received");

                (Written::Nothing, FinalizationState::WaitFontList)
            }

            FinalizationState::WaitFontList => match decode_font_list(input) {
                Ok(font_list) => {
                    debug!(message = ?font_list, "Received");

                    (Written::Nothing, FinalizationState::SendSynchronizeConfirm)
                }

                Err(()) => {
                    defer_input_pdu(&mut self.input_events, input)?;

                    (Written::Nothing, FinalizationState::WaitFontList)
                }
            },

            FinalizationState::SendSynchronizeConfirm => {
                let synchronize_confirm = create_synchronize_confirm();

                debug!(message = ?synchronize_confirm, "Send");

                let share_data = wrap_share_data(synchronize_confirm, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (
                    Written::from_size(written)?,
                    FinalizationState::SendControlCooperateConfirm,
                )
            }

            FinalizationState::SendControlCooperateConfirm => {
                let cooperate_confirm = create_cooperate_confirm();

                debug!(message = ?cooperate_confirm, "Send");

                let share_data = wrap_share_data(cooperate_confirm, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (
                    Written::from_size(written)?,
                    FinalizationState::SendGrantedControlConfirm,
                )
            }

            FinalizationState::SendGrantedControlConfirm => {
                let control_confirm = create_control_confirm(self.user_channel_id);

                debug!(message = ?control_confirm, "Send");

                let share_data = wrap_share_data(control_confirm, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (Written::from_size(written)?, FinalizationState::SendFontMap)
            }

            FinalizationState::SendFontMap => {
                let font_map = create_font_map();

                debug!(message = ?font_map, "Send");

                let share_data = wrap_share_data(font_map, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (Written::from_size(written)?, FinalizationState::Finished)
            }

            _ => unreachable!(),
        };

        self.state = next_state;
        Ok(written)
    }
}

impl FinalizationSequence {
    pub fn new(user_channel_id: u16, io_channel_id: u16) -> Self {
        Self {
            state: FinalizationState::WaitSynchronize,
            user_channel_id,
            io_channel_id,
            input_events: Vec::new(),
        }
    }

    pub fn into_input_events(self) -> Vec<Vec<u8>> {
        self.input_events
    }

    pub fn is_done(&self) -> bool {
        self.state.is_terminal()
    }
}

fn create_synchronize_confirm() -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::Synchronize(rdp::finalization_messages::SynchronizePdu { target_user_id: 0 })
}

fn create_cooperate_confirm() -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::Control(rdp::finalization_messages::ControlPdu {
        action: rdp::finalization_messages::ControlAction::Cooperate,
        grant_id: 0,
        control_id: 0,
    })
}

fn create_control_confirm(user_id: u16) -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::Control(rdp::finalization_messages::ControlPdu {
        action: rdp::finalization_messages::ControlAction::GrantedControl,
        grant_id: user_id,
        control_id: u32::from(rdp::capability_sets::SERVER_CHANNEL_ID),
    })
}

fn create_font_map() -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::FontMap(rdp::finalization_messages::FontPdu::default())
}

fn decode_share_control(input: &[u8]) -> ConnectorResult<rdp::headers::ShareControlHeader> {
    let data_request = ironrdp_core::decode::<X224<ironrdp_pdu::mcs::SendDataRequest<'_>>>(input)
        .map_err(ConnectorError::decode)
        .map(|p| p.0)?;
    let share_control = ironrdp_core::decode::<rdp::headers::ShareControlHeader>(data_request.user_data.as_ref())
        .map_err(ConnectorError::decode)?;
    Ok(share_control)
}

fn decode_font_list(input: &[u8]) -> Result<rdp::finalization_messages::FontPdu, ()> {
    use ironrdp_pdu::rdp::headers::{ShareControlPdu, ShareDataPdu};

    let share_control = decode_share_control(input).map_err(|_| ())?;

    let ShareControlPdu::Data(data_pdu) = share_control.share_control_pdu else {
        return Err(());
    };

    let ShareDataPdu::FontList(font_pdu) = data_pdu.share_data_pdu else {
        return Err(());
    };

    Ok(font_pdu)
}
