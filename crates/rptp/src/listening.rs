use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
    ParentTrackingBmca,
};
use crate::log::PortEvent;
use crate::message::AnnounceMessage;
use crate::port::{AnnounceReceiptTimeout, Port, PortIdentity};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::time::Instant;

pub struct ListeningPort<P: Port, B: Bmca> {
    port: P,
    bmca: B,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    profile: PortProfile,
}

impl<P: Port, B: Bmca> ListeningPort<P, B> {
    pub(crate) fn new(
        port: P,
        bmca: B,
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

    pub(crate) fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B> {
        decision.apply(|parent_port_identity, steps_removed| {
            self.port.log(PortEvent::RecommendedSlave {
                parent: parent_port_identity,
            });

            let parent_tracking_bmca = ParentTrackingBmca::new(self.bmca, parent_port_identity);

            // Update steps removed as per IEEE 1588-2019 Section 9.3.5, Table 16
            self.port.update_steps_removed(steps_removed);

            self.profile.uncalibrated(self.port, parent_tracking_bmca)
        })
    }

    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B> {
        self.port.log(PortEvent::RecommendedMaster);
        let bmca = LocalMasterTrackingBmca::new(self.bmca);

        decision.apply(|qualification_timeout_policy, steps_removed| {
            self.port.update_steps_removed(steps_removed);
            self.profile
                .pre_master(self.port, bmca, qualification_timeout_policy)
        })
    }

    pub(crate) fn announce_receipt_timeout_expired(self) -> PortState<P, B> {
        self.port.log(PortEvent::AnnounceReceiptTimeout);
        let bmca = LocalMasterTrackingBmca::new(self.bmca);
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

    use crate::bmca::{BmcaMasterDecisionPoint, DefaultDS, IncrementalBmca};
    use crate::clock::{LocalClock, StepsRemoved, TimeScale};
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
        ListeningPort<ListeningTestDomainPort<'a>, IncrementalBmca<SortedForeignClockRecordsVec>>;

    struct ListeningPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
    }

    impl ListeningPortTestSetup {
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
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
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
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            decision,
            BmcaMasterDecision::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0))
        );

        let _state = listening.recommended_master(decision);

        assert_eq!(setup.local_clock.steps_removed(), StepsRemoved::new(0));
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
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            decision,
            BmcaMasterDecision::new(BmcaMasterDecisionPoint::M2, StepsRemoved::new(0))
        );

        let _state = listening.recommended_master(decision);

        assert_eq!(setup.local_clock.steps_removed(), StepsRemoved::new(0));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_s1_slave_recommendation() {
        let setup = ListeningPortTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));
        let expected_steps_removed = foreign_clock.steps_removed().increment();

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

        let _state = listening.recommended_slave(decision);

        assert_eq!(setup.local_clock.steps_removed(), expected_steps_removed);
    }
}
