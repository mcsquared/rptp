use crate::time::{Duration, TimeStamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMessage {
    DelayReq(DelayRequestMessage),
    TwoStepSync(TwoStepSyncMessage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralMessage {
    DelayResp(DelayResponseMessage),
    FollowUp(FollowUpMessage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMessage {
    DelayCycle(DelayCycleMessage),
    SyncCycle(SyncCycleMessage),
    Timestamp {
        msg: EventMessage,
        timestamp: TimeStamp,
    },
    Initialized,
}

impl EventMessage {
    pub fn to_wire(&self) -> [u8; 64] {
        match self {
            EventMessage::DelayReq(msg) => msg.to_wire(),
            EventMessage::TwoStepSync(msg) => msg.to_wire(),
        }
    }
}

impl TryFrom<&[u8]> for EventMessage {
    type Error = ();

    fn try_from(b: &[u8]) -> Result<Self, Self::Error> {
        let msg_type = b.get(0).ok_or(())? & 0x0F;
        let sequence_id = u16::from_be_bytes(b.get(30..32).ok_or(())?.try_into().map_err(|_| ())?);

        match msg_type {
            0x00 => Ok(Self::TwoStepSync(TwoStepSyncMessage::new(sequence_id))),
            0x01 => Ok(Self::DelayReq(DelayRequestMessage::new(sequence_id))),
            _ => Err(()),
        }
    }
}

impl GeneralMessage {
    pub fn to_wire(&self) -> [u8; 64] {
        match self {
            GeneralMessage::DelayResp(msg) => msg.to_wire(),
            GeneralMessage::FollowUp(msg) => msg.to_wire(),
        }
    }
}

impl TryFrom<&[u8]> for GeneralMessage {
    type Error = ();

    fn try_from(buf: &[u8]) -> Result<Self, Self::Error> {
        let msgtype = buf.get(0).ok_or(())? & 0x0F;
        let sequence_id =
            u16::from_be_bytes(buf.get(30..32).ok_or(())?.try_into().map_err(|_| ())?);

        let wire_timestamp =
            WireTimeStamp::new(buf.get(34..44).ok_or(())?.try_into().map_err(|_| ())?);

        match msgtype {
            0x08 => Ok(Self::FollowUp(FollowUpMessage::new(
                sequence_id,
                wire_timestamp.timestamp().ok_or(())?,
            ))),
            0x09 => Ok(Self::DelayResp(DelayResponseMessage::new(
                sequence_id,
                wire_timestamp.timestamp().ok_or(())?,
            ))),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwoStepSyncMessage {
    sequence_id: u16,
}

impl TwoStepSyncMessage {
    pub fn new(sequence_id: u16) -> Self {
        Self { sequence_id }
    }

    pub fn follow_up(self, precise_origin_timestamp: TimeStamp) -> FollowUpMessage {
        FollowUpMessage::new(self.sequence_id, precise_origin_timestamp)
    }

    pub fn to_wire(&self) -> [u8; 64] {
        let mut buf = [0; 64];
        buf[0] = 0x00;
        buf[30..32].copy_from_slice(&self.sequence_id.to_be_bytes());

        buf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowUpMessage {
    sequence_id: u16,
    precise_origin_timestamp: TimeStamp,
}

impl FollowUpMessage {
    pub fn new(sequence_id: u16, precise_origin_timestamp: TimeStamp) -> Self {
        Self {
            sequence_id,
            precise_origin_timestamp,
        }
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

    pub fn to_wire(&self) -> [u8; 64] {
        let mut buf = [0; 64];
        buf[0] = 0x08 & 0x0F;
        buf[30..32].copy_from_slice(&self.sequence_id.to_be_bytes());
        buf[34..44].copy_from_slice(&self.precise_origin_timestamp.to_wire());
        buf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayRequestMessage {
    sequence_id: u16,
}

impl DelayRequestMessage {
    pub fn new(sequence_id: u16) -> Self {
        Self { sequence_id }
    }

    pub fn response(self, receive_timestamp: TimeStamp) -> DelayResponseMessage {
        DelayResponseMessage::new(self.sequence_id, receive_timestamp)
    }

    pub fn to_wire(&self) -> [u8; 64] {
        let mut buf = [0; 64];
        buf[0] = 0x01 & 0x0F;
        buf[30..32].copy_from_slice(&self.sequence_id.to_be_bytes());

        buf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayResponseMessage {
    sequence_id: u16,
    receive_timestamp: TimeStamp,
}

impl DelayResponseMessage {
    pub fn new(sequence_id: u16, receive_timestamp: TimeStamp) -> Self {
        Self {
            sequence_id,
            receive_timestamp,
        }
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

    pub fn to_wire(&self) -> [u8; 64] {
        let mut buf = [0; 64];
        buf[0] = 0x09 & 0x0F;
        buf[30..32].copy_from_slice(&self.sequence_id.to_be_bytes());
        buf[34..44].copy_from_slice(&self.receive_timestamp.to_wire());

        buf
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncCycleMessage {
    pub sequence_id: u16,
}

impl SyncCycleMessage {
    pub fn new(start: u16) -> Self {
        Self { sequence_id: start }
    }

    pub fn next(self) -> Self {
        Self {
            sequence_id: self.sequence_id.wrapping_add(1),
        }
    }

    pub fn two_step_sync(&self) -> TwoStepSyncMessage {
        TwoStepSyncMessage::new(self.sequence_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayCycleMessage {
    pub sequence_id: u16,
}

impl DelayCycleMessage {
    pub fn new(start: u16) -> Self {
        Self { sequence_id: start }
    }

    pub fn next(self) -> Self {
        Self {
            sequence_id: self.sequence_id.wrapping_add(1),
        }
    }

    pub fn delay_request(&self) -> DelayRequestMessage {
        DelayRequestMessage::new(self.sequence_id)
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

    pub fn timestamp(&self) -> Option<TimeStamp> {
        let secs_msb = u16::from_be_bytes(self.buf[0..2].try_into().ok()?);
        let secs_lsb = u32::from_be_bytes(self.buf[2..6].try_into().ok()?);

        let seconds = ((secs_msb as u64) << 32) | (secs_lsb as u64);
        let nanos = u32::from_be_bytes(self.buf[6..10].try_into().ok()?);

        if nanos < 1_000_000_000 {
            Some(TimeStamp::new(seconds, nanos))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_step_sync_message_wire_roundtrip() {
        let sync = TwoStepSyncMessage::new(42);
        let wire = sync.to_wire();
        let parsed = EventMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, EventMessage::TwoStepSync(sync));
    }

    #[test]
    fn follow_up_message_wire_roundtrip() {
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(1, 2));
        let wire = follow_up.to_wire();
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::FollowUp(follow_up));
    }

    #[test]
    fn delay_request_message_wire_roundtrip() {
        let delay_req = DelayRequestMessage::new(42);
        let wire = delay_req.to_wire();
        let parsed = EventMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, EventMessage::DelayReq(delay_req));
    }

    #[test]
    fn delay_response_message_wire_roundtrip() {
        let delay_resp = DelayResponseMessage::new(42, TimeStamp::new(1, 2));
        let wire = delay_resp.to_wire();
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::DelayResp(delay_resp));
    }

    #[test]
    fn wire_timestamp_roundtrip() {
        let ts = TimeStamp::new(42, 500_000_000);
        let wire = ts.to_wire();
        let parsed = WireTimeStamp::new(&wire).timestamp();

        assert_eq!(parsed, Some(ts));
    }

    #[test]
    fn wire_timestamp_valid_nanos() {
        let wire = [
            0, 0, 0, 0, 0, 42, // seconds
            0, 0, 0, 42, // nanoseconds
        ];
        let parsed = WireTimeStamp::new(&wire).timestamp();

        assert_eq!(parsed, Some(TimeStamp::new(42, 42)));
    }

    #[test]
    fn wire_timestamp_invalid_nanos() {
        let wire = [
            0, 0, 0, 0, 0, 42, // seconds
            0x3B, 0x9A, 0xCA, 0x00, // nanoseconds (1_000_000_000)
        ];
        let parsed = WireTimeStamp::new(&wire).timestamp();

        assert_eq!(parsed, None);
    }

    #[test]
    fn twostep_sync_message_produces_follow_up() {
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = sync.follow_up(TimeStamp::new(4, 0));

        assert_eq!(follow_up, FollowUpMessage::new(42, TimeStamp::new(4, 0)));
    }

    #[test]
    fn follow_up_message_produces_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(4, 0));

        let sync_ingress_timestamp = TimeStamp::new(5, 0);
        let offset = follow_up.master_slave_offset(sync, sync_ingress_timestamp);

        assert_eq!(offset, Some(Duration::new(1, 0)));
    }

    #[test]
    fn follow_up_message_with_different_sequence_id_produces_no_master_slave_offset() {
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(43, TimeStamp::new(4, 0));

        let sync_ingress_timestamp = TimeStamp::new(5, 0);
        let offset = follow_up.master_slave_offset(sync, sync_ingress_timestamp);

        assert_eq!(offset, None);
    }

    #[test]
    fn delay_response_produces_slave_master_offset() {
        let delay_req = DelayRequestMessage::new(42);
        let delay_resp = DelayResponseMessage::new(42, TimeStamp::new(5, 0));

        let delay_req_egress_timestamp = TimeStamp::new(4, 0);
        let offset = delay_resp.slave_master_offset(delay_req, delay_req_egress_timestamp);

        assert_eq!(offset, Some(Duration::new(1, 0)));
    }

    #[test]
    fn delay_response_with_different_sequence_id_produces_no_slave_master_offset() {
        let delay_req = DelayRequestMessage::new(42);
        let delay_resp = DelayResponseMessage::new(43, TimeStamp::new(5, 0));

        let delay_req_egress_timestamp = TimeStamp::new(4, 0);
        let offset = delay_resp.slave_master_offset(delay_req, delay_req_egress_timestamp);

        assert_eq!(offset, None);
    }

    #[test]
    fn sync_cycle_message_produces_two_step_sync_message() {
        let sync_cycle = SyncCycleMessage::new(0);
        let two_step_sync = sync_cycle.two_step_sync();

        assert_eq!(two_step_sync, TwoStepSyncMessage::new(0));
    }

    #[test]
    fn sync_cycle_next() {
        let sync_cycle = SyncCycleMessage::new(0);
        let next = sync_cycle.next();

        assert_eq!(next, SyncCycleMessage::new(1));
    }

    #[test]
    fn sync_cycle_next_wraps() {
        let sync_cycle = SyncCycleMessage::new(u16::MAX);
        let next = sync_cycle.next();

        assert_eq!(next, SyncCycleMessage::new(0));
    }

    #[test]
    fn delay_cycle_message_produces_delay_request_message() {
        let delay_cycle = DelayCycleMessage::new(0);
        let delay_request = delay_cycle.delay_request();

        assert_eq!(delay_request, DelayRequestMessage::new(0));
    }

    #[test]
    fn delay_cycle_next() {
        let delay_cycle = DelayCycleMessage::new(0);
        let next = delay_cycle.next();

        assert_eq!(next, DelayCycleMessage::new(1));
    }

    #[test]
    fn delay_cycle_next_wraps() {
        let delay_cycle = DelayCycleMessage::new(u16::MAX);
        let next = delay_cycle.next();

        assert_eq!(next, DelayCycleMessage::new(0));
    }

    #[test]
    fn delay_request_message_produces_delay_response_message() {
        let delay_req = DelayRequestMessage::new(42);
        let delay_resp = delay_req.response(TimeStamp::new(4, 0));

        assert_eq!(
            delay_resp,
            DelayResponseMessage::new(42, TimeStamp::new(4, 0))
        );
    }
}
