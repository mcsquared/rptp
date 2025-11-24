use crate::bmca::{Bmca, BmcaDecision};
use crate::log::PortLog;
use crate::message::AnnounceMessage;
use crate::port::{ParentPortIdentity, Port, PortIdentity, PortTimingPolicy, Timeout};
use crate::portstate::{PortState, StateDecision};

pub struct UncalibratedPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    log: L,
    parent_port_identity: ParentPortIdentity,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> UncalibratedPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: B,
        announce_receipt_timeout: P::Timeout,
        log: L,
        parent_port_identity: ParentPortIdentity,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
            log,
            parent_port_identity,
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

        // TODO: real calibration behaviour is yet to be implemented. For now, we just return
        // MasterClockSelected on the first announce from the current parent, as long as the
        // BMCA does not decide otherwise.
        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => {
                if self.parent_port_identity.matches(&source_port_identity) {
                    Some(StateDecision::MasterClockSelected)
                } else {
                    None
                }
            }
        }
    }

    pub fn master_clock_selected(self) -> PortState<P, B, L> {
        self.log.state_transition(
            "Uncalibrated",
            "Slave",
            format!(
                "Master clock selected, parent {}",
                self.parent_port_identity
            )
            .as_str(),
        );

        PortState::slave(
            self.port,
            self.bmca,
            self.parent_port_identity,
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

    use crate::bmca::{DefaultDS, ForeignClockDS, ForeignClockRecord, FullBmca};
    use crate::clock::{FakeClock, LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;

    #[test]
    fn uncalibrated_port_becomes_slave_on_next_announce_from_parent() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::fake();
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(parent_port, foreign_clock_ds)
            .with_qualified_clock(foreign_clock_ds)];
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
            ParentPortIdentity::new(parent_port),
            PortTimingPolicy::default(),
        );

        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(decision, Some(StateDecision::MasterClockSelected)));
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
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

        let mut uncalibrated = PortState::Uncalibrated(UncalibratedPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopPortLog,
            ParentPortIdentity::new(PortIdentity::fake()),
            PortTimingPolicy::default(),
        ));

        let transition = uncalibrated.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::AnnounceReceiptTimeoutExpired)
        ));
    }
}
