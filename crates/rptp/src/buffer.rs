use crate::message::SequenceId;
use crate::port::DomainNumber;
use crate::port::PortIdentity;
use crate::result::{ParseError, ProtocolError, Result};
use crate::time::LogMessageInterval;

use bitflags::bitflags;

const PTP_TRANSPORT_SPECIFIC_OFFSET: usize = 0;

pub(crate) const PTP_HEADER_LEN: usize = 34;
pub(crate) const PTP_LENGTH_RANGE: core::ops::Range<usize> = 2..4;
pub(crate) const PTP_MSG_TYPE_OFFSET: usize = 0;
pub(crate) const PTP_VERSION_OFFSET: usize = 1;
pub(crate) const PTP_DOMAIN_NUMBER_OFFSET: usize = 4;
pub(crate) const PTP_FLAGS_RANGE: core::ops::Range<usize> = 6..8;
pub(crate) const PTP_SOURCE_PORT_IDENTITY_RANGE: core::ops::Range<usize> = 20..30;
pub(crate) const PTP_SEQUENCE_ID_RANGE: core::ops::Range<usize> = 30..32;
pub(crate) const PTP_CONTROL_FIELD_OFFSET: usize = 32;
pub(crate) const PTP_LOG_INTERVAL_OFFSET: usize = 33;
pub(crate) const PTP_PAYLOAD_OFFSET: usize = 34;

pub struct UnvalidatedMessage<'a> {
    buf: &'a [u8],
}

impl<'a> UnvalidatedMessage<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn length_checked_v2(self) -> Result<LengthCheckedMessage<'a>> {
        if self.buf.len() < PTP_HEADER_LEN {
            return Err(ParseError::HeaderTooShort {
                found: self.buf.len(),
            }
            .into());
        }

        let v = PtpVersion(self.buf[PTP_VERSION_OFFSET] & 0x0F);
        if v != PtpVersion::V2 {
            return Err(ProtocolError::UnsupportedPtpVersion(v.as_u8()).into());
        }

        let expected = u16::from_be_bytes(
            self.buf[PTP_LENGTH_RANGE]
                .try_into()
                .map_err(|_| ParseError::PayloadTooShort {
                    field: "PTP length header",
                    expected: PTP_LENGTH_RANGE.len(),
                    found: self.buf[PTP_LENGTH_RANGE].len(),
                })?,
        ) as usize;
        let found = self.buf.len();

        if expected != found {
            return Err(ParseError::LengthMismatch {
                declared: expected,
                actual: found,
            }
            .into());
        }

        Ok(LengthCheckedMessage::new(self.buf))
    }
}

pub struct LengthCheckedMessage<'a> {
    buf: &'a [u8],
}

impl<'a> LengthCheckedMessage<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub(crate) fn buf(&self) -> &'a [u8] {
        self.buf
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportSpecific;

impl TransportSpecific {
    pub const fn new() -> Self {
        Self
    }

    pub const fn to_wire(self) -> u8 {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtpVersion(u8);

impl PtpVersion {
    pub const V2: Self = Self(2);

    pub const fn as_u8(self) -> u8 {
        self.0 & 0x0F
    }
}

pub struct MessageBuffer {
    buf: [u8; 2048],
}

impl MessageBuffer {
    pub fn new(
        transport_specific: TransportSpecific,
        version: PtpVersion,
        domain_number: DomainNumber,
        source_port_identity: PortIdentity,
    ) -> Self {
        let mut buf = [0u8; 2048];
        buf[PTP_TRANSPORT_SPECIFIC_OFFSET] = (transport_specific.to_wire() & 0x0F) << 4;
        buf[PTP_VERSION_OFFSET] = version.as_u8() & 0x0F;
        buf[PTP_DOMAIN_NUMBER_OFFSET] = domain_number.as_u8();
        buf[PTP_SOURCE_PORT_IDENTITY_RANGE]
            .copy_from_slice(source_port_identity.to_bytes().as_ref());

        Self { buf }
    }

    pub fn typed<'a>(
        &'a mut self,
        msg_type: MessageType,
        control: ControlField,
    ) -> TypedBuffer<'a> {
        self.buf[PTP_MSG_TYPE_OFFSET] =
            (self.buf[PTP_MSG_TYPE_OFFSET] & 0xF0) | msg_type.to_nibble();
        self.buf[PTP_CONTROL_FIELD_OFFSET] = control as u8;
        TypedBuffer { buf: &mut self.buf }
    }
}

pub struct TypedBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> TypedBuffer<'a> {
    pub fn flagged(self, flags: MessageFlags) -> FlaggedBuffer<'a> {
        self.buf[PTP_FLAGS_RANGE].copy_from_slice(&flags.bits().to_be_bytes());
        FlaggedBuffer { buf: self.buf }
    }
}

pub struct FlaggedBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> FlaggedBuffer<'a> {
    pub fn sequenced(self, sequence_id: SequenceId) -> SequencedBuffer<'a> {
        self.buf[PTP_SEQUENCE_ID_RANGE].copy_from_slice(&sequence_id.to_be_bytes());
        SequencedBuffer { buf: self.buf }
    }
}

pub struct SequencedBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> SequencedBuffer<'a> {
    pub fn with_log_message_interval(
        self,
        log_message_interval: LogMessageInterval,
    ) -> LogMessageIntervalBuffer<'a> {
        self.buf[PTP_LOG_INTERVAL_OFFSET] = log_message_interval.as_u8();
        LogMessageIntervalBuffer { buf: self.buf }
    }
}

pub struct LogMessageIntervalBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> LogMessageIntervalBuffer<'a> {
    pub fn payload(self) -> PayloadBuffer<'a> {
        PayloadBuffer { buf: self.buf }
    }
}

pub struct PayloadBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> PayloadBuffer<'a> {
    pub fn buf(&mut self) -> &mut [u8] {
        &mut self.buf[PTP_PAYLOAD_OFFSET..]
    }

    pub fn finalize(self, payload_len: u16) -> FinalizedBuffer<'a> {
        let payload_len = payload_len as usize;
        let total_len = (PTP_PAYLOAD_OFFSET + payload_len) as u16;
        self.buf[PTP_LENGTH_RANGE].copy_from_slice(&total_len.to_be_bytes());

        FinalizedBuffer {
            buf: &mut self.buf[..total_len as usize],
        }
    }
}

pub struct FinalizedBuffer<'a> {
    buf: &'a mut [u8],
}

impl AsRef<[u8]> for FinalizedBuffer<'_> {
    fn as_ref(&self) -> &[u8] {
        self.buf
    }
}

pub enum MessageType {
    Sync = 0x00,
    DelayRequest = 0x01,
    FollowUp = 0x08,
    DelayResponse = 0x09,
    Announce = 0x0B,
}

impl MessageType {
    pub fn to_nibble(self) -> u8 {
        self as u8 & 0x0F
    }

    pub fn from_nibble(n: u8) -> Result<Self> {
        match n {
            0x00 => Ok(MessageType::Sync),
            0x01 => Ok(MessageType::DelayRequest),
            0x08 => Ok(MessageType::FollowUp),
            0x09 => Ok(MessageType::DelayResponse),
            0x0B => Ok(MessageType::Announce),
            _ => Err(ProtocolError::UnknownMessageType(n).into()),
        }
    }
}

pub enum ControlField {
    Sync = 0x00,
    DelayRequest = 0x01,
    FollowUp = 0x02,
    DelayResponse = 0x03,
    Management = 0x04,
    Other = 0x05,
}

bitflags! {
    #[derive(Default)]
    pub struct MessageFlags: u16 {
        const TWO_STEP = 0x0002;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{ForeignClockDS, Priority1, Priority2};
    use crate::clock::{ClockIdentity, ClockQuality, StepsRemoved};
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::port::PortNumber;
    use crate::result::{Error, ParseError, ProtocolError};
    use crate::time::TimeStamp;

    #[test]
    fn buffer_integrity_two_step_sync() {
        let msg = TwoStepSyncMessage::new(7.into());
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = msg.serialize(&mut buf);
        let bytes = wire.as_ref();
        let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len());
        assert_eq!(bytes[0] & 0x0F, 0x00);
        assert_eq!(bytes[32], 0x00);
    }

    #[test]
    fn buffer_integrity_follow_up() {
        let msg = FollowUpMessage::new(9.into(), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = msg.serialize(&mut buf);
        let bytes = wire.as_ref();
        let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len());
        assert_eq!(bytes[0] & 0x0F, 0x08);
        assert_eq!(bytes[32], 0x02);
    }

    #[test]
    fn buffer_integrity_delay_resp() {
        let msg = DelayResponseMessage::new(11.into(), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = msg.serialize(&mut buf);
        let bytes = wire.as_ref();
        let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len());
        assert_eq!(bytes[0] & 0x0F, 0x09);
        assert_eq!(bytes[32], 0x03);
    }

    #[test]
    fn buffer_integrity_delay_req() {
        let msg = DelayRequestMessage::new(13.into());
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = msg.serialize(&mut buf);
        let bytes = wire.as_ref();
        let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len());
        assert_eq!(bytes[0] & 0x0F, 0x01);
        assert_eq!(bytes[32], 0x01);
    }

    #[test]
    fn buffer_integrity_announce() {
        let msg = AnnounceMessage::new(
            21.into(),
            LogMessageInterval::new(0x7F),
            ForeignClockDS::new(
                ClockIdentity::new(&[0; 8]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
                StepsRemoved::new(0),
            ),
        );
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = msg.serialize(&mut buf);
        let bytes = wire.as_ref();
        let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len());
        assert_eq!(bytes[0] & 0x0F, 0x0B);
        assert_eq!(bytes[32], 0x05);
    }

    #[test]
    fn unvalidated_message_header_too_short() {
        let buf = [0u8; PTP_HEADER_LEN - 1];
        let res = UnvalidatedMessage::new(&buf).length_checked_v2();

        match res {
            Err(e) => assert_eq!(
                e,
                Error::Parse(ParseError::HeaderTooShort {
                    found: PTP_HEADER_LEN - 1
                })
            ),
            Ok(_) => panic!("expected HeaderTooShort error"),
        }
    }

    #[test]
    fn unvalidated_message_unsupported_version() {
        let mut buf = [0u8; PTP_HEADER_LEN];
        buf[PTP_VERSION_OFFSET] = 1; // not V2
        buf[PTP_LENGTH_RANGE]
            .copy_from_slice(&(PTP_HEADER_LEN as u16).to_be_bytes());

        let res = UnvalidatedMessage::new(&buf).length_checked_v2();

        match res {
            Err(e) => assert_eq!(
                e,
                Error::Protocol(ProtocolError::UnsupportedPtpVersion(1))
            ),
            Ok(_) => panic!("expected UnsupportedPtpVersion error"),
        }
    }

    #[test]
    fn unvalidated_message_length_mismatch() {
        let mut buf = [0u8; PTP_HEADER_LEN + 1];
        buf[PTP_VERSION_OFFSET] = PtpVersion::V2.as_u8();
        buf[PTP_LENGTH_RANGE]
            .copy_from_slice(&(PTP_HEADER_LEN as u16).to_be_bytes());

        let res = UnvalidatedMessage::new(&buf).length_checked_v2();

        match res {
            Err(e) => assert_eq!(
                e,
                Error::Parse(ParseError::LengthMismatch {
                    declared: PTP_HEADER_LEN,
                    actual: PTP_HEADER_LEN + 1,
                })
            ),
            Ok(_) => panic!("expected LengthMismatch error"),
        }
    }

    #[test]
    fn message_type_from_nibble_unknown() {
        let res = MessageType::from_nibble(0x07);

        match res {
            Err(e) => assert_eq!(
                e,
                Error::Protocol(ProtocolError::UnknownMessageType(0x07))
            ),
            Ok(_) => panic!("expected UnknownMessageType error"),
        }
    }
}
