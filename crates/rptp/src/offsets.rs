use std::cell::RefCell;

use crate::message::{
    DelayRequestMessage, DelayResponseMessage, FollowUpMessage, TwoStepSyncMessage,
};
use crate::time::{Duration, TimeStamp};

pub struct MasterSlaveOffset {
    sync: RefCell<Option<(TwoStepSyncMessage, TimeStamp)>>,
    follow_up: RefCell<Option<FollowUpMessage>>,
    offset: RefCell<Option<Duration>>,
}

impl MasterSlaveOffset {
    pub fn new() -> Self {
        Self {
            sync: RefCell::new(None),
            follow_up: RefCell::new(None),
            offset: RefCell::new(None),
        }
    }

    pub fn with_sync(&self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        if let Some(follow_up) = self.follow_up.borrow_mut().take() {
            self.offset
                .replace(follow_up.master_slave_offset(sync, timestamp));
        } else {
            self.sync.replace(Some((sync, timestamp)));
        }
    }

    pub fn with_follow_up(&self, follow_up: FollowUpMessage) {
        if let Some((sync, timestamp)) = self.sync.borrow_mut().take() {
            self.offset
                .replace(follow_up.master_slave_offset(sync, timestamp));
        } else {
            self.follow_up.replace(Some(follow_up));
        }
    }

    pub fn duration(&self) -> Option<Duration> {
        self.offset.borrow_mut().take()
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

    pub fn with_delay_request(&self, req: DelayRequestMessage, timestamp: TimeStamp) {
        if let Some(resp) = self.resp.borrow_mut().take() {
            self.offset
                .replace(resp.slave_master_offset(req, timestamp));
        } else {
            self.req.replace(Some((req, timestamp)));
        }
    }

    pub fn with_delay_response(&self, resp: DelayResponseMessage) {
        if let Some((req, timestamp)) = self.req.borrow_mut().take() {
            self.offset
                .replace(resp.slave_master_offset(req, timestamp));
        } else {
            self.resp.replace(Some(resp));
        }
    }

    pub fn duration(&self) -> Option<Duration> {
        self.offset.borrow_mut().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_slave_offset_new() {
        let offset = MasterSlaveOffset::new();
        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn master_slave_offset_got_sync_only() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);

        offset.with_sync(sync, TimeStamp::new(1, 0));

        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn master_slave_offset_got_follow_up_only() {
        let offset = MasterSlaveOffset::new();
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(1, 0));

        offset.with_follow_up(follow_up);

        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn master_slave_offset_got_sync_then_follow_up_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(2, 0));

        offset.with_sync(sync, TimeStamp::new(3, 0));
        offset.with_follow_up(follow_up);

        assert_eq!(offset.duration(), Some(Duration::new(1, 0)));
    }

    #[test]
    fn master_slave_offset_got_follow_up_then_sync_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(42, TimeStamp::new(3, 0));

        offset.with_follow_up(follow_up);
        offset.with_sync(sync, TimeStamp::new(5, 0));

        assert_eq!(offset.duration(), Some(Duration::new(2, 0)));
    }

    #[test]
    fn master_slave_offset_got_sync_then_follow_up_not_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(43, TimeStamp::new(1, 0));

        offset.with_sync(sync, TimeStamp::new(2, 0));
        offset.with_follow_up(follow_up);

        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn master_slave_offset_got_follow_up_then_sync_not_matching() {
        let offset = MasterSlaveOffset::new();
        let sync = TwoStepSyncMessage::new(42);
        let follow_up = FollowUpMessage::new(43, TimeStamp::new(1, 0));

        offset.with_follow_up(follow_up);
        offset.with_sync(sync, TimeStamp::new(2, 0));

        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn slave_master_offset_new() {
        let offset = SlaveMasterOffset::new();
        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn slave_master_offset_got_req_only() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);

        offset.with_delay_request(req, TimeStamp::new(1, 0));

        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn slave_master_offset_got_resp_only() {
        let offset = SlaveMasterOffset::new();
        let resp = DelayResponseMessage::new(42, TimeStamp::new(1, 0));

        offset.with_delay_response(resp);

        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn slave_master_offset_got_req_then_resp_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(42, TimeStamp::new(2, 0));

        offset.with_delay_request(req, TimeStamp::new(1, 0));
        offset.with_delay_response(resp);

        assert_eq!(offset.duration(), Some(Duration::new(1, 0)));
    }

    #[test]
    fn slave_master_offset_got_resp_then_req_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(42, TimeStamp::new(4, 0));

        offset.with_delay_response(resp);
        offset.with_delay_request(req, TimeStamp::new(3, 0));

        assert_eq!(offset.duration(), Some(Duration::new(1, 0)));
    }

    #[test]
    fn slave_master_offset_got_req_then_resp_not_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(43, TimeStamp::new(2, 0));

        offset.with_delay_request(req, TimeStamp::new(1, 0));
        offset.with_delay_response(resp);

        assert_eq!(offset.duration(), None);
    }

    #[test]
    fn slave_master_offset_got_resp_then_req_not_matching() {
        let offset = SlaveMasterOffset::new();
        let req = DelayRequestMessage::new(42);
        let resp = DelayResponseMessage::new(43, TimeStamp::new(2, 0));

        offset.with_delay_response(resp);
        offset.with_delay_request(req, TimeStamp::new(4, 0));

        assert_eq!(offset.duration(), None);
    }
}
