use crate::bmca::{Bmca, BmcaRecommendation};
use crate::log::PortLog;
use crate::message::AnnounceMessage;
use crate::port::{ParentPortIdentity, Port, PortIdentity, PortTimingPolicy, Timeout};
use crate::portstate::{PortState, StateDecision};

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
        Self {
            port,
            bmca,
            announce_receipt_timeout,
            log,
            timing_policy,
        }
    }

    pub fn recommended_slave(self, parent_port_identity: ParentPortIdentity) -> PortState<P, B, L> {
        self.log.state_transition(
            "Listening",
            "Uncalibrated",
            format!("Recommended Slave, parent {}", parent_port_identity).as_str(),
        );

        PortState::uncalibrated(self.port, self.bmca, self.log, self.timing_policy)
    }

    pub fn recommended_master(self) -> PortState<P, B, L> {
        self.log
            .state_transition("Listening", "Pre-Master", "Recommended Master");

        PortState::pre_master(self.port, self.bmca, self.log, self.timing_policy)
    }

    pub fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log.state_transition(
            "Listening",
            "Master",
            "Announce receipt timeout expired, becoming to Master",
        );

        PortState::master(self.port, self.bmca, self.log, self.timing_policy)
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(self.timing_policy.announce_receipt_timeout_interval());
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateDecision::RecommendedMaster),
            BmcaRecommendation::Slave(parent) => Some(StateDecision::RecommendedSlave(parent)),
            BmcaRecommendation::Undecided => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::bmca::{ForeignClockDS, FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
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

        let mut listening = PortState::Listening(ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        // Drain any initial schedules
        timer_host.take_system_messages();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), foreign_clock),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, None));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_to_pre_master_transition_on_two_announces() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
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
        let mut listening = ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), foreign_clock.clone()),
            PortIdentity::fake(),
        );
        assert!(matches!(transition, None));

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), foreign_clock.clone()),
            PortIdentity::fake(),
        );
        assert!(matches!(transition, Some(StateDecision::RecommendedMaster)));
    }

    #[test]
    fn listening_port_to_uncalibrated_transition_() {
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
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::high_grade_test_clock();

        // Drain any setup timers
        timer_host.take_system_messages();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), foreign_clock),
            PortIdentity::fake(),
        );
        assert!(matches!(transition, None));

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), foreign_clock),
            PortIdentity::fake(),
        );

        assert!(matches!(
            transition,
            Some(StateDecision::RecommendedSlave(_))
        ));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }
}
