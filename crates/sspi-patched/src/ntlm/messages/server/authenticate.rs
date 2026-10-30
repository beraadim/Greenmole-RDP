use std::io::{self, Read};

use byteorder::{LittleEndian, ReadBytesExt};

use crate::crypto::compute_md5_channel_bindings_hash;
use crate::ntlm::messages::av_pair::{AV_PAIR_CHANNEL_BINDINGS, AvPair, MsvAvFlags};
use crate::ntlm::messages::computations::*;
use crate::ntlm::messages::{MessageFields, MessageTypes, read_ntlm_header, try_read_version};
use crate::ntlm::{
    AuthIdentityBuffers, AuthenticateMessage, ChannelBindings, ENCRYPTED_RANDOM_SESSION_KEY_SIZE,
    MESSAGE_INTEGRITY_CHECK_SIZE, Mic, NegotiateFlags, Ntlm, NtlmState,
};
use crate::{SecurityStatus, Utf16String, Utf16StringExt};

const HEADER_SIZE: usize = 64;

struct AuthenticateMessageFields {
    workstation: MessageFields,
    domain_name: MessageFields,
    encrypted_random_session_key: MessageFields,
    user_name: MessageFields,
    lm_challenge_response: MessageFields,
    nt_challenge_response: MessageFields,
}

pub(crate) fn read_authenticate(context: &mut Ntlm, mut stream: impl Read) -> crate::Result<SecurityStatus> {
    check_state(context.state)?;

    let mut buffer = Vec::with_capacity(HEADER_SIZE);
    stream.read_to_end(&mut buffer)?;
    let mut buffer = io::Cursor::new(buffer);

    read_ntlm_header(&mut buffer, MessageTypes::Authenticate)?;
    let (mut message_fields, flags) = read_header(&mut buffer)?;
    context.flags = flags;
    let _version = try_read_version(context.flags, &mut buffer)?;
    let mic_offset = u8::try_from(buffer.position())
        .map_err(|_| crate::Error::new(crate::ErrorKind::InvalidToken, "MIC offset exceeds u8"))?;
    read_payload(&mut message_fields, &mut buffer)?;
    let message = buffer.into_inner();

    let (authenticate_message, updated_identity) = process_message_fields(
        &context.identity,
        message_fields,
        mic_offset,
        message,
        &context.channel_bindings,
    )?;
    context.identity = Some(updated_identity);
    context.authenticate_message = Some(authenticate_message);

    context.state = NtlmState::Completion;

    Ok(SecurityStatus::CompleteNeeded)
}

fn check_state(state: NtlmState) -> crate::Result<()> {
    if state != NtlmState::Authenticate {
        Err(crate::Error::new(
            crate::ErrorKind::OutOfSequence,
            "Read authenticate was fired but the state is not an Authenticate",
        ))
    } else {
        Ok(())
    }
}

fn read_header(mut buffer: impl Read) -> crate::Result<(AuthenticateMessageFields, NegotiateFlags)> {
    let mut lm_challenge_response = MessageFields::new();
    let mut nt_challenge_response = MessageFields::new();
    let mut domain_name = MessageFields::new();
    let mut user_name = MessageFields::new();
    let mut workstation = MessageFields::new();
    let mut encrypted_random_session_key = MessageFields::new();

    lm_challenge_response.read_from(&mut buffer)?;
    nt_challenge_response.read_from(&mut buffer)?;
    domain_name.read_from(&mut buffer)?;
    user_name.read_from(&mut buffer)?;
    workstation.read_from(&mut buffer)?;
    encrypted_random_session_key.read_from(&mut buffer)?;
    let negotiate_flags =
        NegotiateFlags::from_bits(buffer.read_u32::<LittleEndian>()?).unwrap_or_else(NegotiateFlags::empty);

    let negotiate_key_exchange = negotiate_flags.contains(NegotiateFlags::NTLM_SSP_NEGOTIATE_KEY_EXCH);
    if negotiate_key_exchange && encrypted_random_session_key.buffer.is_empty()
        || !negotiate_key_exchange && !encrypted_random_session_key.buffer.is_empty()
    {
        return Err(crate::Error::new(
            crate::ErrorKind::InvalidToken,
            "Negotiate key exchange flag is set but encrypted random session key \
                 is empty or the flag is not set but the key is not empty",
        ));
    }

    if negotiate_key_exchange && encrypted_random_session_key.buffer.len() != ENCRYPTED_RANDOM_SESSION_KEY_SIZE {
        return Err(crate::Error::new(
            crate::ErrorKind::InvalidToken,
            "Invalid encrypted random session key",
        ));
    }

    let message_fields = AuthenticateMessageFields {
        workstation,
        domain_name,
        encrypted_random_session_key,
        user_name,
        lm_challenge_response,
        nt_challenge_response,
    };

    Ok((message_fields, negotiate_flags))
}

fn read_payload<T>(message_fields: &mut AuthenticateMessageFields, buffer: &mut io::Cursor<T>) -> crate::Result<()>
where
    io::Cursor<T>: Read + io::Seek,
{
    message_fields.domain_name.read_buffer_from_cursor(buffer)?;
    message_fields.user_name.read_buffer_from_cursor(buffer)?;
    message_fields.workstation.read_buffer_from_cursor(buffer)?;
    message_fields.lm_challenge_response.read_buffer_from_cursor(buffer)?;
    message_fields.nt_challenge_response.read_buffer_from_cursor(buffer)?;
    message_fields
        .encrypted_random_session_key
        .read_buffer_from_cursor(buffer)?;

    Ok(())
}

fn process_message_fields(
    identity: &Option<AuthIdentityBuffers>,
    message_fields: AuthenticateMessageFields,
    mic_offset: u8,
    authenticate_message: Vec<u8>,
    channel_bindings: &Option<ChannelBindings>,
) -> crate::Result<(AuthenticateMessage, AuthIdentityBuffers)> {
    if message_fields.nt_challenge_response.buffer.is_empty() {
        return Err(crate::Error::new(
            crate::ErrorKind::InvalidToken,
            "NtChallengeResponse cannot be empty",
        ));
    }

    let (target_info, _) = read_ntlm_v2_response(message_fields.nt_challenge_response.buffer.as_ref())?;

    let av_pairs = AvPair::buffer_to_av_pairs(target_info.as_ref())?;

    // MS-NLMP 3.2.5.1.2 declares MIC presence through MsvAvFlags in the
    // NTLMv2 response, including when other AV flag bits are unknown.
    // NEGOTIATE_TARGET_INFO describes the CHALLENGE's TargetInfo field; it
    // cannot determine whether an AUTHENTICATE message contains a MIC.
    let mic_required = av_pairs
        .iter()
        .any(|pair| matches!(pair, AvPair::Flags(flags) if *flags & MsvAvFlags::MESSAGE_INTEGRITY_CHECK.bits() != 0));
    let mic = read_mic(&message_fields, mic_offset, &authenticate_message, mic_required)?;

    if let Some(AvPair::ChannelBindings(hash)) = av_pairs
        .iter()
        .find(|av_pair| av_pair.as_u16() == AV_PAIR_CHANNEL_BINDINGS)
        && let Some(channel_bindings) = channel_bindings.as_ref()
        && compute_md5_channel_bindings_hash(channel_bindings)? != *hash
    {
        return Err(crate::Error::new(
            crate::ErrorKind::BadBindings,
            "Channel bindings hash mismatch",
        ));
    }

    // will not set workstation because it is not used anywhere

    let encrypted_random_session_key: Option<[u8; ENCRYPTED_RANDOM_SESSION_KEY_SIZE]> = match message_fields
        .encrypted_random_session_key
        .buffer
        .len()
    {
        0 => None,
        ENCRYPTED_RANDOM_SESSION_KEY_SIZE => {
            let mut encrypted_random_session_key = [0x00; ENCRYPTED_RANDOM_SESSION_KEY_SIZE];
            encrypted_random_session_key.clone_from_slice(message_fields.encrypted_random_session_key.buffer.as_ref());
            Some(encrypted_random_session_key)
        }
        key_len => {
            return Err(crate::Error::new(
                crate::ErrorKind::InvalidToken,
                format!(
                    "Encrypted random session key has wrong length. Expected {ENCRYPTED_RANDOM_SESSION_KEY_SIZE} bytes, got {key_len} bytes."
                ),
            ));
        }
    };

    let mut identity = if let Some(identity) = identity {
        identity.clone()
    } else {
        AuthIdentityBuffers::default()
    };

    if !message_fields.user_name.buffer.is_empty() {
        identity.user = Utf16String::from_bytes_le(message_fields.user_name.buffer)?;
    }
    if !message_fields.domain_name.buffer.is_empty() {
        identity.domain = Utf16String::from_bytes_le(message_fields.domain_name.buffer)?;
    }

    Ok((
        AuthenticateMessage::new(authenticate_message, mic, encrypted_random_session_key),
        identity,
    ))
}

fn read_mic(
    fields: &AuthenticateMessageFields,
    mic_offset: u8,
    message: &[u8],
    required: bool,
) -> crate::Result<Option<Mic>> {
    let header_end = usize::from(mic_offset);
    let payload_start = if required {
        header_end + MESSAGE_INTEGRITY_CHECK_SIZE
    } else {
        header_end
    };
    let minimum_payload_offset = u32::try_from(payload_start)?;
    // A declared MIC must occupy its own complete field before payload data.
    // Ignoring zero-length buffers permits their protocol-defined placeholder
    // offsets, without allowing a missing MIC to consume the first payload.
    for field in [
        &fields.domain_name,
        &fields.user_name,
        &fields.workstation,
        &fields.lm_challenge_response,
        &fields.nt_challenge_response,
        &fields.encrypted_random_session_key,
    ] {
        if !field.buffer.is_empty() && field.buffer_offset < minimum_payload_offset {
            return Err(crate::Error::new(
                crate::ErrorKind::InvalidToken,
                "NTLM authentication payload overlaps its header or declared MIC",
            ));
        }
    }
    if !required {
        return Ok(None);
    }
    let value = message
        .get(header_end..payload_start)
        .ok_or_else(|| crate::Error::new(crate::ErrorKind::InvalidToken, "NTLM authentication MIC is truncated"))?;
    let mut mic_value = [0; MESSAGE_INTEGRITY_CHECK_SIZE];
    mic_value.copy_from_slice(value);
    Ok(Some(Mic::new(mic_value, mic_offset)))
}
