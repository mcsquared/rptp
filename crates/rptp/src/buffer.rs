use crate::message::SequenceId;
use crate::port::DomainNumber;
use crate::port::PortIdentity;

pub struct MessageBuffer {
    buf: [u8; 2048],
}

impl MessageBuffer {
    pub fn new(
        transport_specific: u8,
        version: u8,
        domain_number: DomainNumber,
        source_port_identity: PortIdentity,
        log_message_interval: i8,
    ) -> Self {
        let mut buf = [0u8; 2048];
        buf[0] = (transport_specific & 0x0F) << 4;
        buf[1] = version & 0x0F;
        buf[4] = domain_number.as_u8();
        buf[20..30].copy_from_slice(source_port_identity.to_bytes().as_ref());
        buf[33] = log_message_interval as u8;

        Self { buf }
    }

    pub fn typed<'a>(
        &'a mut self,
        msg_type: MessageType,
        control: ControlField,
    ) -> TypedBuffer<'a> {
        self.buf[0] = (self.buf[0] & 0xF0) | (msg_type as u8 & 0x0F);
        self.buf[32] = control as u8;
        TypedBuffer { buf: &mut self.buf }
    }
}

pub struct TypedBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> TypedBuffer<'a> {
    pub fn flagged(self, flags: u16) -> FlaggedBuffer<'a> {
        self.buf[6..8].copy_from_slice(&flags.to_be_bytes());
        FlaggedBuffer { buf: self.buf }
    }
}

pub struct FlaggedBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> FlaggedBuffer<'a> {
    pub fn sequenced(self, sequence_id: SequenceId) -> SequencedBuffer<'a> {
        self.buf[30..32].copy_from_slice(&sequence_id.to_be_bytes());
        SequencedBuffer { buf: self.buf }
    }
}

pub struct SequencedBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> SequencedBuffer<'a> {
    pub fn payload(self) -> PayloadBuffer<'a> {
        PayloadBuffer { buf: self.buf }
    }
}

pub struct PayloadBuffer<'a> {
    buf: &'a mut [u8],
}

impl<'a> PayloadBuffer<'a> {
    pub fn buf(&mut self) -> &mut [u8] {
        &mut self.buf[34..]
    }

    pub fn finalize(self, payload_len: u16) -> FinalizedBuffer<'a> {
        let payload_len = payload_len as usize;
        let total_len = (34 + payload_len) as u16;
        self.buf[2..4].copy_from_slice(&total_len.to_be_bytes());

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

pub enum ControlField {
    Sync = 0x00,
    DelayRequest = 0x01,
    FollowUp = 0x02,
    DelayResponse = 0x03,
    Management = 0x04,
    Other = 0x05,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{ForeignClockDS, Priority1, Priority2};
    use crate::clock::{ClockIdentity, ClockQuality};
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::port::PortNumber;
    use crate::time::TimeStamp;

    #[test]
    fn buffer_integrity_two_step_sync() {
        let msg = TwoStepSyncMessage::new(7.into());
        let mut buf = MessageBuffer::new(
            0,
            2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            0x7F,
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
            0,
            2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            0x7F,
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
            0,
            2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            0x7F,
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
            0,
            2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            0x7F,
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
            ForeignClockDS::new(
                ClockIdentity::new(&[0; 8]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
        );
        let mut buf = MessageBuffer::new(
            0,
            2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            0x7F,
        );
        let wire = msg.serialize(&mut buf);
        let bytes = wire.as_ref();
        let len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len());
        assert_eq!(bytes[0] & 0x0F, 0x0B);
        assert_eq!(bytes[32], 0x05);
    }
}
