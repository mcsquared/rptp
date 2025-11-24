use crate::{
    bmca::{Bmca, ForeignClockDS},
    buffer::{ControlField, FinalizedBuffer, MessageBuffer, MessageFlags, MessageType},
    port::{DomainNumber, PortIdentity, PortMap},
    result::{ParseError, ProtocolError, Result},
    time::{Duration, TimeStamp},
};

pub struct DomainMessage<'a> {
    buf: &'a [u8],
}

impl<'a> DomainMessage<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn dispatch_event(self, ports: &mut impl PortMap, timestamp: TimeStamp) -> Result<()> {
        let domain_number = self.domain_number()?;
        let port = ports.port_by_domain(domain_number)?;
        let source_port_identity = self.source_port_identity()?;
        let msg = EventMessage::try_from(self.buf)?;
        port.process_event_message(source_port_identity, msg, timestamp);

        Ok(())
    }

    pub fn dispatch_general(self, ports: &mut impl PortMap) -> Result<()> {
        let domain_number = self.domain_number()?;
        let port = ports.port_by_domain(domain_number)?;
        let source_port_identity = self.source_port_identity()?;
        let msg = GeneralMessage::try_from(self.buf)?;
        port.process_general_message(source_port_identity, msg);

        Ok(())
    }

    fn domain_number(&self) -> Result<DomainNumber> {
        self.buf
            .get(4)
            .copied()
            .map(DomainNumber::new)
            .ok_or(ProtocolError::DomainNotFound.into())
    }

    fn source_port_identity(&self) -> Result<PortIdentity> {
        Ok(PortIdentity::from_slice(
            self.buf
                .get(20..30)
                .ok_or(ParseError::BadLength)?
                .try_into()
                .map_err(|_| ParseError::BadLength)?,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMessage {
    DelayReq(DelayRequestMessage),
    TwoStepSync(TwoStepSyncMessage),
}

impl EventMessage {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralMessage {
    Announce(AnnounceMessage),
    DelayResp(DelayResponseMessage),
    FollowUp(FollowUpMessage),
}

impl GeneralMessage {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMessage {
    AnnounceSendTimeout,
    DelayRequestTimeout,
    SyncTimeout,
    Timestamp(TimestampMessage),
    Initialized,
    AnnounceReceiptTimeout,
    QualificationTimeout,
}

impl EventMessage {
    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        match self {
            EventMessage::DelayReq(msg) => msg.serialize(buf),
            EventMessage::TwoStepSync(msg) => msg.serialize(buf),
        }
    }
}

impl TryFrom<&[u8]> for EventMessage {
    type Error = crate::result::Error;

    fn try_from(buf: &[u8]) -> Result<Self> {
        let msg_type = buf.get(0).ok_or(ParseError::BadLength)? & 0x0F;

        match msg_type {
            0x00 => Ok(Self::TwoStepSync(TwoStepSyncMessage::from_slice(
                buf.try_into().map_err(|_| ParseError::BadLength)?,
            )?)),
            0x01 => Ok(Self::DelayReq(DelayRequestMessage::from_slice(
                buf.try_into().map_err(|_| ParseError::BadLength)?,
            )?)),
            _ => Err(ParseError::BadMessageType.into()),
        }
    }
}

impl GeneralMessage {
    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        match self {
            GeneralMessage::Announce(msg) => msg.serialize(buf),
            GeneralMessage::DelayResp(msg) => msg.serialize(buf),
            GeneralMessage::FollowUp(msg) => msg.serialize(buf),
        }
    }
}

impl TryFrom<&[u8]> for GeneralMessage {
    type Error = crate::result::Error;

    fn try_from(buf: &[u8]) -> Result<Self> {
        let msgtype = buf.get(0).ok_or(ParseError::BadLength)? & 0x0F;

        match msgtype {
            0x0B => Ok(Self::Announce(AnnounceMessage::from_slice(
                buf.try_into().map_err(|_| ParseError::BadLength)?,
            )?)),
            0x08 => Ok(Self::FollowUp(FollowUpMessage::from_slice(
                buf.try_into().map_err(|_| ParseError::BadLength)?,
            )?)),
            0x09 => Ok(Self::DelayResp(DelayResponseMessage::from_slice(
                buf.try_into().map_err(|_| ParseError::BadLength)?,
            )?)),
            _ => Err(ParseError::BadMessageType.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequenceId {
    id: u16,
}

impl SequenceId {
    pub fn new(id: u16) -> Self {
        Self { id }
    }

    pub fn follows(&self, previous: SequenceId) -> bool {
        self.id.wrapping_sub(previous.id) == 1
    }

    pub fn next(&self) -> Self {
        Self {
            id: self.id.wrapping_add(1),
        }
    }

    pub fn to_be_bytes(&self) -> [u8; 2] {
        self.id.to_be_bytes()
    }
}

impl From<u16> for SequenceId {
    fn from(id: u16) -> Self {
        Self::new(id)
    }
}

impl TryFrom<&[u8]> for SequenceId {
    type Error = crate::result::Error;

    fn try_from(buf: &[u8]) -> Result<Self> {
        let id = u16::from_be_bytes(
            buf.get(0..2)
                .ok_or(ParseError::BadLength)?
                .try_into()
                .map_err(|_| ParseError::BadLength)?,
        );
        Ok(Self::new(id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnounceMessage {
    sequence_id: SequenceId,
    foreign_clock_ds: ForeignClockDS,
}

impl AnnounceMessage {
    pub fn new(sequence_id: SequenceId, foreign_clock_ds: ForeignClockDS) -> Self {
        Self {
            sequence_id,
            foreign_clock_ds,
        }
    }

    pub fn from_slice(buf: &[u8]) -> Result<Self> {
        let sequence_id = SequenceId::try_from(&buf[30..32])?;
        let foreign_clock_ds =
            ForeignClockDS::from_slice(&buf[47..63].try_into().map_err(|_| ParseError::BadLength)?);

        Ok(Self {
            sequence_id,
            foreign_clock_ds,
        })
    }

    pub fn feed_bmca(self, bmca: &mut impl Bmca, source_port_identity: PortIdentity) {
        bmca.consider(source_port_identity, self.foreign_clock_ds)
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .typed(MessageType::Announce, ControlField::Other)
            .flagged(MessageFlags::empty())
            .sequenced(self.sequence_id)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[13..29].copy_from_slice(&self.foreign_clock_ds.to_bytes());

        payload.finalize(30)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwoStepSyncMessage {
    sequence_id: SequenceId,
}

impl TwoStepSyncMessage {
    pub fn new(sequence_id: SequenceId) -> Self {
        Self { sequence_id }
    }

    pub fn from_slice(buf: &[u8]) -> Result<Self> {
        let sequence_id = SequenceId::try_from(&buf[30..32])?;

        Ok(Self { sequence_id })
    }

    pub fn follow_up(self, precise_origin_timestamp: TimeStamp) -> FollowUpMessage {
        FollowUpMessage::new(self.sequence_id, precise_origin_timestamp)
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let payload = buf
            .typed(MessageType::Sync, ControlField::Sync)
            .flagged(MessageFlags::TWO_STEP)
            .sequenced(self.sequence_id)
            .payload();

        payload.finalize(10)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowUpMessage {
    sequence_id: SequenceId,
    precise_origin_timestamp: TimeStamp,
}

impl FollowUpMessage {
    pub fn new(sequence_id: SequenceId, precise_origin_timestamp: TimeStamp) -> Self {
        Self {
            sequence_id,
            precise_origin_timestamp,
        }
    }

    pub fn from_slice(buf: &[u8]) -> Result<Self> {
        let sequence_id = SequenceId::try_from(&buf[30..32])?;
        let wire_timestamp = WireTimeStamp::new(
            buf.get(34..44)
                .ok_or(ParseError::BadLength)?
                .try_into()
                .map_err(|_| ParseError::BadLength)?,
        );
        let precise_origin_timestamp = wire_timestamp.timestamp()?;

        Ok(Self {
            sequence_id,
            precise_origin_timestamp,
        })
    }

    pub fn master_slave_offset(
        &self,
        sync: TwoStepSyncMessage,
        sync_ingress_timestamp: TimeStamp,
    ) -> Option<Duration> {
        if self.sequence_id == sync.sequence_id {
            Some(sync_ingress_timestamp - self.precise_origin_timestamp)
        } else {
            None
        }
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .typed(MessageType::FollowUp, ControlField::FollowUp)
            .flagged(MessageFlags::empty())
            .sequenced(self.sequence_id)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[..10].copy_from_slice(&self.precise_origin_timestamp.to_wire());

        payload.finalize(10)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayRequestMessage {
    sequence_id: SequenceId,
}

impl DelayRequestMessage {
    pub fn new(sequence_id: SequenceId) -> Self {
        Self { sequence_id }
    }

    pub fn from_slice(buf: &[u8]) -> Result<Self> {
        let sequence_id = SequenceId::try_from(&buf[30..32])?;

        Ok(Self { sequence_id })
    }

    pub fn response(self, receive_timestamp: TimeStamp) -> DelayResponseMessage {
        DelayResponseMessage::new(self.sequence_id, receive_timestamp)
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let payload = buf
            .typed(MessageType::DelayRequest, ControlField::DelayRequest)
            .flagged(MessageFlags::empty())
            .sequenced(self.sequence_id)
            .payload();

        payload.finalize(10)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayResponseMessage {
    sequence_id: SequenceId,
    receive_timestamp: TimeStamp,
}

impl DelayResponseMessage {
    pub fn new(sequence_id: SequenceId, receive_timestamp: TimeStamp) -> Self {
        Self {
            sequence_id,
            receive_timestamp,
        }
    }

    pub fn from_slice(buf: &[u8]) -> Result<Self> {
        let sequence_id = SequenceId::try_from(&buf[30..32])?;
        let wire_timestamp = WireTimeStamp::new(
            buf.get(34..44)
                .ok_or(ParseError::BadLength)?
                .try_into()
                .map_err(|_| ParseError::BadLength)?,
        );
        let receive_timestamp = wire_timestamp.timestamp()?;

        Ok(Self {
            sequence_id,
            receive_timestamp,
        })
    }

    pub fn slave_master_offset(
        &self,
        delay_req: DelayRequestMessage,
        delay_req_egress_timestamp: TimeStamp,
    ) -> Option<Duration> {
        if self.sequence_id == delay_req.sequence_id {
            Some(self.receive_timestamp - delay_req_egress_timestamp)
        } else {
            None
        }
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .typed(MessageType::DelayResponse, ControlField::DelayResponse)
            .flagged(MessageFlags::empty())
            .sequenced(self.sequence_id)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[..10].copy_from_slice(&self.receive_timestamp.to_wire());

        payload.finalize(20)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampMessage {
    pub event_msg: EventMessage,
    pub egress_timestamp: TimeStamp,
}

impl TimestampMessage {
    pub fn new(event_msg: EventMessage, egress_timestamp: TimeStamp) -> Self {
        Self {
            event_msg,
            egress_timestamp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireTimeStamp<'a> {
    buf: &'a [u8; 10],
}

impl<'a> WireTimeStamp<'a> {
    pub fn new(buf: &'a [u8; 10]) -> Self {
        Self { buf }
    }

    pub fn timestamp(&self) -> Result<TimeStamp> {
        let secs_msb = u16::from_be_bytes(
            self.buf[0..2]
                .try_into()
                .map_err(|_| ParseError::BadLength)?,
        );
        let secs_lsb = u32::from_be_bytes(
            self.buf[2..6]
                .try_into()
                .map_err(|_| ParseError::BadLength)?,
        );

        let seconds = ((secs_msb as u64) << 32) | (secs_lsb as u64);
        let nanos = u32::from_be_bytes(
            self.buf[6..10]
                .try_into()
                .map_err(|_| ParseError::BadLength)?,
        );

        if nanos < 1_000_000_000 {
            Ok(TimeStamp::new(seconds, nanos))
        } else {
            Err(ProtocolError::InvalidTimestamp.into())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageWindow<M> {
    current: Option<M>,
}

impl<M> MessageWindow<M> {
    pub fn new() -> Self {
        Self { current: None }
    }

    pub fn record(&mut self, msg: M) {
        self.current.replace(msg);
    }

    pub fn combine_latest<N, F, T>(&self, other: &MessageWindow<N>, combine: F) -> Option<T>
    where
        F: Fn(&M, &N) -> Option<T>,
    {
        if let (Some(m), Some(n)) = (self.current.as_ref(), other.current.as_ref()) {
            combine(m, n)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{Priority1, Priority2};
    use crate::buffer::{LogMessageInterval, PtpVersion, TransportSpecific};
    use crate::clock::{ClockIdentity, ClockQuality, StepsRemoved};
    use crate::port::PortNumber;

    #[test]
    fn announce_message_wire_roundtrip() {
        let announce = AnnounceMessage::new(
            42.into(),
            ForeignClockDS::new(
                ClockIdentity::new(&[0; 8]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
                StepsRemoved::new(42),
            ),
        );

        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            LogMessageInterval::new(0x7F),
        );
        let wire = announce.serialize(&mut buf);
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::Announce(announce));
    }

    #[test]
    fn two_step_sync_message_wire_roundtrip() {
        let sync = TwoStepSyncMessage::new(42.into());
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            LogMessageInterval::new(0x7F),
        );
        let wire = sync.serialize(&mut buf);
        let parsed = EventMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, EventMessage::TwoStepSync(sync));
    }

    #[test]
    fn follow_up_message_wire_roundtrip() {
        let follow_up = FollowUpMessage::new(42.into(), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            LogMessageInterval::new(0x7F),
        );
        let wire = follow_up.serialize(&mut buf);
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::FollowUp(follow_up));
    }

    #[test]
    fn delay_request_message_wire_roundtrip() {
        let delay_req = DelayRequestMessage::new(42.into());
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            LogMessageInterval::new(0x7F),
        );
        let wire = delay_req.serialize(&mut buf);
        let parsed = EventMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, EventMessage::DelayReq(delay_req));
    }

    #[test]
    fn delay_response_message_wire_roundtrip() {
        let delay_resp = DelayResponseMessage::new(42.into(), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific::new(),
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
            LogMessageInterval::new(0x7F),
        );
        let wire = delay_resp.serialize(&mut buf);
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::DelayResp(delay_resp));
    }

    #[test]
    fn wire_timestamp_roundtrip() {
        let ts = TimeStamp::new(42, 500_000_000);
        let wire = ts.to_wire();
        let parsed = WireTimeStamp::new(&wire).timestamp();

        assert_eq!(parsed, Ok(ts));
    }

    #[test]
    fn wire_timestamp_valid_nanos() {
        let wire = [
            0, 0, 0, 0, 0, 42, // seconds
            0, 0, 0, 42, // nanoseconds
        ];
        let parsed = WireTimeStamp::new(&wire).timestamp();

        assert_eq!(parsed, Ok(TimeStamp::new(42, 42)));
    }

    #[test]
    fn wire_timestamp_invalid_nanos() {
        let wire = [
            0, 0, 0, 0, 0, 42, // seconds
            0x3B, 0x9A, 0xCA, 0x00, // nanoseconds (1_000_000_000)
        ];
        let parsed = WireTimeStamp::new(&wire).timestamp();

        assert_eq!(parsed, Err(ProtocolError::InvalidTimestamp.into()));
    }

    #[test]
    fn twostep_sync_message_produces_follow_up() {
        let sync = TwoStepSyncMessage::new(42.into());
        let follow_up = sync.follow_up(TimeStamp::new(4, 0));

        assert_eq!(
            follow_up,
            FollowUpMessage::new(42.into(), TimeStamp::new(4, 0))
        );
    }

    #[test]
    fn follow_up_message_produces_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42.into());
        let follow_up = FollowUpMessage::new(42.into(), TimeStamp::new(4, 0));

        let sync_ingress_timestamp = TimeStamp::new(5, 0);
        let offset = follow_up.master_slave_offset(sync, sync_ingress_timestamp);

        assert_eq!(offset, Some(Duration::new(1, 0)));
    }

    #[test]
    fn follow_up_message_with_different_sequence_id_produces_no_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42.into());
        let follow_up = FollowUpMessage::new(43.into(), TimeStamp::new(4, 0));

        let sync_ingress_timestamp = TimeStamp::new(5, 0);
        let offset = follow_up.master_slave_offset(sync, sync_ingress_timestamp);

        assert_eq!(offset, None);
    }

    #[test]
    fn delay_response_produces_slave_master_offset() {
        let delay_req = DelayRequestMessage::new(42.into());
        let delay_resp = DelayResponseMessage::new(42.into(), TimeStamp::new(5, 0));

        let delay_req_egress_timestamp = TimeStamp::new(4, 0);
        let offset = delay_resp.slave_master_offset(delay_req, delay_req_egress_timestamp);

        assert_eq!(offset, Some(Duration::new(1, 0)));
    }

    #[test]
    fn delay_response_with_different_sequence_id_produces_no_slave_master_offset() {
        let delay_req = DelayRequestMessage::new(42.into());
        let delay_resp = DelayResponseMessage::new(43.into(), TimeStamp::new(5, 0));

        let delay_req_egress_timestamp = TimeStamp::new(4, 0);
        let offset = delay_resp.slave_master_offset(delay_req, delay_req_egress_timestamp);

        assert_eq!(offset, None);
    }

    #[test]
    fn delay_request_message_produces_delay_response_message() {
        let delay_req = DelayRequestMessage::new(42.into());
        let delay_resp = delay_req.response(TimeStamp::new(4, 0));

        assert_eq!(
            delay_resp,
            DelayResponseMessage::new(42.into(), TimeStamp::new(4, 0))
        );
    }

    #[test]
    fn message_window_match_latest() {
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        sync_window.record((TwoStepSyncMessage::new(1.into()), TimeStamp::new(2, 0)));
        follow_up_window.record(FollowUpMessage::new(1.into(), TimeStamp::new(1, 0)));

        let offset = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });

        assert_eq!(offset, Some(Duration::new(1, 0)));
    }

    #[test]
    fn message_window_no_match_if_one_empty() {
        let mut sync_window = MessageWindow::new();
        let follow_up_window = MessageWindow::<FollowUpMessage>::new();

        sync_window.record((TwoStepSyncMessage::new(1.into()), TimeStamp::new(2, 0)));

        let offset = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });

        assert_eq!(offset, None);
    }

    #[test]
    fn message_window_out_of_order_then_recover_follow_up_arrives() {
        // Start with a sync, then a non-matching follow-up, then a matching follow-up.
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        sync_window.record((TwoStepSyncMessage::new(1.into()), TimeStamp::new(2, 0)));
        follow_up_window.record(FollowUpMessage::new(2.into(), TimeStamp::new(1, 0)));

        let no_match = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(no_match, None);

        follow_up_window.record(FollowUpMessage::new(1.into(), TimeStamp::new(1, 0)));
        let matched = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(matched, Some(Duration::new(1, 0)));
    }

    #[test]
    fn message_window_out_of_order_then_recover_sync_arrives() {
        // Start with a follow-up, then a non-matching sync, then a matching sync.
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        follow_up_window.record(FollowUpMessage::new(3.into(), TimeStamp::new(1, 0)));
        sync_window.record((TwoStepSyncMessage::new(4.into()), TimeStamp::new(2, 0)));

        let no_match = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(no_match, None);

        sync_window.record((TwoStepSyncMessage::new(3.into()), TimeStamp::new(2, 0)));
        let matched = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(matched, Some(Duration::new(1, 0)));
    }

    #[test]
    fn message_window_in_order_follow_up_then_sync() {
        // FollowUp for seq=5 arrives first, then matching Sync for seq=5.
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        follow_up_window.record(FollowUpMessage::new(5.into(), TimeStamp::new(10, 0)));
        sync_window.record((TwoStepSyncMessage::new(5.into()), TimeStamp::new(11, 0)));

        let matched = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(matched, Some(Duration::new(1, 0)));
    }

    #[test]
    fn message_window_in_order_updates_to_newer_pair() {
        // Two successive matching pairs; combine_latest should reflect the latest pair.
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        // First pair -> 2s offset
        sync_window.record((TwoStepSyncMessage::new(1.into()), TimeStamp::new(5, 0)));
        follow_up_window.record(FollowUpMessage::new(1.into(), TimeStamp::new(3, 0)));
        let first = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(first, Some(Duration::new(2, 0)));

        // Second pair -> 3s offset overwrites windows
        sync_window.record((TwoStepSyncMessage::new(2.into()), TimeStamp::new(9, 0)));
        follow_up_window.record(FollowUpMessage::new(2.into(), TimeStamp::new(6, 0)));
        let second = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(second, Some(Duration::new(3, 0)));
    }

    #[test]
    fn sequence_id_follows_next_for_all_values() {
        for id in 0u16..=u16::MAX {
            let a: SequenceId = id.into();
            let b = a.next();
            assert!(b.follows(a), "next({id}) should follow {id}");
            assert!(!a.follows(a));
        }
    }

    #[test]
    fn sequence_id_roundtrip_to_from_be_bytes() {
        let samples = [0u16, 1, 2, 7, 42, 255, 256, 1024, 4096, 32767, 65534, 65535];
        for &id in &samples {
            let sid: SequenceId = id.into();
            let bytes = sid.to_be_bytes();
            let parsed = SequenceId::try_from(&bytes[..]).unwrap();
            assert_eq!(parsed, sid);
        }
    }

    #[test]
    fn sequence_id_does_not_follow_on_gap() {
        let a: SequenceId = 10u16.into();
        let c: SequenceId = 12u16.into();
        assert!(!c.follows(a));
    }

    #[test]
    fn sequence_id_wraps() {
        let a: SequenceId = u16::MAX.into();
        let b: SequenceId = 0u16.into();
        assert!(b.follows(a));
        assert_eq!(a.next(), b);
    }
}
