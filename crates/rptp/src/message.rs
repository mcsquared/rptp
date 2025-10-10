use crate::{
    bmca::ForeignClockDS,
    clock::{ClockIdentity, ClockQuality},
    time::{Duration, TimeStamp},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventMessage {
    DelayReq(DelayRequestMessage),
    TwoStepSync(TwoStepSyncMessage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralMessage {
    Announce(AnnounceMessage),
    DelayResp(DelayResponseMessage),
    FollowUp(FollowUpMessage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMessage {
    AnnounceCycle(AnnounceCycleMessage),
    DelayCycle(DelayCycleMessage),
    SyncCycle(SyncCycleMessage),
    Timestamp {
        msg: EventMessage,
        timestamp: TimeStamp,
    },
    Initialized,
    AnnounceReceiptTimeout,
    QualificationTimeout,
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
            GeneralMessage::Announce(msg) => msg.to_wire(),
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
            0x0B => Ok(Self::Announce(AnnounceMessage::new(
                sequence_id,
                ForeignClockDS::new(
                    ClockIdentity::new([0; 8]),
                    ClockQuality::new(248, 0xFE, 0xFFFF),
                ),
            ))),
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
pub struct AnnounceMessage {
    sequence_id: u16,
    foreign_clock: ForeignClockDS,
}

impl AnnounceMessage {
    pub fn new(sequence_id: u16, foreign_clock: ForeignClockDS) -> Self {
        Self {
            sequence_id,
            foreign_clock,
        }
    }

    pub fn follows(&self, previous: AnnounceMessage) -> Option<ForeignClockDS> {
        if self.sequence_id.wrapping_sub(previous.sequence_id) == 1
            && self.foreign_clock == previous.foreign_clock
        {
            Some(self.foreign_clock)
        } else {
            None
        }
    }

    pub fn foreign_clock(&self) -> &ForeignClockDS {
        &self.foreign_clock
    }

    pub fn to_wire(&self) -> [u8; 64] {
        let mut buf = [0; 64];
        buf[0] = 0x0B & 0x0F;
        buf[30..32].copy_from_slice(&self.sequence_id.to_be_bytes());

        buf
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
pub struct AnnounceCycleMessage {
    pub sequence_id: u16,
}

impl AnnounceCycleMessage {
    pub fn new(start: u16) -> Self {
        Self { sequence_id: start }
    }

    pub fn sequence_id(&self) -> u16 {
        self.sequence_id
    }

    pub fn next(self) -> Self {
        Self {
            sequence_id: self.sequence_id.wrapping_add(1),
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MessageWindow<M> {
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

struct SyncExchange {
    two_step_sync_window: MessageWindow<(TwoStepSyncMessage, TimeStamp)>,
    follow_up_window: MessageWindow<FollowUpMessage>,
}

impl SyncExchange {
    pub fn new() -> Self {
        Self {
            two_step_sync_window: MessageWindow::new(),
            follow_up_window: MessageWindow::new(),
        }
    }

    pub fn ingest_two_step_sync(&mut self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        self.two_step_sync_window.record((sync, timestamp));
    }

    pub fn ingest_follow_up(&mut self, follow_up: FollowUpMessage) {
        self.follow_up_window.record(follow_up);
    }

    pub fn master_slave_offset(&self) -> Option<Duration> {
        self.follow_up_window
            .combine_latest(&self.two_step_sync_window, |follow, &(sync, ts)| {
                follow.master_slave_offset(sync, ts)
            })
    }
}

struct DelayExchange {
    delay_request_window: MessageWindow<(DelayRequestMessage, TimeStamp)>,
    delay_response_window: MessageWindow<DelayResponseMessage>,
}

impl DelayExchange {
    pub fn new() -> Self {
        Self {
            delay_request_window: MessageWindow::new(),
            delay_response_window: MessageWindow::new(),
        }
    }

    pub fn ingest_delay_request(&mut self, req: DelayRequestMessage, timestamp: TimeStamp) {
        self.delay_request_window.record((req, timestamp));
    }

    pub fn ingest_delay_response(&mut self, resp: DelayResponseMessage) {
        self.delay_response_window.record(resp);
    }

    pub fn slave_master_offset(&self) -> Option<Duration> {
        self.delay_response_window
            .combine_latest(&self.delay_request_window, |resp, &(req, ts)| {
                resp.slave_master_offset(req, ts)
            })
    }
}

pub struct MasterEstimate {
    sync_exchange: SyncExchange,
    delay_exchange: DelayExchange,
    t2: Option<TimeStamp>,
}

impl MasterEstimate {
    pub fn new() -> Self {
        Self {
            sync_exchange: SyncExchange::new(),
            delay_exchange: DelayExchange::new(),
            t2: None,
        }
    }

    pub fn ingest_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        timestamp: TimeStamp,
    ) -> Option<TimeStamp> {
        self.sync_exchange.ingest_two_step_sync(sync, timestamp);
        self.t2.replace(timestamp);
        self.estimate()
    }

    pub fn ingest_follow_up(&mut self, follow_up: FollowUpMessage) -> Option<TimeStamp> {
        self.sync_exchange.ingest_follow_up(follow_up);
        self.estimate()
    }

    pub fn ingest_delay_request(
        &mut self,
        req: DelayRequestMessage,
        timestamp: TimeStamp,
    ) -> Option<TimeStamp> {
        self.delay_exchange.ingest_delay_request(req, timestamp);
        self.estimate()
    }

    pub fn ingest_delay_response(&mut self, resp: DelayResponseMessage) -> Option<TimeStamp> {
        self.delay_exchange.ingest_delay_response(resp);
        self.estimate()
    }

    fn estimate(&self) -> Option<TimeStamp> {
        let ms_offset = self.sync_exchange.master_slave_offset()?;
        let sm_offset = self.delay_exchange.slave_master_offset()?;

        if let Some(t2) = self.t2 {
            let offset_from_master = (ms_offset - sm_offset).half();
            Some(t2 - offset_from_master)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_message_wire_roundtrip() {
        let announce = AnnounceMessage::new(
            42,
            ForeignClockDS::new(
                ClockIdentity::new([0; 8]),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
        );
        let wire = announce.to_wire();
        let parsed = GeneralMessage::try_from(wire.as_ref()).unwrap();

        assert_eq!(parsed, GeneralMessage::Announce(announce));
    }

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

    #[test]
    fn message_window_match_latest() {
        let mut sync_window = MessageWindow::new();
        let mut follow_up_window = MessageWindow::new();

        sync_window.record((TwoStepSyncMessage::new(1), TimeStamp::new(2, 0)));
        follow_up_window.record(FollowUpMessage::new(1, TimeStamp::new(1, 0)));

        let offset = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });

        assert_eq!(offset, Some(Duration::new(1, 0)));
    }

    #[test]
    fn message_window_no_match_if_one_empty() {
        let mut sync_window = MessageWindow::new();
        let follow_up_window = MessageWindow::<FollowUpMessage>::new();

        sync_window.record((TwoStepSyncMessage::new(1), TimeStamp::new(2, 0)));

        let offset = follow_up_window.combine_latest(&sync_window, |&follow, &(sync, ts)| {
            follow.master_slave_offset(sync, ts)
        });

        assert_eq!(offset, None);
    }

    #[test]
    fn sync_exchange_new_produces_no_master_slave_offset() {
        let sync_exchange = SyncExchange::new();
        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_no_master_slave_offset_on_two_step_sync_only() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(2, 0));

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_no_master_slave_offset_on_follow_up_only() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_produces_master_slave_offset_on_two_step_sync_then_follow_up() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(2, 0));
        sync_exchange.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn sync_exchange_produces_master_slave_offset_on_follow_up_then_two_step_sync() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(2, 0));

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn sync_exchange_produces_no_master_slave_offset_on_non_matching_follow_up() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(2, 0));
        sync_exchange.ingest_follow_up(FollowUpMessage::new(43, TimeStamp::new(1, 0)));

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_produces_no_master_slave_offset_on_non_matching_two_step_sync() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(43), TimeStamp::new(2, 0));

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_recovers_when_matching_follow_up_arrives_later() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(43), TimeStamp::new(2, 0));
        sync_exchange.ingest_follow_up(FollowUpMessage::new(43, TimeStamp::new(1, 0)));

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn sync_exchange_recovers_when_matching_two_step_sync_arrives_later() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(2, 0));
        sync_exchange.ingest_follow_up(FollowUpMessage::new(43, TimeStamp::new(1, 0)));
        sync_exchange.ingest_two_step_sync(TwoStepSyncMessage::new(43), TimeStamp::new(2, 0));

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_new_produces_no_slave_master_offset() {
        let delay_exchange = DelayExchange::new();
        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_request_only() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(42), TimeStamp::new(1, 0));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_response_only() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(42, TimeStamp::new(1, 0)));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_slave_master_offset_on_request_then_response_matching() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(42), TimeStamp::new(1, 0));
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(42, TimeStamp::new(2, 0)));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_produces_slave_master_offset_on_response_then_request_matching() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(42, TimeStamp::new(2, 0)));
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(42), TimeStamp::new(1, 0));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_non_matching_response() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(42), TimeStamp::new(1, 0));
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(43, TimeStamp::new(2, 0)));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_non_matching_request() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(42, TimeStamp::new(2, 0)));
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(43), TimeStamp::new(1, 0));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_recovers_when_matching_response_arrives_later() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(42, TimeStamp::new(2, 0)));
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(43), TimeStamp::new(1, 0));
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(43, TimeStamp::new(2, 0)));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_recovers_when_matching_request_arrives_later() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(42), TimeStamp::new(1, 0));
        delay_exchange.ingest_delay_response(DelayResponseMessage::new(43, TimeStamp::new(2, 0)));
        delay_exchange.ingest_delay_request(DelayRequestMessage::new(43), TimeStamp::new(1, 0));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(Duration::new(1, 0))
        );
    }

    #[test]
    fn master_estimate_yields_after_sync_and_delay_message_exchange() {
        let mut estimate = MasterEstimate::new();

        estimate.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(1, 0));
        estimate.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));
        estimate.ingest_delay_request(DelayRequestMessage::new(43), TimeStamp::new(0, 0));
        estimate.ingest_delay_response(DelayResponseMessage::new(43, TimeStamp::new(2, 0)));

        assert_eq!(estimate.estimate(), Some(TimeStamp::new(2, 0)));
    }

    #[test]
    fn master_estimate_yields_after_reversed_sync_follow_up_and_delay_message_exchange() {
        let mut estimate = MasterEstimate::new();

        estimate.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));
        estimate.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(1, 0));
        estimate.ingest_delay_request(DelayRequestMessage::new(43), TimeStamp::new(0, 0));
        estimate.ingest_delay_response(DelayResponseMessage::new(43, TimeStamp::new(2, 0)));

        assert_eq!(estimate.estimate(), Some(TimeStamp::new(2, 0)));
    }
}
