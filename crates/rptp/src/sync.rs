use crate::message::{
    DelayRequestMessage, DelayResponseMessage, FollowUpMessage, MessageWindow, OneStepSyncMessage,
    TwoStepSyncMessage,
};
use crate::port::Timeout;
use crate::servo::ServoSample;
use crate::slave::DelayCycle;
use crate::time::{TimeInterval, TimeStamp};

struct SyncExchange {
    one_step_sync: Option<(OneStepSyncMessage, TimeStamp)>,
    two_step_sync_window: MessageWindow<(TwoStepSyncMessage, TimeStamp)>,
    follow_up_window: MessageWindow<FollowUpMessage>,
}

impl SyncExchange {
    pub fn new() -> Self {
        Self {
            one_step_sync: None,
            two_step_sync_window: MessageWindow::new(),
            follow_up_window: MessageWindow::new(),
        }
    }

    pub fn record_one_step_sync(&mut self, sync: OneStepSyncMessage, timestamp: TimeStamp) {
        self.one_step_sync.replace((sync, timestamp));
    }

    pub fn record_two_step_sync(&mut self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        self.one_step_sync = None;
        self.two_step_sync_window.record((sync, timestamp));
    }

    pub fn record_follow_up(&mut self, follow_up: FollowUpMessage) {
        self.follow_up_window.record(follow_up);
    }

    pub fn master_slave_offset(&self) -> Option<TimeInterval> {
        if let Some((sync, ts)) = &self.one_step_sync {
            return Some(sync.master_slave_offset(*ts));
        }

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

    pub fn record_delay_request(&mut self, req: DelayRequestMessage, timestamp: TimeStamp) {
        self.delay_request_window.record((req, timestamp));
    }

    pub fn record_delay_response(&mut self, resp: DelayResponseMessage) {
        self.delay_response_window.record(resp);
    }

    pub fn slave_master_offset(&self) -> Option<TimeInterval> {
        self.delay_response_window
            .combine_latest(&self.delay_request_window, |resp, &(req, ts)| {
                resp.slave_master_offset(req, ts)
            })
    }
}

pub struct EndToEndDelayMechanism<T: Timeout> {
    delay_cycle: DelayCycle<T>,
    sync_exchange: SyncExchange,
    delay_exchange: DelayExchange,
    sync_ingress_timestamp: Option<TimeStamp>,
}

impl<T: Timeout> EndToEndDelayMechanism<T> {
    pub fn new(delay_cycle: DelayCycle<T>) -> Self {
        Self {
            delay_cycle,
            sync_exchange: SyncExchange::new(),
            delay_exchange: DelayExchange::new(),
            sync_ingress_timestamp: None,
        }
    }

    pub fn delay_request(&mut self) -> DelayRequestMessage {
        let delay_request = self.delay_cycle.delay_request();
        self.delay_cycle.next();
        delay_request
    }

    pub fn record_one_step_sync(&mut self, sync: OneStepSyncMessage, timestamp: TimeStamp) {
        self.sync_exchange.record_one_step_sync(sync, timestamp);
        self.sync_ingress_timestamp.replace(timestamp);
    }

    pub fn record_two_step_sync(&mut self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        self.sync_exchange.record_two_step_sync(sync, timestamp);
        self.sync_ingress_timestamp.replace(timestamp);
    }

    pub fn record_follow_up(&mut self, follow_up: FollowUpMessage) {
        self.sync_exchange.record_follow_up(follow_up);
    }

    pub fn record_delay_request(&mut self, req: DelayRequestMessage, timestamp: TimeStamp) {
        self.delay_exchange.record_delay_request(req, timestamp);
    }

    pub fn record_delay_response(&mut self, resp: DelayResponseMessage) {
        self.delay_exchange.record_delay_response(resp);
    }

    pub fn sample(&self) -> Option<ServoSample> {
        let ms_offset = self.sync_exchange.master_slave_offset()?;
        let sm_offset = self.delay_exchange.slave_master_offset()?;

        if let Some(sync_ingress) = self.sync_ingress_timestamp {
            let offset_from_master = (ms_offset - sm_offset).half();
            Some(ServoSample::new(sync_ingress, offset_from_master))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::FakeTimeout;
    use crate::time::{LogInterval, LogMessageInterval};

    use super::*;

    #[test]
    fn sync_exchange_new_produces_no_master_slave_offset() {
        let sync_exchange = SyncExchange::new();
        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_no_master_slave_offset_on_two_step_sync_only() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_no_master_slave_offset_on_follow_up_only() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_produces_master_slave_offset_on_one_step_sync_only() {
        let mut sync_exchange = SyncExchange::new();
        let one_step =
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0));

        sync_exchange.record_one_step_sync(one_step, TimeStamp::new(2, 0));

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn sync_exchange_prefers_one_step_over_two_step_when_both_available() {
        let mut sync_exchange = SyncExchange::new();

        // Two-step path: master at t=1, ingress at t=2 -> offset 1s
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );
        sync_exchange.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));

        // One-step path: master at t=10, ingress at t=13 -> offset 3s
        let one_step =
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(10, 0));
        sync_exchange.record_one_step_sync(one_step, TimeStamp::new(13, 0));

        // One-step should take precedence
        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(TimeInterval::new(3, 0))
        );
    }

    #[test]
    fn sync_exchange_two_step_clears_prior_one_step_sync() {
        let mut sync_exchange = SyncExchange::new();

        // First record a one-step sync: master at t=1, ingress at t=2 -> offset 1s
        let one_step =
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0));
        sync_exchange.record_one_step_sync(one_step, TimeStamp::new(2, 0));

        // Then a two-step sync + follow-up: master at t=4, ingress at t=6 -> offset 2s
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(43.into(), LogMessageInterval::new(0)),
            TimeStamp::new(6, 0),
        );
        sync_exchange.record_follow_up(FollowUpMessage::new(
            43.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(4, 0),
        ));

        // Two-step path should now be used; prior one-step is ignored
        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(TimeInterval::new(2, 0))
        );
    }

    #[test]
    fn sync_exchange_produces_master_slave_offset_on_two_step_sync_then_follow_up() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );
        sync_exchange.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn sync_exchange_produces_master_slave_offset_on_follow_up_then_two_step_sync() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn sync_exchange_produces_no_master_slave_offset_on_non_matching_follow_up() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );
        sync_exchange.record_follow_up(FollowUpMessage::new(
            43.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_produces_no_master_slave_offset_on_non_matching_two_step_sync() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(43.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );

        assert_eq!(sync_exchange.master_slave_offset(), None);
    }

    #[test]
    fn sync_exchange_recovers_when_matching_follow_up_arrives_later() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(43.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );
        sync_exchange.record_follow_up(FollowUpMessage::new(
            43.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn sync_exchange_recovers_when_matching_two_step_sync_arrives_later() {
        let mut sync_exchange = SyncExchange::new();
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );
        sync_exchange.record_follow_up(FollowUpMessage::new(
            43.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));
        sync_exchange.record_two_step_sync(
            TwoStepSyncMessage::new(43.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );

        assert_eq!(
            sync_exchange.master_slave_offset(),
            Some(TimeInterval::new(1, 0))
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
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_response_only() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(42.into(), TimeStamp::new(1, 0)));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_slave_master_offset_on_request_then_response_matching() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(42.into(), TimeStamp::new(2, 0)));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_produces_slave_master_offset_on_response_then_request_matching() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(42.into(), TimeStamp::new(2, 0)));
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_non_matching_response() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_non_matching_request() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(42.into(), TimeStamp::new(2, 0)));
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(1, 0));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_recovers_when_matching_response_arrives_later() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(42.into(), TimeStamp::new(2, 0)));
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(1, 0));
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_recovers_when_matching_request_arrives_later() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));
        delay_exchange
            .record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(1, 0));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn e2e_delay_mechanism_yields_after_sync_and_delay_message_exchange() {
        let mut e2e = EndToEndDelayMechanism::new(DelayCycle::new(
            0.into(),
            FakeTimeout::new(crate::message::SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        ));

        e2e.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(1, 0),
        );
        e2e.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));
        e2e.record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        e2e.record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));

        assert_eq!(
            e2e.sample(),
            Some(ServoSample::new(
                TimeStamp::new(1, 0),
                TimeInterval::new(-1, 0)
            ))
        );
    }

    #[test]
    fn e2e_delay_mechanism_yields_after_reversed_sync_follow_up_and_delay_message_exchange() {
        let mut e2e = EndToEndDelayMechanism::new(DelayCycle::new(
            0.into(),
            FakeTimeout::new(crate::message::SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        ));

        e2e.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));
        e2e.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(1, 0),
        );
        e2e.record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        e2e.record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));

        assert_eq!(
            e2e.sample(),
            Some(ServoSample::new(
                TimeStamp::new(1, 0),
                TimeInterval::new(-1, 0)
            ))
        );
    }

    #[test]
    fn e2e_delay_mechanism_yields_with_one_step_sync() {
        let mut e2e = EndToEndDelayMechanism::new(DelayCycle::new(
            0.into(),
            FakeTimeout::new(crate::message::SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        ));

        e2e.record_one_step_sync(
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            TimeStamp::new(1, 0),
        );
        e2e.record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        e2e.record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));

        assert_eq!(
            e2e.sample(),
            Some(ServoSample::new(
                TimeStamp::new(1, 0),
                TimeInterval::new(-1, 0)
            ))
        );
    }

    #[test]
    fn e2e_delay_mechanism_yields_with_one_step_sync_after_two_step() {
        let mut e2e = EndToEndDelayMechanism::new(DelayCycle::new(
            0.into(),
            FakeTimeout::new(crate::message::SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        ));

        e2e.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(0, 0),
        );
        e2e.record_one_step_sync(
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            TimeStamp::new(1, 0),
        );
        e2e.record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        e2e.record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));

        assert_eq!(
            e2e.sample(),
            Some(ServoSample::new(
                TimeStamp::new(1, 0),
                TimeInterval::new(-1, 0)
            ))
        );
    }

    #[test]
    fn e2e_delay_mechanism_two_step_sync_invalidates_prior_one_step_sync() {
        let mut e2e = EndToEndDelayMechanism::new(DelayCycle::new(
            0.into(),
            FakeTimeout::new(crate::message::SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        ));

        e2e.record_one_step_sync(
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            TimeStamp::new(1, 0),
        );
        e2e.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(0, 0),
        );
        e2e.record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        e2e.record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)));

        assert_eq!(e2e.sample(), None);
    }

    #[test]
    fn e2e_delay_mechanism_yields_with_two_step_and_follow_up_after_one_step() {
        let mut e2e = EndToEndDelayMechanism::new(DelayCycle::new(
            0.into(),
            FakeTimeout::new(crate::message::SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        ));

        e2e.record_one_step_sync(
            OneStepSyncMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            TimeStamp::new(1, 0),
        );
        e2e.record_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            TimeStamp::new(2, 0),
        );
        e2e.record_follow_up(FollowUpMessage::new(
            42.into(),
            LogMessageInterval::new(0),
            TimeStamp::new(1, 0),
        ));
        e2e.record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        e2e.record_delay_response(DelayResponseMessage::new(43.into(), TimeStamp::new(3, 0)));

        assert_eq!(
            e2e.sample(),
            Some(ServoSample::new(
                TimeStamp::new(2, 0),
                TimeInterval::new(-1, 0)
            ))
        );
    }
}
