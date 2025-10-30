use crate::message::{
    DelayRequestMessage, DelayResponseMessage, FollowUpMessage, MessageWindow, TwoStepSyncMessage,
};
use crate::time::{Duration, TimeStamp};

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
            .combine_latest(&self.two_step_sync_window, |follow_up, &(sync, ts)| {
                follow_up.master_slave_offset(sync, ts)
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
    sync_ingress_timestamp: Option<TimeStamp>,
}

impl MasterEstimate {
    pub fn new() -> Self {
        Self {
            sync_exchange: SyncExchange::new(),
            delay_exchange: DelayExchange::new(),
            sync_ingress_timestamp: None,
        }
    }

    pub fn ingest_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        timestamp: TimeStamp,
    ) -> Option<TimeStamp> {
        self.sync_exchange.ingest_two_step_sync(sync, timestamp);
        self.sync_ingress_timestamp.replace(timestamp);
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

        if let Some(sync_ingress) = self.sync_ingress_timestamp {
            let offset_from_master = (ms_offset - sm_offset).half();
            Some(sync_ingress - offset_from_master)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
