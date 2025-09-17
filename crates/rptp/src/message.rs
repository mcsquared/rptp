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
    DelayCycle,
    SyncCycle,
    Timestamp {
        msg: EventMessage,
        timestamp: TimeStamp,
    },
}

const DELAY_REQ_BYTES: &[u8] = b"DELAY-REQ";
const DELAY_RESP_BYTES: &[u8] = b"DELAY-RESP";
const SYNC_BYTES: &[u8] = b"SYNC";
const FOLLOW_UP_BYTES: &[u8] = b"FOLLOW-UP";

impl AsRef<[u8]> for EventMessage {
    fn as_ref(&self) -> &[u8] {
        match self {
            EventMessage::DelayReq(_) => DELAY_REQ_BYTES,
            EventMessage::TwoStepSync(_) => SYNC_BYTES,
        }
    }
}

impl TryFrom<&[u8]> for EventMessage {
    type Error = ();

    fn try_from(b: &[u8]) -> Result<Self, Self::Error> {
        if b == DELAY_REQ_BYTES {
            Ok(Self::DelayReq(DelayRequestMessage::new(0)))
        } else if b == SYNC_BYTES {
            Ok(Self::TwoStepSync(TwoStepSyncMessage::new(0)))
        } else {
            Err(())
        }
    }
}

impl AsRef<[u8]> for GeneralMessage {
    fn as_ref(&self) -> &[u8] {
        match self {
            GeneralMessage::DelayResp(_) => DELAY_RESP_BYTES,
            GeneralMessage::FollowUp(_) => FOLLOW_UP_BYTES,
        }
    }
}

impl TryFrom<&[u8]> for GeneralMessage {
    type Error = ();

    fn try_from(b: &[u8]) -> Result<Self, Self::Error> {
        if b == DELAY_RESP_BYTES {
            Ok(Self::DelayResp(DelayResponseMessage::new(
                0,
                TimeStamp::new(0, 0),
            )))
        } else if b == FOLLOW_UP_BYTES {
            Ok(Self::FollowUp(FollowUpMessage::new(
                0,
                TimeStamp::new(0, 0),
            )))
        } else {
            Err(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwoStepSyncMessage {
    sequence_id: u32,
}

impl TwoStepSyncMessage {
    pub fn new(sequence_id: u32) -> Self {
        Self { sequence_id }
    }

    pub fn follow_up(self, precise_origin_timestamp: TimeStamp) -> FollowUpMessage {
        FollowUpMessage::new(self.sequence_id, precise_origin_timestamp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowUpMessage {
    sequence_id: u32,
    precise_origin_timestamp: TimeStamp,
}

impl FollowUpMessage {
    pub fn new(sequence_id: u32, precise_origin_timestamp: TimeStamp) -> Self {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayRequestMessage {
    sequence_id: u32,
}

impl DelayRequestMessage {
    pub fn new(sequence_id: u32) -> Self {
        Self { sequence_id }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayResponseMessage {
    sequence_id: u32,
    origin_timestamp: TimeStamp,
}

impl DelayResponseMessage {
    pub fn new(sequence_id: u32, origin_timestamp: TimeStamp) -> Self {
        Self {
            sequence_id,
            origin_timestamp,
        }
    }

    pub fn slave_master_offset(
        &self,
        delay_req: DelayRequestMessage,
        delay_req_egress_timestamp: TimeStamp,
    ) -> Option<Duration> {
        if self.sequence_id == delay_req.sequence_id {
            Some(self.origin_timestamp - delay_req_egress_timestamp)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
