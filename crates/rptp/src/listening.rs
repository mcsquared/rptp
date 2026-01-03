use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, ListeningBmca, SortedForeignClockRecords,
};
use crate::log::PortEvent;
use crate::message::AnnounceMessage;
use crate::port::{AnnounceReceiptTimeout, ParentPortIdentity, Port, PortIdentity};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::time::Instant;

pub struct ListeningPort<'a, P: Port, S: SortedForeignClockRecords> {
    port: P,
    bmca: ListeningBmca<'a, S>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    profile: PortProfile,
}

impl<'a, P: Port, S: SortedForeignClockRecords> ListeningPort<'a, P, S> {
    pub(crate) fn new(
        port: P,
        bmca: ListeningBmca<'a, S>,
        announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become ListeningPort"));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            profile,
        }
    }

    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let parent_tracking_bmca = self.bmca.into_parent_tracking(parent);

        self.profile.uncalibrated(self.port, parent_tracking_bmca)
    }

    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedMaster);

        decision.apply(|qualification_timeout_policy, grandmaster_id| {
            let bmca = self.bmca.into_grandmaster_tracking(grandmaster_id);

            self.profile
                .pre_master(self.port, bmca, qualification_timeout_policy)
        })
    }

    pub(crate) fn announce_receipt_timeout_expired(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::AnnounceReceiptTimeout);
        let bmca = self.bmca.into_current_grandmaster_tracking();
        self.profile.master(self.port, bmca)
    }

    pub(crate) fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("Announce"));
        self.announce_receipt_timeout.restart();

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
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::cell::Cell;

    use crate::bmca::{
        BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, BmcaMasterDecisionPoint,
        ClockDS, ListeningBmca,
    };
    use crate::clock::{ClockIdentity, LocalClock, StepsRemoved, TimeScale};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeTimerHost, FakeTimestamping, TestClockCatalog,
    };
    use crate::time::{Duration, Instant, LogMessageInterval};

    type ListeningTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type ListeningTestPort<'a> =
        ListeningPort<'a, ListeningTestDomainPort<'a>, SortedForeignClockRecordsVec>;

    struct ListeningPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        foreign_candidates: Cell<BestForeignSnapshot>,
    }

    impl ListeningPortTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    *ds.identity(),
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                default_ds: ds,
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
                foreign_candidates: Cell::new(BestForeignSnapshot::Empty),
            }
        }

        fn local_clock_identity(&self) -> &ClockIdentity {
            self.local_clock.identity()
        }

        fn port_under_test(&self) -> ListeningTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            );

            let announce_receipt_timeout = AnnounceReceiptTimeout::new(
                domain_port.timeout(SystemMessage::AnnounceReceiptTimeout),
                Duration::from_secs(5),
            );

            ListeningPort::new(
                domain_port,
                ListeningBmca::new(
                    BestMasterClockAlgorithm::new(
                        PortNumber::new(1),
                        self.default_ds,
                        &self.foreign_candidates,
                    ),
                    BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                ),
                announce_receipt_timeout,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn listening_port_test_setup_is_side_effect_free() {
        let setup =
            ListeningPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let _listening = setup.port_under_test();

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let setup =
            ListeningPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let listening = setup.port_under_test();

        let master = listening.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
        let setup = ListeningPortTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));

        let decision = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(decision.is_none());

        let system_messages = setup.timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_recommends_master_on_two_announces() {
        let setup =
            ListeningPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));

        let decision = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(matches!(
            decision,
            Some(StateDecision::RecommendedMaster(_))
        ));

        let system_messages = setup.timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_recommends_slave_on_two_announces() {
        let setup = ListeningPortTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));

        let decision = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(matches!(decision, Some(StateDecision::RecommendedSlave(_))));

        let system_messages = setup.timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_m1_master_recommendation() {
        let setup = ListeningPortTestSetup::new(TestClockCatalog::gps_grandmaster().default_ds());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0));

        let _ = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let master_decision = match decision {
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            master_decision,
            BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0),
                *setup.local_clock_identity()
            )
        );

        assert!(matches!(
            listening.recommended_master(master_decision),
            PortState::PreMaster(_)
        ));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_m2_master_recommendation() {
        let setup = ListeningPortTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let mut listening = setup.port_under_test();

        let foreign_clock =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

        let _ = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let master_decision = match decision {
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            master_decision,
            BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0),
                *setup.local_clock_identity()
            )
        );

        assert!(matches!(
            listening.recommended_master(master_decision),
            PortState::PreMaster(_)
        ));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_s1_slave_recommendation() {
        let setup = ListeningPortTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));

        setup.timer_host.take_system_messages();

        let _ = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let transition = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = match transition {
            Some(StateDecision::RecommendedSlave(decision)) => decision,
            _ => panic!("expected RecommendedSlave decision"),
        };

        assert!(matches!(
            listening.recommended_slave(decision),
            PortState::Uncalibrated(_)
        ));
    }
}
