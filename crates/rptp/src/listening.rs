use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
};
use crate::log::{PortEvent, PortLog};
use crate::message::AnnounceMessage;
use crate::port::{Port, PortIdentity, PortTimingPolicy, Timeout};
use crate::portstate::{PortState, StateDecision};
use crate::time::Instant;

pub struct ListeningPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    log: L,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> ListeningPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: B,
        announce_receipt_timeout: P::Timeout,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        log.port_event(PortEvent::Static("Become ListeningPort"));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            log,
            timing_policy,
        }
    }

    pub fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedSlave {
            parent: *decision.parent_port_identity(),
        });
        decision.apply(self.port, self.bmca, self.log, self.timing_policy)
    }

    pub fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedMaster);
        decision.apply(self.port, self.bmca, self.log, self.timing_policy)
    }

    pub fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::AnnounceReceiptTimeout);
        let bmca = LocalMasterTrackingBmca::new(self.bmca);
        PortState::master(self.port, bmca, self.log, self.timing_policy)
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(self.timing_policy.announce_receipt_timeout_interval());

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(parent) => Some(StateDecision::RecommendedSlave(parent)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{BmcaMasterDecisionPoint, DefaultDS, ForeignClockDS, IncrementalBmca};
    use crate::clock::{LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::test_support::{FakeClock, FakePort, FakeTimerHost, FakeTimestamping};
    use crate::time::{Duration, Instant, LogMessageInterval};

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = PortState::Listening(ListeningPort::new(
            domain_port,
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        ));

        let transition = listening.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::AnnounceReceiptTimeoutExpired)
        ));
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
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
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        // Drain any initial schedules
        timer_host.take_system_messages();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(transition.is_none());

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_to_pre_master_transition_on_two_announces() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let mut listening = ListeningPort::new(
            domain_port,
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(transition.is_none());

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(matches!(
            transition,
            Some(StateDecision::RecommendedMaster(_))
        ));
    }

    #[test]
    fn listening_port_to_uncalibrated_transition_() {
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
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::high_grade_test_clock();

        // Drain any setup timers
        timer_host.take_system_messages();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(transition.is_none());

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(matches!(
            transition,
            Some(StateDecision::RecommendedSlave(_))
        ));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_m1_master_recommendation() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(5),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let _ = listening.process_announce(
            AnnounceMessage::new(0.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = match transition {
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            decision,
            BmcaMasterDecision::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0))
        );

        let _state = listening.recommended_master(decision);

        assert_eq!(local_clock.steps_removed(), StepsRemoved::new(0));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_m2_master_recommendation() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(5),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::low_grade_test_clock();

        let _ = listening.process_announce(
            AnnounceMessage::new(0.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = match transition {
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            decision,
            BmcaMasterDecision::new(BmcaMasterDecisionPoint::M2, StepsRemoved::new(0))
        );

        let _state = listening.recommended_master(decision);

        assert_eq!(local_clock.steps_removed(), StepsRemoved::new(0));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_s1_slave_recommendation() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(5),
        );
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::high_grade_test_clock();
        let expected_steps_removed = foreign_clock.steps_removed().increment();

        timer_host.take_system_messages();

        let _ = listening.process_announce(
            AnnounceMessage::new(0.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), LogMessageInterval::new(0), foreign_clock),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = match transition {
            Some(StateDecision::RecommendedSlave(decision)) => decision,
            _ => panic!("expected RecommendedSlave decision"),
        };

        let _state = listening.recommended_slave(decision);

        assert_eq!(local_clock.steps_removed(), expected_steps_removed);
    }
}
