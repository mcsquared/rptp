use crate::{
    bmca::{Bmca, ForeignClockDS},
    port::{PortIdentity, PortMap},
    result::{ParseError, ProtocolError, Result},
    time::{Instant, LogMessageInterval, TimeInterval, TimeStamp},
    wire::{
        AnnouncePayload, ControlField, DelayResponsePayload, FinalizedBuffer, FollowUpPayload,
        LengthCheckedMessage, MessageBuffer, MessageFlags, MessageHeader, MessageType, SyncPayload,
    },
};

pub struct DomainMessage<'a> {
    header: MessageHeader<'a>,
}

impl<'a> DomainMessage<'a> {
    pub fn new(length_checked: LengthCheckedMessage<'a>) -> Self {
        Self {
            header: MessageHeader::new(length_checked),
        }
    }

    pub fn dispatch_event(self, ports: &mut impl PortMap, timestamp: TimeStamp) -> Result<()> {
        let domain_number = self.header.domain_number();
        let port = ports.port_by_domain(domain_number)?;
        let source_port_identity = self.header.source_port_identity();
        let msg = EventMessage::new(
            self.header.message_type()?,
            self.header.sequence_id(),
            self.header.flags(),
            self.header.log_message_interval(),
            self.header.payload(),
        )?;
        port.process_event_message(source_port_identity, msg, timestamp);

        Ok(())
    }

    pub fn dispatch_general(self, ports: &mut impl PortMap, now: Instant) -> Result<()> {
        let domain_number = self.header.domain_number();
        let port = ports.port_by_domain(domain_number)?;
        let source_port_identity = self.header.source_port_identity();
        let msg = GeneralMessage::new(
            self.header.message_type()?,
            self.header.sequence_id(),
            self.header.log_message_interval(),
            self.header.payload(),
        )?;
        port.process_general_message(source_port_identity, msg, now);

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMessage {
    DelayReq(DelayRequestMessage),
    OneStepSync(OneStepSyncMessage),
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
    pub fn new(
        msg_type: MessageType,
        sequence_id: SequenceId,
        flags: MessageFlags,
        log_message_interval: LogMessageInterval,
        payload: &[u8],
    ) -> Result<Self> {
        match msg_type {
            MessageType::Sync => {
                if flags.contains(MessageFlags::TWO_STEP) {
                    Ok(Self::TwoStepSync(TwoStepSyncMessage::new(
                        sequence_id,
                        log_message_interval,
                    )))
                } else {
                    Ok(Self::OneStepSync(OneStepSyncMessage::new(
                        sequence_id,
                        log_message_interval,
                        SyncPayload::new(payload).origin_timestamp()?,
                    )))
                }
            }
            MessageType::DelayRequest => Ok(Self::DelayReq(DelayRequestMessage::new(sequence_id))),
            _ => Err(ProtocolError::UnknownMessageType(msg_type.to_nibble()).into()),
        }
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        match self {
            EventMessage::DelayReq(msg) => msg.serialize(buf),
            EventMessage::OneStepSync(msg) => msg.serialize(buf),
            EventMessage::TwoStepSync(msg) => msg.serialize(buf),
        }
    }
}

impl GeneralMessage {
    pub fn new(
        msg_type: MessageType,
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        payload: &[u8],
    ) -> Result<Self> {
        match msg_type {
            MessageType::Announce => {
                let announce_payload = AnnouncePayload::new(payload);
                Ok(Self::Announce(AnnounceMessage::new(
                    sequence_id,
                    log_message_interval,
                    announce_payload.foreign_clock_ds()?,
                )))
            }
            MessageType::FollowUp => Ok(Self::FollowUp(FollowUpMessage::new(
                sequence_id,
                log_message_interval,
                FollowUpPayload::new(payload).precise_origin_timestamp()?,
            ))),
            MessageType::DelayResponse => Ok(Self::DelayResp(DelayResponseMessage::new(
                sequence_id,
                DelayResponsePayload::new(payload).receive_timestamp()?,
            ))),
            _ => Err(ProtocolError::UnknownMessageType(msg_type.to_nibble()).into()),
        }
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        match self {
            GeneralMessage::Announce(msg) => msg.serialize(buf),
            GeneralMessage::DelayResp(msg) => msg.serialize(buf),
            GeneralMessage::FollowUp(msg) => msg.serialize(buf),
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
                .ok_or(ParseError::PayloadTooShort {
                    field: "SequenceId",
                    expected: 2,
                    found: buf.len(),
                })?
                .try_into()
                .map_err(|_| ParseError::PayloadTooShort {
                    field: "SequenceId",
                    expected: 2,
                    found: buf.len(),
                })?,
        );
        Ok(Self::new(id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnounceMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
    foreign_clock_ds: ForeignClockDS,
}

impl AnnounceMessage {
    pub fn new(
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        foreign_clock_ds: ForeignClockDS,
    ) -> Self {
        Self {
            sequence_id,
            log_message_interval,
            foreign_clock_ds,
        }
    }

    pub fn feed_bmca(self, bmca: &mut impl Bmca, source_port_identity: PortIdentity, now: Instant) {
        if let Some(log_interval) = self.log_message_interval.log_interval() {
            bmca.consider(
                source_port_identity,
                self.foreign_clock_ds,
                log_interval,
                now,
            );
        }
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .typed(MessageType::Announce, ControlField::Other)
            .flagged(MessageFlags::empty())
            .sequenced(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[13..29].copy_from_slice(&self.foreign_clock_ds.to_bytes());

        payload.finalize(30)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OneStepSyncMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
    origin_timestamp: TimeStamp,
}

impl OneStepSyncMessage {
    pub fn new(
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        origin_timestamp: TimeStamp,
    ) -> Self {
        Self {
            sequence_id,
            log_message_interval,
            origin_timestamp,
        }
    }

    pub fn master_slave_offset(&self, ingress_timestamp: TimeStamp) -> TimeInterval {
        ingress_timestamp - self.origin_timestamp
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let mut payload = buf
            .typed(MessageType::Sync, ControlField::Sync)
            .flagged(MessageFlags::empty())
            .sequenced(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        let payload_buf = payload.buf();
        payload_buf[..10].copy_from_slice(&self.origin_timestamp.to_wire());

        payload.finalize(10)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwoStepSyncMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
}

impl TwoStepSyncMessage {
    pub fn new(sequence_id: SequenceId, log_message_interval: LogMessageInterval) -> Self {
        Self {
            sequence_id,
            log_message_interval,
        }
    }

    pub fn follow_up(self, precise_origin_timestamp: TimeStamp) -> FollowUpMessage {
        FollowUpMessage::new(
            self.sequence_id,
            self.log_message_interval,
            precise_origin_timestamp,
        )
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let payload = buf
            .typed(MessageType::Sync, ControlField::Sync)
            .flagged(MessageFlags::TWO_STEP)
            .sequenced(self.sequence_id)
            .with_log_message_interval(self.log_message_interval)
            .payload();

        payload.finalize(10)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowUpMessage {
    sequence_id: SequenceId,
    log_message_interval: LogMessageInterval,
    precise_origin_timestamp: TimeStamp,
}

impl FollowUpMessage {
    pub fn new(
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        precise_origin_timestamp: TimeStamp,
    ) -> Self {
        Self {
            sequence_id,
            log_message_interval,
            precise_origin_timestamp,
        }
    }

    pub fn master_slave_offset(
        &self,
        sync: TwoStepSyncMessage,
        sync_ingress_timestamp: TimeStamp,
    ) -> Option<TimeInterval> {
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
            .with_log_message_interval(self.log_message_interval)
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

    pub fn response(self, receive_timestamp: TimeStamp) -> DelayResponseMessage {
        DelayResponseMessage::new(self.sequence_id, receive_timestamp)
    }

    pub fn serialize<'a>(&self, buf: &'a mut MessageBuffer) -> FinalizedBuffer<'a> {
        let payload = buf
            .typed(MessageType::DelayRequest, ControlField::DelayRequest)
            .flagged(MessageFlags::empty())
            .sequenced(self.sequence_id)
            .with_log_message_interval(LogMessageInterval::unspecified())
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

    pub fn slave_master_offset(
        &self,
        delay_req: DelayRequestMessage,
        delay_req_egress_timestamp: TimeStamp,
    ) -> Option<TimeInterval> {
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
            .with_log_message_interval(LogMessageInterval::unspecified())
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

impl<M> Default for MessageWindow<M> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::wire::UnvalidatedMessage;

    use crate::bmca::{Priority1, Priority2};
    use crate::clock::{ClockIdentity, ClockQuality, StepsRemoved};
    use crate::port::{DomainNumber, PortIdentity, PortNumber};
    use crate::time::LogMessageInterval;
    use crate::wire::{PtpVersion, TransportSpecific};

    #[test]
    fn announce_message_wire_roundtrip() {
        let announce = AnnounceMessage::new(
            42.into(),
            LogMessageInterval::new(0x7F),
            ForeignClockDS::new(
                ClockIdentity::new(&[0; 8]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
                StepsRemoved::new(42),
            ),
        );

        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = announce.serialize(&mut buf);
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::Announce(announce));
    }

    #[test]
    fn one_step_sync_message_wire_roundtrip() {
        let sync =
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(5), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = sync.serialize(&mut buf);
        let parsed = EventMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, EventMessage::OneStepSync(sync));
    }

    #[test]
    fn two_step_sync_message_wire_roundtrip() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(7));
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = sync.serialize(&mut buf);
        let parsed = EventMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, EventMessage::TwoStepSync(sync));
    }

    #[test]
    fn follow_up_message_wire_roundtrip() {
        let follow_up =
            FollowUpMessage::new(42.into(), LogMessageInterval::new(3), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = follow_up.serialize(&mut buf);
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::FollowUp(follow_up));
    }

    #[test]
    fn delay_request_message_wire_roundtrip() {
        let delay_req = DelayRequestMessage::new(42.into());
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = delay_req.serialize(&mut buf);
        let parsed = EventMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, EventMessage::DelayReq(delay_req));
    }

    #[test]
    fn delay_response_message_wire_roundtrip() {
        let delay_resp = DelayResponseMessage::new(42.into(), TimeStamp::new(1, 2));
        let mut buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::new(ClockIdentity::new(&[0; 8]), PortNumber::new(1)),
        );
        let wire = delay_resp.serialize(&mut buf);
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::DelayResp(delay_resp));
    }

    #[test]
    fn event_message_new_reports_short_sync_payload() {
        let payload = [0u8; 5]; // less than 10 bytes required for origin_timestamp

        let res = EventMessage::new(
            MessageType::Sync,
            1.into(),
            MessageFlags::empty(),
            LogMessageInterval::new(3),
            &payload,
        );

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "Sync.origin_timestamp",
                expected: 10,
                found: 5,
            }
            .into())
        );
    }

    #[test]
    fn general_message_new_reports_short_announce_payload() {
        let payload = [0u8; 20]; // shorter than 13 + 16 required bytes

        let res = GeneralMessage::new(
            MessageType::Announce,
            1.into(),
            LogMessageInterval::new(0),
            &payload,
        );

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "Announce.foreign_clock_ds",
                expected: 16,
                found: payload.len().saturating_sub(13),
            }
            .into())
        );
    }

    #[test]
    fn sequence_id_try_from_reports_short_payload() {
        let buf = [0u8; 1];

        let res = SequenceId::try_from(&buf[..]);

        assert_eq!(
            res,
            Err(ParseError::PayloadTooShort {
                field: "SequenceId",
                expected: 2,
                found: 1,
            }
            .into())
        );
    }

    #[test]
    fn domain_message_dispatch_event_propagates_domain_not_found() {
        struct FailingPortMap;

        impl PortMap for FailingPortMap {
            fn port_by_domain(
                &mut self,
                domain_number: DomainNumber,
            ) -> Result<&mut dyn crate::port::PortIngress> {
                Err(ProtocolError::DomainNotFound(domain_number.as_u8()).into())
            }
        }

        let mut buf = [0u8; 34 + 10];
        buf[1] = PtpVersion::V2.as_u8();
        let total_len = buf.len() as u16;
        buf[2..4].copy_from_slice(&total_len.to_be_bytes());

        // Message type nibble: Sync and some domain number
        buf[0] |= MessageType::Sync.to_nibble();
        buf[4] = 42;

        let length_checked = UnvalidatedMessage::new(&buf)
            .length_checked_v2()
            .expect("length check must succeed");
        let domain_msg = DomainMessage::new(length_checked);

        let mut ports = FailingPortMap;
        let res = domain_msg.dispatch_event(&mut ports, TimeStamp::new(0, 0));

        assert_eq!(res, Err(ProtocolError::DomainNotFound(42).into()));
    }

    #[test]
    fn twostep_sync_message_produces_follow_up() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(9));
        let follow_up = sync.follow_up(TimeStamp::new(4, 0));

        assert_eq!(
            follow_up,
            FollowUpMessage::new(42.into(), LogMessageInterval::new(9), TimeStamp::new(4, 0))
        );
    }

    #[test]
    fn follow_up_message_produces_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(11));
        let follow_up =
            FollowUpMessage::new(42.into(), LogMessageInterval::new(11), TimeStamp::new(4, 0));

        let sync_ingress_timestamp = TimeStamp::new(5, 0);
        let offset = follow_up.master_slave_offset(sync, sync_ingress_timestamp);

        assert_eq!(offset, Some(TimeInterval::new(1, 0)));
    }

    #[test]
    fn follow_up_message_with_different_sequence_id_produces_no_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(11));
        let follow_up =
            FollowUpMessage::new(43.into(), LogMessageInterval::new(11), TimeStamp::new(4, 0));

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

        assert_eq!(offset, Some(TimeInterval::new(1, 0)));
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

        sync_window.record((
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(5)),
            TimeStamp::new(2, 0),
        ));
        follow_up_window.record(FollowUpMessage::new(
            1.into(),
            LogMessageInterval::new(5),
            TimeStamp::new(1, 0),
        ));

        let offset = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });

        assert_eq!(offset, Some(TimeInterval::new(1, 0)));
    }

    #[test]
    fn message_window_no_match_if_one_empty() {
        let mut sync_window = MessageWindow::new();
        let follow_up_window = MessageWindow::<FollowUpMessage>::new();

        sync_window.record((
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(3)),
            TimeStamp::new(2, 0),
        ));

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

        sync_window.record((
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(3)),
            TimeStamp::new(2, 0),
        ));
        follow_up_window.record(FollowUpMessage::new(
            2.into(),
            LogMessageInterval::new(3),
            TimeStamp::new(1, 0),
        ));

        let no_match = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(no_match, None);

        follow_up_window.record(FollowUpMessage::new(
            1.into(),
            LogMessageInterval::new(3),
            TimeStamp::new(1, 0),
        ));
        let matched = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(matched, Some(TimeInterval::new(1, 0)));
    }

    #[test]
    fn message_window_out_of_order_then_recover_sync_arrives() {
        // Start with a follow-up, then a non-matching sync, then a matching sync.
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        follow_up_window.record(FollowUpMessage::new(
            3.into(),
            LogMessageInterval::new(3),
            TimeStamp::new(1, 0),
        ));
        sync_window.record((
            TwoStepSyncMessage::new(4.into(), LogMessageInterval::new(3)),
            TimeStamp::new(2, 0),
        ));

        let no_match = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(no_match, None);

        sync_window.record((
            TwoStepSyncMessage::new(3.into(), LogMessageInterval::new(3)),
            TimeStamp::new(2, 0),
        ));
        let matched = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(matched, Some(TimeInterval::new(1, 0)));
    }

    #[test]
    fn message_window_in_order_follow_up_then_sync() {
        // FollowUp for seq=5 arrives first, then matching Sync for seq=5.
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        follow_up_window.record(FollowUpMessage::new(
            5.into(),
            LogMessageInterval::new(3),
            TimeStamp::new(10, 0),
        ));
        sync_window.record((
            TwoStepSyncMessage::new(5.into(), LogMessageInterval::new(3)),
            TimeStamp::new(11, 0),
        ));

        let matched = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(matched, Some(TimeInterval::new(1, 0)));
    }

    #[test]
    fn message_window_in_order_updates_to_newer_pair() {
        // Two successive matching pairs; combine_latest should reflect the latest pair.
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        // First pair -> 2s offset
        sync_window.record((
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(5)),
            TimeStamp::new(5, 0),
        ));
        follow_up_window.record(FollowUpMessage::new(
            1.into(),
            LogMessageInterval::new(5),
            TimeStamp::new(3, 0),
        ));
        let first = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(first, Some(TimeInterval::new(2, 0)));

        // Second pair -> 3s offset overwrites windows
        sync_window.record((
            TwoStepSyncMessage::new(2.into(), LogMessageInterval::new(5)),
            TimeStamp::new(9, 0),
        ));
        follow_up_window.record(FollowUpMessage::new(
            2.into(),
            LogMessageInterval::new(5),
            TimeStamp::new(6, 0),
        ));
        let second = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });
        assert_eq!(second, Some(TimeInterval::new(3, 0)));
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
