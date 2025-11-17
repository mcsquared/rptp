use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::log::Log;
use crate::message::AnnounceMessage;
use crate::port::{Port, PortIdentity, Timeout};
use crate::portstate::{PortState, StateTransition};
use crate::uncalibrated::UncalibratedPort;

pub struct ListeningPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> ListeningPort<P, B, L> {
    pub fn new(port: P, bmca: B, announce_receipt_timeout: P::Timeout, log: L) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
            log,
        }
    }

    pub fn to_uncalibrated(self) -> PortState<P, B, L> {
        PortState::Uncalibrated(UncalibratedPort::new(
            self.port,
            self.bmca,
            self.announce_receipt_timeout,
            self.log,
        ))
    }

    pub fn to_pre_master(self) -> PortState<P, B, L> {
        PortState::pre_master(self.port, self.bmca, self.log)
    }

    pub fn to_master(self) -> PortState<P, B, L> {
        PortState::master(self.port, self.bmca, self.log)
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToPreMaster),
            BmcaRecommendation::Slave(_) => Some(StateTransition::ToUncalibrated),
            BmcaRecommendation::Undecided => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{ForeignClockDS, FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};

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
            NoopLog,
        ));

        let transition = listening.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(transition, Some(StateTransition::ToMaster)));
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
            NoopLog,
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
            NoopLog,
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
        assert!(matches!(transition, Some(StateTransition::ToPreMaster)));
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
            NoopLog,
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
        assert!(matches!(transition, Some(StateTransition::ToUncalibrated)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }
}
