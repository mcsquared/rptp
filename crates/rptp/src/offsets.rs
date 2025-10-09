use std::cell::RefCell;

use crate::message::{
    DelayRequestMessage, DelayResponseMessage, FollowUpMessage, TwoStepSyncMessage,
};
use crate::time::{Duration, TimeStamp};

pub struct MasterSlaveOffset {
    sync: RefCell<Option<TwoStepSyncMessage>>,
    sync_timestamp: RefCell<Option<TimeStamp>>,
    follow_up: RefCell<Option<FollowUpMessage>>,
    offset: RefCell<Option<Duration>>,
}

impl MasterSlaveOffset {
    pub fn new() -> Self {
        Self {
            sync: RefCell::new(None),
            sync_timestamp: RefCell::new(None),
            follow_up: RefCell::new(None),
            offset: RefCell::new(None),
        }
    }

    pub fn ingest_two_step_sync(&self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        self.sync_timestamp.replace(Some(timestamp));
        if let Some(follow_up) = self.follow_up.borrow_mut().take() {
            self.offset
                .replace(follow_up.master_slave_offset(sync, timestamp));
        } else {
            self.sync.replace(Some(sync));
        }
    }

    pub fn ingest_follow_up(&self, follow_up: FollowUpMessage) {
        if let (Some(sync), Some(timestamp)) = (
            self.sync.borrow_mut().take(),
            self.sync_timestamp.borrow().as_ref(),
        ) {
            self.offset
                .replace(follow_up.master_slave_offset(sync, *timestamp));
        } else {
            self.follow_up.replace(Some(follow_up));
        }
    }

    pub fn master_estimate(&self, sm_offset: &SlaveMasterOffset) -> Option<TimeStamp> {
        if let (Some(ms_offset), Some(sm_offset), Some(t2)) = (
            self.current(),
            sm_offset.current(),
            self.sync_timestamp.borrow().clone(),
        ) {
            let offset_from_master = (ms_offset - sm_offset).half();
            let estimate = t2 - offset_from_master;
            Some(estimate)
        } else {
            None
        }
    }

    pub fn current(&self) -> Option<Duration> {
        self.offset.borrow().clone()
    }
}

pub struct SlaveMasterOffset {
    req: RefCell<Option<(DelayRequestMessage, TimeStamp)>>,
    resp: RefCell<Option<DelayResponseMessage>>,
    offset: RefCell<Option<Duration>>,
}

impl SlaveMasterOffset {
    pub fn new() -> Self {
        Self {
            req: RefCell::new(None),
            resp: RefCell::new(None),
            offset: RefCell::new(None),
        }
    }

    pub fn ingest_delay_request(&self, req: DelayRequestMessage, timestamp: TimeStamp) {
        if let Some(resp) = self.resp.borrow_mut().take() {
            self.offset
                .replace(resp.slave_master_offset(req, timestamp));
        } else {
            self.req.replace(Some((req, timestamp)));
        }
    }

    pub fn ingest_delay_response(&self, resp: DelayResponseMessage) {
        if let Some((req, timestamp)) = self.req.borrow_mut().take() {
            self.offset
                .replace(resp.slave_master_offset(req, timestamp));
        } else {
            self.resp.replace(Some(resp));
        }
    }

    pub fn current(&self) -> Option<Duration> {
        self.offset.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_slave_offset_new() {
        let offset = MasterSlaveOffset::new();
        assert_eq!(offset.current(), None);
    }

    #[test]
    fn master_slave_offset_got_sync_only() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);

        offset.ingest_two_step_sync(sync, TimeStamp::new(1, 0));

        assert_eq!(offset.current(), None);
    }

    #[test]
    fn master_slave_offset_got_follow_up_only() {
        let offset = MasterSlaveOffset::new();
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(1, 0));

        offset.ingest_follow_up(follow_up);

        assert_eq!(offset.current(), None);
    }

    #[test]
    fn master_slave_offset_got_sync_then_follow_up_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(2, 0));

        offset.ingest_two_step_sync(sync, TimeStamp::new(3, 0));
        offset.ingest_follow_up(follow_up);

        assert_eq!(offset.current(), Some(Duration::new(1, 0)));
    }

    #[test]
    fn master_slave_offset_got_follow_up_then_sync_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(3, 0));

        offset.ingest_follow_up(follow_up);
        offset.ingest_two_step_sync(sync, TimeStamp::new(5, 0));

        assert_eq!(offset.current(), Some(Duration::new(2, 0)));
    }

    #[test]
    fn master_slave_offset_got_sync_then_follow_up_not_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(43, TimeStamp::new(1, 0));

        offset.ingest_two_step_sync(sync, TimeStamp::new(2, 0));
        offset.ingest_follow_up(follow_up);

        assert_eq!(offset.current(), None);
    }

    #[test]
    fn master_slave_offset_got_follow_up_then_sync_not_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(43, TimeStamp::new(1, 0));

        offset.ingest_follow_up(follow_up);
        offset.ingest_two_step_sync(sync, TimeStamp::new(2, 0));

        assert_eq!(offset.current(), None);
    }

    #[test]
    fn master_slave_offset_produces_estimate() {
        let sm_offset = SlaveMasterOffset::new();
        sm_offset.ingest_delay_request(DelayRequestMessage::new(42), TimeStamp::new(0, 0));
        sm_offset.ingest_delay_response(DelayResponseMessage::new(42, TimeStamp::new(2, 0)));

        let ms_offset = MasterSlaveOffset::new();
        ms_offset.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(1, 0));
        ms_offset.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));

        let estimate = ms_offset.master_estimate(&sm_offset);

        assert_eq!(estimate, Some(TimeStamp::new(2, 0)));
    }

    #[test]
    fn master_slave_offset_produces_estimate_with_reversed_sync_follow_up() {
        let sm_offset = SlaveMasterOffset::new();
        sm_offset.ingest_delay_request(DelayRequestMessage::new(42), TimeStamp::new(0, 0));
        sm_offset.ingest_delay_response(DelayResponseMessage::new(42, TimeStamp::new(2, 0)));

        let ms_offset = MasterSlaveOffset::new();
        ms_offset.ingest_follow_up(FollowUpMessage::new(42, TimeStamp::new(1, 0)));
        ms_offset.ingest_two_step_sync(TwoStepSyncMessage::new(42), TimeStamp::new(1, 0));

        let estimate = ms_offset.master_estimate(&sm_offset);

        assert_eq!(estimate, Some(TimeStamp::new(2, 0)));
    }

    #[test]
    fn slave_master_offset_new() {
        let offset = SlaveMasterOffset::new();
        assert_eq!(offset.current(), None);
    }

    #[test]
    fn slave_master_offset_got_req_only() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);

        offset.ingest_delay_request(req, TimeStamp::new(1, 0));

        assert_eq!(offset.current(), None);
    }

    #[test]
    fn slave_master_offset_got_resp_only() {
        let offset = SlaveMasterOffset::new();
        let resp = DelayResponseMessage::new(42, TimeStamp::new(1, 0));

        offset.ingest_delay_response(resp);

        assert_eq!(offset.current(), None);
    }

    #[test]
    fn slave_master_offset_got_req_then_resp_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(42, TimeStamp::new(2, 0));

        offset.ingest_delay_request(req, TimeStamp::new(1, 0));
        offset.ingest_delay_response(resp);

        assert_eq!(offset.current(), Some(Duration::new(1, 0)));
    }

    #[test]
    fn slave_master_offset_got_resp_then_req_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(42, TimeStamp::new(4, 0));

        offset.ingest_delay_response(resp);
        offset.ingest_delay_request(req, TimeStamp::new(3, 0));

        assert_eq!(offset.current(), Some(Duration::new(1, 0)));
    }

    #[test]
    fn slave_master_offset_got_req_then_resp_not_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(43, TimeStamp::new(2, 0));

        offset.ingest_delay_request(req, TimeStamp::new(1, 0));
        offset.ingest_delay_response(resp);

        assert_eq!(offset.current(), None);
    }

    #[test]
    fn slave_master_offset_got_resp_then_req_not_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(43, TimeStamp::new(2, 0));

        offset.ingest_delay_response(resp);
        offset.ingest_delay_request(req, TimeStamp::new(4, 0));

        assert_eq!(offset.current(), None);
    }
}
