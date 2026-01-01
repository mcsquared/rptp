use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, GrandMasterTrackingBmca, SortedForeignClockRecords,
};
use crate::log::PortEvent;
use crate::message::AnnounceMessage;
use crate::port::{ParentPortIdentity, Port, PortIdentity};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::time::Instant;

pub struct PreMasterPort<P: Port, S: SortedForeignClockRecords> {
    port: P,
    bmca: GrandMasterTrackingBmca<S>,
    _qualification_timeout: P::Timeout,
    profile: PortProfile,
}

impl<P: Port, S: SortedForeignClockRecords> PreMasterPort<P, S> {
    pub(crate) fn new(
        port: P,
        bmca: GrandMasterTrackingBmca<S>,
        _qualification_timeout: P::Timeout,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become PreMasterPort"));

        Self {
            port,
            bmca,
            _qualification_timeout,
            profile,
        }
    }

    pub(crate) fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("Announce"));

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision() {
            Some(BmcaDecision::Master(decision)) => {
                Some(StateDecision::RecommendedMaster(decision))
            }
            Some(BmcaDecision::Slave(parent)) => Some(StateDecision::RecommendedSlave(parent)),
            Some(BmcaDecision::Passive) => None, // TODO: Handle Passive transition --- IGNORE ---
            None => None,
        }
    }

    pub(crate) fn qualified(self) -> PortState<P, S> {
        self.port.log(PortEvent::QualifiedMaster);
        self.profile.master(self.port, self.bmca)
    }

    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, S> {
        self.port.log(PortEvent::RecommendedMaster);

        decision.apply(|qualification_timeout_policy, grandmaster_id| {
            let bmca = self.bmca.with_grandmaster_id(grandmaster_id);

            self.profile
                .pre_master(self.port, bmca, qualification_timeout_policy)
        })
    }

    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<P, S> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let bmca = self.bmca.into_parent_tracking(parent);

        self.profile.uncalibrated(self.port, bmca)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{
        BestForeignRecord, BestMasterClockAlgorithm, ClockDS, ForeignClockRecord,
        GrandMasterTrackingBmca,
    };
    use crate::clock::{LocalClock, StepsRemoved, TimeScale};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::portstate::StateDecision;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeTimerHost, FakeTimestamping, TestClockCatalog,
    };
    use crate::time::{Instant, LogMessageInterval};

    type PreMasterTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type PreMasterTestPort<'a> =
        PreMasterPort<PreMasterTestDomainPort<'a>, SortedForeignClockRecordsVec>;

    struct PreMasterPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
    }

    impl PreMasterPortTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    ds,
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
            }
        }

        fn port_under_test(&self, records: &[ForeignClockRecord]) -> PreMasterTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            );

            let qualification_timeout = domain_port.timeout(SystemMessage::QualificationTimeout);
            let grandmaster_id = *self.local_clock.identity();

            PreMasterPort::new(
                domain_port,
                GrandMasterTrackingBmca::new(
                    BestMasterClockAlgorithm::new(*self.local_clock.default_ds()),
                    BestForeignRecord::new(SortedForeignClockRecordsVec::from_records(records)),
                    grandmaster_id,
                ),
                qualification_timeout,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn pre_master_port_test_setup_is_side_effect_free() {
        let setup = PreMasterPortTestSetup::new(TestClockCatalog::default_low_grade().default_ds());

        let _pre_master = setup.port_under_test(&[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn pre_master_port_to_master_on_qualified() {
        let setup =
            PreMasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let pre_master = setup.port_under_test(&[]);

        let master = pre_master.qualified();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn pre_master_port_produces_slave_recommendation_on_two_better_announces() {
        use crate::message::AnnounceMessage;
        use crate::port::{ParentPortIdentity, PortIdentity, PortNumber};

        let setup = PreMasterPortTestSetup::new(TestClockCatalog::default_low_grade().default_ds());

        let foreign_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let better_port = PortIdentity::new(
            TestClockCatalog::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );
        let mut pre_master = setup.port_under_test(&[]);

        // Receive first better announce
        let decision = pre_master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            better_port,
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        // Receive second better announce
        let decision = pre_master.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            better_port,
            Instant::from_secs(0),
        );

        // expect a slave recommendation
        assert_eq!(
            decision,
            Some(StateDecision::RecommendedSlave(ParentPortIdentity::new(
                better_port
            )))
        );
    }
}
