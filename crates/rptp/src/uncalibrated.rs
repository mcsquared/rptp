use crate::bmca::{Bmca, BmcaRecommendation};
use crate::log::PortLog;
use crate::message::AnnounceMessage;
use crate::port::{ParentPortIdentity, Port, PortIdentity, PortTimingPolicy, Timeout};
use crate::portstate::{PortState, StateDecision};

pub struct UncalibratedPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    log: L,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> UncalibratedPort<P, B, L> {
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
            BmcaRecommendation::Master(qualification_timeout_policy) => Some(
                StateDecision::RecommendedMaster(qualification_timeout_policy),
            ),
            BmcaRecommendation::Slave(parent) => Some(StateDecision::MasterClockSelected(parent)),
            BmcaRecommendation::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaRecommendation::Undecided => None,
        }
    }

    pub fn master_clock_selected(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> PortState<P, B, L> {
        self.log.state_transition(
            "Uncalibrated",
            "Slave",
            format!("Master clock selected, parent {}", parent_port_identity).as_str(),
        );

        PortState::slave(
            self.port,
            self.bmca,
            parent_port_identity,
            self.log,
            self.timing_policy,
        )
    }

    pub fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log
            .state_transition("Uncalibrated", "Master", "Announce receipt timeout expired");

        PortState::master(self.port, self.bmca, self.log, self.timing_policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::bmca::{ForeignClockDS, ForeignClockRecord, FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;

    #[test]
    fn uncalibrated_port_to_slave_transition_on_following_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(
            PortIdentity::fake(),
            AnnounceMessage::new(41.into(), foreign_clock_ds),
        )
        .with_resolved_clock(foreign_clock_ds)];
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

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let transition = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(
            transition,
            Some(StateDecision::MasterClockSelected(_))
        ));
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
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

        let mut uncalibrated = PortState::Uncalibrated(UncalibratedPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        ));

        let transition = uncalibrated.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::AnnounceReceiptTimeoutExpired)
        ));
    }
}
