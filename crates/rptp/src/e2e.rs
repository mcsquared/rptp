//! End-to-end delay mechanism and message exchanges.
//!
//! This module implements the core “clock discipline input” pipeline for the end-to-end (E2E)
//! delay mechanism:
//!
//! - observe message exchanges (Sync + FollowUp, DelayReq + DelayResp),
//! - derive an offset estimate, and
//! - expose it as a [`ServoSample`] that port roles can feed into [`crate::clock::LocalClock`].
//!
//! ## Exchange model
//!
//! - [`SyncExchange`] tracks either:
//!   - a one-step Sync (origin timestamp carried in the Sync message), or
//!   - a two-step Sync + FollowUp pair (origin timestamp carried in FollowUp).
//! - [`DelayExchange`] tracks DelayReq + DelayResp pairs.
//! - [`MessageWindow`] is a minimal “latest matching pair” helper used by both exchanges to deal
//!   with out-of-order arrivals. It is intentionally simple at the moment and subject to future
//!   extension for more complex and robust strategies (loss/reordering strategies, multiple
//!   outstanding requests, etc.).

use crate::message::{
    DelayRequestMessage, DelayResponseMessage, FollowUpMessage, OneStepSyncMessage, SequenceId,
    TwoStepSyncMessage,
};
use crate::port::Timeout;
use crate::servo::ServoSample;
use crate::time::{LogInterval, TimeInterval, TimeStamp};

/// End-to-end (E2E) delay mechanism state for producing [`ServoSample`]s.
///
/// This object is owned by port roles (e.g. slave/uncalibrated) and fed by message reception:
///
/// - `record_*` methods retain the latest relevant message fragments (and timestamps).
/// - [`sample`](Self::sample) attempts to combine the currently known fragments into an offset
///   estimate.
/// - [`delay_request`](Self::delay_request) produces the next DelayReq message to send and advances
///   its scheduling/sequence state.
///
/// ### One-step vs two-step Sync precedence
///
/// - Recording a **two-step** Sync invalidates any previously recorded **one-step** Sync.
/// - Recording a **one-step** Sync takes precedence over any currently stored two-step fragments
///   until a two-step Sync is recorded again.
pub struct EndToEndDelayMechanism<T: Timeout> {
    delay_cycle: DelayCycle<T>,
    sync_exchange: SyncExchange,
    delay_exchange: DelayExchange,
    sync_ingress_timestamp: Option<TimeStamp>,
}

impl<T: Timeout> EndToEndDelayMechanism<T> {
    /// Create a new E2E delay mechanism with the given DelayReq scheduling cycle.
    pub fn new(delay_cycle: DelayCycle<T>) -> Self {
        Self {
            delay_cycle,
            sync_exchange: SyncExchange::new(),
            delay_exchange: DelayExchange::new(),
            sync_ingress_timestamp: None,
        }
    }

    /// Produce the next DelayReq message to send and advance the delay-request cycle.
    pub(crate) fn delay_request(&mut self) -> DelayRequestMessage {
        let delay_request = self.delay_cycle.delay_request();
        self.delay_cycle.next();
        delay_request
    }

    /// Record a received one-step Sync and its ingress timestamp.
    pub(crate) fn record_one_step_sync(&mut self, sync: OneStepSyncMessage, timestamp: TimeStamp) {
        self.sync_exchange.record_one_step_sync(sync, timestamp);
        self.sync_ingress_timestamp.replace(timestamp);
    }

    /// Record a received two-step Sync and its ingress timestamp.
    pub(crate) fn record_two_step_sync(&mut self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        self.sync_exchange.record_two_step_sync(sync, timestamp);
        self.sync_ingress_timestamp.replace(timestamp);
    }

    /// Record a received FollowUp message.
    pub(crate) fn record_follow_up(&mut self, follow_up: FollowUpMessage) {
        self.sync_exchange.record_follow_up(follow_up);
    }

    /// Record a sent DelayReq and its egress timestamp.
    pub(crate) fn record_delay_request(&mut self, req: DelayRequestMessage, timestamp: TimeStamp) {
        self.delay_exchange.record_delay_request(req, timestamp);
    }

    /// Record a received DelayResp message.
    pub(crate) fn record_delay_response(&mut self, resp: DelayResponseMessage) {
        self.delay_exchange.record_delay_response(resp);
    }

    /// Attempt to produce a [`ServoSample`] from the currently known message fragments.
    ///
    /// This returns `None` until both:
    /// - the Sync side yields a master→slave offset estimate, and
    /// - the Delay side yields a slave→master offset estimate.
    ///
    /// The resulting sample uses the latest Sync ingress timestamp as its “sample time”.
    pub(crate) fn sample(&self) -> Option<ServoSample> {
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

/// Tracks the most recent Sync observation(s) and yields a master→slave offset estimate.
///
/// - One-step Sync carries the origin timestamp directly.
/// - Two-step Sync requires a matching FollowUp for the same sequence id.
struct SyncExchange {
    one_step_sync: Option<(OneStepSyncMessage, TimeStamp)>,
    two_step_sync_window: MessageWindow<(TwoStepSyncMessage, TimeStamp)>,
    follow_up_window: MessageWindow<FollowUpMessage>,
}

impl SyncExchange {
    fn new() -> Self {
        Self {
            one_step_sync: None,
            two_step_sync_window: MessageWindow::new(),
            follow_up_window: MessageWindow::new(),
        }
    }

    fn record_one_step_sync(&mut self, sync: OneStepSyncMessage, timestamp: TimeStamp) {
        self.one_step_sync.replace((sync, timestamp));
    }

    fn record_two_step_sync(&mut self, sync: TwoStepSyncMessage, timestamp: TimeStamp) {
        self.one_step_sync = None;
        self.two_step_sync_window.record((sync, timestamp));
    }

    fn record_follow_up(&mut self, follow_up: FollowUpMessage) {
        self.follow_up_window.record(follow_up);
    }

    /// Yield the current master→slave offset estimate, if available.
    fn master_slave_offset(&self) -> Option<TimeInterval> {
        if let Some((sync, ts)) = &self.one_step_sync {
            return Some(sync.master_slave_offset(*ts));
        }

        self.follow_up_window
            .combine_latest(&self.two_step_sync_window, |follow_up, &(sync, ts)| {
                follow_up.master_slave_offset(sync, ts)
            })
    }
}

/// Tracks the most recent DelayReq/DelayResp observations and yields a slave→master offset estimate.
struct DelayExchange {
    delay_request_window: MessageWindow<(DelayRequestMessage, TimeStamp)>,
    delay_response_window: MessageWindow<DelayResponseMessage>,
}

impl DelayExchange {
    fn new() -> Self {
        Self {
            delay_request_window: MessageWindow::new(),
            delay_response_window: MessageWindow::new(),
        }
    }

    fn record_delay_request(&mut self, req: DelayRequestMessage, timestamp: TimeStamp) {
        self.delay_request_window.record((req, timestamp));
    }

    fn record_delay_response(&mut self, resp: DelayResponseMessage) {
        self.delay_response_window.record(resp);
    }

    /// Yield the current slave→master offset estimate, if available.
    fn slave_master_offset(&self) -> Option<TimeInterval> {
        self.delay_response_window
            .combine_latest(&self.delay_request_window, |resp, &(req, ts)| {
                resp.slave_master_offset(req, ts)
            })
    }
}

/// DelayReq production and scheduling state.
///
/// This owns the DelayReq sequence counter and a timeout handle. Calling [`next`](Self::next)
/// schedules the next request and advances the sequence id.
#[derive(Debug, PartialEq, Eq)]
pub struct DelayCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
    log_interval: LogInterval,
}

impl<T: Timeout> DelayCycle<T> {
    /// Create a new delay-request cycle.
    ///
    /// - `start` is the first DelayReq sequence id to use.
    /// - `delay_request_timeout` is restarted on each [`next`](Self::next).
    /// - `log_interval` controls the request spacing.
    pub fn new(start: SequenceId, delay_request_timeout: T, log_interval: LogInterval) -> Self {
        Self {
            sequence_id: start,
            timeout: delay_request_timeout,
            log_interval,
        }
    }

    /// Schedule the next DelayReq and advance the sequence id.
    pub(crate) fn next(&mut self) {
        self.timeout.restart(self.log_interval.duration());
        self.sequence_id = self.sequence_id.next();
    }

    /// Build a DelayReq message for the current sequence id.
    pub(crate) fn delay_request(&self) -> DelayRequestMessage {
        DelayRequestMessage::new(self.sequence_id)
    }
}

/// “Latest message wins” cache with an optional combiner.
///
/// This is a deliberately tiny helper to model the behaviour needed by the message exchanges: keep
/// the latest message of a kind and attempt to combine it with the latest message of another kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MessageWindow<M> {
    current: Option<M>,
}

impl<M> MessageWindow<M> {
    /// Create an empty window.
    fn new() -> Self {
        Self { current: None }
    }

    /// Record a new message, replacing any previously stored value.
    fn record(&mut self, msg: M) {
        self.current.replace(msg);
    }

    /// Combine the latest message in `self` with the latest message in `other`.
    ///
    /// The `combine` closure can decide whether the pair “matches” (e.g. by comparing sequence ids)
    /// by returning `Some(T)` for matching pairs and `None` otherwise.
    fn combine_latest<N, F, T>(&self, other: &MessageWindow<N>, combine: F) -> Option<T>
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

    use crate::message::SystemMessage;
    use crate::port::PortIdentity;
    use crate::test_support::FakeTimeout;
    use crate::time::{LogInterval, LogMessageInterval};

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
        e2e.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(2),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

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
        e2e.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(2),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

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
        e2e.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(2),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

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
        e2e.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(2),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

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
        e2e.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(2),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

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
        e2e.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(2),
            TimeStamp::new(3, 0),
            PortIdentity::fake(),
        ));

        assert_eq!(
            e2e.sample(),
            Some(ServoSample::new(
                TimeStamp::new(2, 0),
                TimeInterval::new(-1, 0)
            ))
        );
    }

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
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            42.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(1, 0),
            PortIdentity::fake(),
        ));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_slave_master_offset_on_request_then_response_matching() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            42.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(TimeInterval::new(1, 0))
        );
    }

    #[test]
    fn delay_exchange_produces_slave_master_offset_on_response_then_request_matching() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            42.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));
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
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_produces_no_slave_master_offset_on_non_matching_request() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            42.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(1, 0));

        assert_eq!(delay_exchange.slave_master_offset(), None);
    }

    #[test]
    fn delay_exchange_recovers_when_matching_response_arrives_later() {
        let mut delay_exchange = DelayExchange::new();
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            42.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(1, 0));
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));

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
        delay_exchange.record_delay_response(DelayResponseMessage::new(
            43.into(),
            LogMessageInterval::new(1),
            TimeStamp::new(2, 0),
            PortIdentity::fake(),
        ));
        delay_exchange
            .record_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(1, 0));

        assert_eq!(
            delay_exchange.slave_master_offset(),
            Some(TimeInterval::new(1, 0))
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
    fn delay_cycle_produces_delay_request_message() {
        let delay_cycle = DelayCycle::new(
            42.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        );
        let delay_request = delay_cycle.delay_request();

        assert_eq!(delay_request, DelayRequestMessage::new(42.into()));
    }

    #[test]
    fn delay_cycle_next() {
        let mut delay_cycle = DelayCycle::new(
            42.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        );
        delay_cycle.next();

        assert_eq!(
            delay_cycle,
            DelayCycle::new(
                43.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                LogInterval::new(0)
            )
        );
    }

    #[test]
    fn delay_cycle_next_wraps() {
        let mut delay_cycle = DelayCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
            LogInterval::new(0),
        );
        delay_cycle.next();

        assert_eq!(
            delay_cycle,
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                LogInterval::new(0),
            )
        );
    }
}
