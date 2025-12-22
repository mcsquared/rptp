use crate::bmca::{
    Bmca, BmcaDecision, BmcaSlaveDecision, LocalMasterTrackingBmca, ParentTrackingBmca,
};
use crate::log::PortEvent;
use crate::message::AnnounceMessage;
use crate::port::{Port, PortIdentity};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::time::Instant;

pub struct PreMasterPort<P: Port, B: Bmca> {
    port: P,
    bmca: LocalMasterTrackingBmca<B>,
    _qualification_timeout: P::Timeout,
    profile: PortProfile,
}

impl<P: Port, B: Bmca> PreMasterPort<P, B> {
    pub(crate) fn new(
        port: P,
        bmca: LocalMasterTrackingBmca<B>,
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

        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => None,
        }
    }

    pub(crate) fn qualified(self) -> PortState<P, B> {
        self.port.log(PortEvent::QualifiedMaster);
        self.profile.master(self.port, self.bmca)
    }

    pub(crate) fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B> {
        decision.apply(|parent_port_identity, steps_removed| {
            self.port.log(PortEvent::RecommendedSlave {
                parent: parent_port_identity,
            });

            let parent_tracking_bmca =
                ParentTrackingBmca::new(self.bmca.into_inner(), parent_port_identity);

            // Update steps removed as per IEEE 1588-2019 Section 9.3.5, Table 16
            self.port.update_steps_removed(steps_removed);

            self.profile.uncalibrated(self.port, parent_tracking_bmca)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{DefaultDS, ForeignClockRecord, IncrementalBmca};
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
        PreMasterPort<PreMasterTestDomainPort<'a>, IncrementalBmca<SortedForeignClockRecordsVec>>;

    struct PreMasterPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
    }

    impl PreMasterPortTestSetup {
        fn new(default_ds: DefaultDS) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    default_ds,
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

            PreMasterPort::new(
                domain_port,
                LocalMasterTrackingBmca::new(IncrementalBmca::new(
                    SortedForeignClockRecordsVec::from_records(records),
                )),
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
        use crate::bmca::BmcaSlaveDecision;
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
        assert!(decision.is_none()); // first announce is not yet qualified

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
            Some(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(better_port),
                StepsRemoved::new(1)
            )))
        );
    }
}
