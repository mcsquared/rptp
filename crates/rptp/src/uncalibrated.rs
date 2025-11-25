use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
    ParentTrackingBmca,
};
use crate::log::PortLog;
use crate::message::AnnounceMessage;
use crate::port::{Port, PortIdentity, PortTimingPolicy, Timeout};
use crate::portstate::{PortState, StateDecision};

pub struct UncalibratedPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: ParentTrackingBmca<B>,
    announce_receipt_timeout: P::Timeout,
    log: L,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> UncalibratedPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: ParentTrackingBmca<B>,
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

        msg.feed_bmca(&mut self.bmca, source_port_identity);

        // TODO: real calibration behaviour is yet to be implemented. For now, we just return
        // MasterClockSelected on the first announce from the current parent, as long as the
        // BMCA does not decide otherwise.
        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => {
                if self.bmca.matches_parent(&source_port_identity) {
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
            format!("Master clock selected, parent {}", self.bmca.parent()).as_str(),
        );

        PortState::slave(self.port, self.bmca, self.log, self.timing_policy)
    }

    pub fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log
            .state_transition("Uncalibrated", "Master", "Announce receipt timeout expired");

        let local_tracking_bmca = LocalMasterTrackingBmca::new(self.bmca.into_inner());

        PortState::master(self.port, local_tracking_bmca, self.log, self.timing_policy)
    }

    pub fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log
            .state_transition("Uncalibrated", "Pre-Master", "Recommended Master");

        decision.apply(
            self.port,
            self.bmca.into_inner(),
            self.log,
            self.timing_policy,
        )
    }

    pub fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        self.log.state_transition(
            "Uncalibrated",
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use crate::bmca::{
        BmcaMasterDecision, BmcaMasterDecisionPoint, DefaultDS, ForeignClockDS, ForeignClockRecord,
        IncrementalBmca,
    };
    use crate::clock::{ClockIdentity, FakeClock, LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::portstate::PortState;

    #[test]
    fn uncalibrated_port_produces_slave_recommendation_with_new_parent() {
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

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Receive two better announces from another parent port
        let new_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );
        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), ForeignClockDS::high_grade_test_clock()),
            new_parent,
        );
        assert!(matches!(decision, None)); // first announce from new parent is ignored

        let decision = uncalibrated.process_announce(
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
    fn uncalibrated_port_becomes_slave_on_next_announce_from_parent() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::fake();
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

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            parent_port,
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
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
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

    #[test]
    fn uncalibrated_port_produces_m1_master_recommendation_when_local_better_than_foreign() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::fake();
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
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );

        // First announce qualifies the foreign record but yields no decision yet.
        let _ = uncalibrated
            .process_announce(AnnounceMessage::new(42.into(), foreign_clock), foreign_port);

        // Second announce from the same foreign clock drives BMCA to a Master(M1) decision.
        let decision = uncalibrated
            .process_announce(AnnounceMessage::new(43.into(), foreign_clock), foreign_port);

        assert_eq!(
            decision,
            Some(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0)
            )))
        );
    }

    #[test]
    fn uncalibrated_port_produces_m2_master_recommendation_when_non_gm_local_better_than_foreign() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::fake();
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
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let foreign_clock = ForeignClockDS::low_grade_test_clock();
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        let _ = uncalibrated
            .process_announce(AnnounceMessage::new(42.into(), foreign_clock), foreign_port);

        let decision = uncalibrated
            .process_announce(AnnounceMessage::new(43.into(), foreign_clock), foreign_port);

        assert_eq!(
            decision,
            Some(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0)
            )))
        );
    }
}
