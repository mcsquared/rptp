use std::time::Duration;

use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
    ParentTrackingBmca,
};
use crate::log::PortLog;
use crate::message::{
    AnnounceMessage, DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
    SequenceId, TwoStepSyncMessage,
};
use crate::port::{Port, PortIdentity, PortTimingPolicy, Timeout};
use crate::portstate::{PortState, StateDecision};
use crate::sync::MasterEstimate;
use crate::time::TimeStamp;

pub struct SlavePort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: ParentTrackingBmca<B>,
    announce_receipt_timeout: P::Timeout,
    delay_cycle: DelayCycle<P::Timeout>,
    master_estimate: MasterEstimate,
    log: L,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> SlavePort<P, B, L> {
    pub fn new(
        port: P,
        bmca: ParentTrackingBmca<B>,
        announce_receipt_timeout: P::Timeout,
        delay_cycle: DelayCycle<P::Timeout>,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
            delay_cycle,
            master_estimate: MasterEstimate::new(),
            log,
            timing_policy,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(self.timing_policy.announce_receipt_timeout_interval());

        msg.feed_bmca(&mut self.bmca, source_port_identity);

        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => None,
        }
    }

    pub fn process_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("Sync");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self
            .master_estimate
            .record_two_step_sync(sync, ingress_timestamp)
        {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn process_follow_up(
        &mut self,
        follow_up: FollowUpMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("FollowUp");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self.master_estimate.record_follow_up(follow_up) {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        egress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("DelayReq");
        if let Some(estimate) = self
            .master_estimate
            .record_delay_request(req, egress_timestamp)
        {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn process_delay_response(
        &mut self,
        resp: DelayResponseMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("DelayResp");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self.master_estimate.record_delay_response(resp) {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn send_delay_request(&mut self) {
        let delay_request = self.delay_cycle.delay_request();
        self.port.send_event(EventMessage::DelayReq(delay_request));
        self.delay_cycle
            .next(self.timing_policy.min_delay_request_interval());
        self.log.message_sent("DelayReq");
    }

    pub fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log
            .state_transition("Slave", "Master", "Announce receipt timeout expired");

        let bmca = LocalMasterTrackingBmca::new(self.bmca.into_inner());

        PortState::master(self.port, bmca, self.log, self.timing_policy)
    }

    pub fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        self.log.state_transition(
            "Slave",
            "Uncalibrated",
            format!(
                "Recommended Slave, parent {}",
                decision.parent_port_identity()
            )
            .as_str(),
        );

        decision.apply(
            self.port,
            self.bmca.into_inner(),
            self.log,
            self.timing_policy,
        )
    }

    pub fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log
            .state_transition("Slave", "Pre-Master", "Recommended Master");

        decision.apply(
            self.port,
            self.bmca.into_inner(),
            self.log,
            self.timing_policy,
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct DelayCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
}

impl<T: Timeout> DelayCycle<T> {
    pub fn new(start: SequenceId, delay_request_timeout: T) -> Self {
        Self {
            sequence_id: start,
            timeout: delay_request_timeout,
        }
    }

    pub fn next(&mut self, interval: Duration) {
        self.timeout.restart(interval);
        self.sequence_id = self.sequence_id.next();
    }

    pub fn delay_request(&self) -> DelayRequestMessage {
        DelayRequestMessage::new(self.sequence_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{DefaultDS, ForeignClockDS, ForeignClockRecord, IncrementalBmca};
    use crate::clock::{ClockIdentity, FakeClock, LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimeout, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::portstate::PortState;

    #[test]
    fn slave_port_synchronizes_clock() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = SlavePort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            FakeTimeout::new(SystemMessage::AnnounceReceiptTimeout),
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        slave.process_two_step_sync(
            TwoStepSyncMessage::new(0.into()),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );
        slave.process_follow_up(
            FollowUpMessage::new(0.into(), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
        );
        slave.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));
        slave.process_delay_response(
            DelayResponseMessage::new(0.into(), TimeStamp::new(2, 0)),
            PortIdentity::fake(),
        );

        assert_eq!(local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_schedules_next_delay_request_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = PortState::slave(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        timer_host.take_system_messages();

        slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayRequestTimeout));
    }

    #[test]
    fn slave_port_answers_delay_request_timeout_with_delay_request() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = PortState::slave(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        let events = port.take_event_messages();
        assert!(events.contains(&EventMessage::DelayReq(DelayRequestMessage::new(0.into()))));
    }

    #[test]
    fn slave_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = PortState::slave(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let transition = slave.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::AnnounceReceiptTimeoutExpired)
        ));
    }

    #[test]
    fn slave_port_ignores_general_messages_from_non_parent() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        // Define a parent and a different non-parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );
        let non_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        // Create slave with a chosen parent
        let mut slave = SlavePort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(parent),
            ),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Record a TwoStepSync from the parent so a matching FollowUp could produce ms_offset
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into()),
            parent,
            TimeStamp::new(2, 0),
        );
        assert!(matches!(transition, None));

        // Record a delay request timestamp to allow sm_offset calculation
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(matches!(transition, None));

        // Send FollowUp and DelayResp from a non-parent; these should be ignored
        let transition = slave.process_follow_up(
            FollowUpMessage::new(1.into(), TimeStamp::new(1, 0)),
            non_parent,
        );
        assert!(matches!(transition, None));

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(2.into(), TimeStamp::new(2, 0)),
            non_parent,
        );
        assert!(matches!(transition, None));

        // With correct filtering, the local clock should remain unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_ignores_event_messages_from_non_parent() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        // Define a parent and a different non-parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );
        let non_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        // Create slave with chosen parent
        let mut slave = SlavePort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(parent),
            ),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Send a FollowUp from the parent first (ms offset incomplete without sync)
        let transition =
            slave.process_follow_up(FollowUpMessage::new(1.into(), TimeStamp::new(1, 0)), parent);
        assert!(matches!(transition, None));

        // Now send TwoStepSync from a non-parent; should be ignored
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into()),
            non_parent,
            TimeStamp::new(2, 0),
        );
        assert!(matches!(transition, None));

        // Even if delay path completes, estimate should not trigger without accepted sync
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(matches!(transition, None));

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(2.into(), TimeStamp::new(2, 0)),
            parent,
        );
        assert!(matches!(transition, None));

        // Local clock remains unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_disciplines_on_matching_conversation() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        // Parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xCC, 0xCC, 0xCC]),
            PortNumber::new(1),
        );

        // Create slave with parent
        let mut slave = SlavePort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(parent),
            ),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Matching conversation from the parent (numbers chosen to yield estimate 2s)
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(42.into()),
            parent,
            TimeStamp::new(1, 0),
        );
        assert!(matches!(transition, None));

        let transition = slave.process_follow_up(
            FollowUpMessage::new(42.into(), TimeStamp::new(1, 0)),
            parent,
        );
        assert!(matches!(transition, None));

        let transition =
            slave.process_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        assert!(matches!(transition, None));

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)),
            parent,
        );
        assert!(matches!(transition, None));

        // Local clock disciplined to the estimate
        assert_eq!(local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_produces_no_decision_on_updated_same_parent() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = ForeignClockDS::mid_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(parent_port, foreign_clock_ds).qualify()];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        let mut slave = SlavePort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Receive a better announce from the same parent port
        let decision = slave.process_announce(
            AnnounceMessage::new(42.into(), ForeignClockDS::high_grade_test_clock()),
            parent_port,
        );

        // expect a no decision since parent is unchanged
        assert_eq!(decision, None);
    }

    #[test]
    fn slave_port_produces_slave_recommendation_with_new_parent() {
        use crate::bmca::{ForeignClockDS, ForeignClockRecord};

        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = ForeignClockDS::mid_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(parent_port, foreign_clock_ds).qualify()];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        let mut slave = SlavePort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Receive two better announces from another parent port
        let new_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );
        let decision = slave.process_announce(
            AnnounceMessage::new(42.into(), ForeignClockDS::high_grade_test_clock()),
            new_parent,
        );
        assert!(matches!(decision, None)); // first announce from new parent is ignored

        let decision = slave.process_announce(
            AnnounceMessage::new(43.into(), ForeignClockDS::high_grade_test_clock()),
            new_parent,
        );

        // expect a slave recommendation
        assert_eq!(
            decision,
            Some(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(new_parent),
                StepsRemoved::new(1),
            )))
        );
    }

    #[test]
    fn slave_port_produces_master_recommendation_on_worse_announces() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(parent_port, foreign_clock_ds).qualify()];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        let mut slave = SlavePort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Receive a worse announce from the current parent
        let decision = slave.process_announce(
            AnnounceMessage::new(42.into(), ForeignClockDS::low_grade_test_clock()),
            parent_port,
        );

        // expect a master recommendation since local clock is better
        assert!(matches!(
            decision,
            Some(StateDecision::RecommendedMaster(_))
        ));
    }

    #[test]
    fn delay_cycle_produces_delay_request_message() {
        let delay_cycle = DelayCycle::new(
            42.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        let delay_request = delay_cycle.delay_request();

        assert_eq!(delay_request, DelayRequestMessage::new(42.into()));
    }

    #[test]
    fn delay_cycle_next() {
        let mut delay_cycle = DelayCycle::new(
            42.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        delay_cycle.next(Duration::from_secs(1));

        assert_eq!(
            delay_cycle,
            DelayCycle::new(
                43.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout)
            )
        );
    }

    #[test]
    fn delay_cycle_next_wraps() {
        let mut delay_cycle = DelayCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        delay_cycle.next(Duration::from_secs(1));

        assert_eq!(
            delay_cycle,
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout)
            )
        );
    }
}
