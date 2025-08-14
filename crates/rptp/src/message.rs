#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMessage {
    DelayReq,
    Sync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralMessage {
    DelayResp,
    FollowUp(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMessage {
    DelayCycle,
    SyncCycle,
    Timestamp { msg: EventMessage, timestamp: u64 },
}

const DELAY_REQ_BYTES: &[u8] = b"DELAY-REQ";
const DELAY_RESP_BYTES: &[u8] = b"DELAY-RESP";
const SYNC_BYTES: &[u8] = b"SYNC";
const FOLLOW_UP_BYTES: &[u8] = b"FOLLOW-UP";

impl AsRef<[u8]> for EventMessage {
    fn as_ref(&self) -> &[u8] {
        match self {
            EventMessage::DelayReq => DELAY_REQ_BYTES,
            EventMessage::Sync => SYNC_BYTES,
        }
    }
}

impl TryFrom<&[u8]> for EventMessage {
    type Error = ();

    fn try_from(b: &[u8]) -> Result<Self, Self::Error> {
        if b == DELAY_REQ_BYTES {
            Ok(Self::DelayReq)
        } else if b == SYNC_BYTES {
            Ok(Self::Sync)
        } else {
            Err(())
        }
    }
}

impl AsRef<[u8]> for GeneralMessage {
    fn as_ref(&self) -> &[u8] {
        match self {
            GeneralMessage::DelayResp => DELAY_RESP_BYTES,
            GeneralMessage::FollowUp(_) => FOLLOW_UP_BYTES,
        }
    }
}

impl TryFrom<&[u8]> for GeneralMessage {
    type Error = ();

    fn try_from(b: &[u8]) -> Result<Self, Self::Error> {
        if b == DELAY_RESP_BYTES {
            Ok(Self::DelayResp)
        } else if b == FOLLOW_UP_BYTES {
            Ok(Self::FollowUp(0))
        } else {
            Err(())
        }
    }
}
