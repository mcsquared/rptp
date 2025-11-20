use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::log::PortLog;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::{
    AnnounceMessage, DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
    SequenceId, SystemMessage, TwoStepSyncMessage,
};
use crate::port::{ParentPortIdentity, Port, PortIdentity, Timeout};
use crate::portstate::StateDecision;
use crate::sync::MasterEstimate;
use crate::time::TimeStamp;

pub struct SlavePort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    delay_cycle: DelayCycle<P::Timeout>,
    master_estimate: MasterEstimate,
    parent_port_identity: ParentPortIdentity,
    log: L,
}

impl<P: Port, B: Bmca, L: PortLog> SlavePort<P, B, L> {
    pub fn new(
        port: P,
        bmca: B,
        parent_port_identity: ParentPortIdentity,
        announce_receipt_timeout: P::Timeout,
        delay_cycle: DelayCycle<P::Timeout>,
        log: L,
    ) -> Self {
        Self {
            port,
            bmca,
            parent_port_identity,
            announce_receipt_timeout,
            delay_cycle,
            master_estimate: MasterEstimate::new(),
            log,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateDecision::RecommendedMaster),
            BmcaRecommendation::Slave(parent) => {
                self.parent_port_identity = parent;
                None
            }
            BmcaRecommendation::Undecided => None,
        }
    }

    pub fn process_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("Sync");
        if !self.parent_port_identity.matches(&source_port_identity) {
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
        if !self.parent_port_identity.matches(&source_port_identity) {
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
        if !self.parent_port_identity.matches(&source_port_identity) {
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
        self.delay_cycle.next();
        self.log.message_sent("DelayReq");
    }

    pub fn announce_receipt_timeout_expired(self) -> MasterPort<P, B, L> {
        self.log
            .state_transition("Slave", "Master", "Announce receipt timeout expired");

        let announce_send_timeout = self
            .port
            .timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0));
        let announce_cycle = AnnounceCycle::new(0.into(), announce_send_timeout);
        let sync_timeout = self
            .port
            .timeout(SystemMessage::SyncTimeout, Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout);

        MasterPort::new(self.port, self.bmca, announce_cycle, sync_cycle, self.log)
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

    pub fn next(&mut self) {
        self.timeout.restart(Duration::from_secs(1));
        self.sequence_id = self.sequence_id.next();
    }

    pub fn delay_request(&self) -> DelayRequestMessage {
        DelayRequestMessage::new(self.sequence_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{FullBmca, LocalClockDS};
    use crate::clock::{ClockIdentity, FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimeout, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;

    #[test]
    fn slave_port_synchronizes_clock() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            FakeTimeout::new(SystemMessage::AnnounceReceiptTimeout),
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
            ),
            NoopPortLog,
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
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopPortLog,
        );

        timer_host.take_system_messages();

        slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayRequestTimeout));
    }

    #[test]
    fn slave_port_answers_delay_request_timeout_with_delay_request() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopPortLog,
        );

        slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        let events = port.take_event_messages();
        assert!(events.contains(&EventMessage::DelayReq(DelayRequestMessage::new(0.into()))));
    }

    #[test]
    fn slave_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = PortState::slave(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopPortLog,
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
            LocalClockDS::mid_grade_test_clock(),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
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
            bmca,
            ParentPortIdentity::new(parent),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
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
            LocalClockDS::mid_grade_test_clock(),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
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
            bmca,
            ParentPortIdentity::new(parent),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
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
            LocalClockDS::mid_grade_test_clock(),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
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
            bmca,
            ParentPortIdentity::new(parent),
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
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
        delay_cycle.next();

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
        delay_cycle.next();

        assert_eq!(
            delay_cycle,
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout)
            )
        );
    }
}
