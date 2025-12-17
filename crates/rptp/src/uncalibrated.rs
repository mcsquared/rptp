use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
    ParentTrackingBmca,
};
use crate::e2e::EndToEndDelayMechanism;
use crate::log::{PortEvent, PortLog};
use crate::message::{
    AnnounceMessage, DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
    OneStepSyncMessage, TwoStepSyncMessage,
};
use crate::port::{AnnounceReceiptTimeout, Port, PortIdentity, SendResult};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::servo::ServoState;
use crate::time::{Instant, TimeStamp};

pub struct UncalibratedPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: ParentTrackingBmca<B>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
    log: L,
    profile: PortProfile,
}

impl<P: Port, B: Bmca, L: PortLog> UncalibratedPort<P, B, L> {
    pub(crate) fn new(
        port: P,
        bmca: ParentTrackingBmca<B>,
        announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
        delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
        log: L,
        profile: PortProfile,
    ) -> Self {
        log.port_event(PortEvent::Static("Become UncalibratedPort"));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            delay_mechanism,
            log,
            profile,
        }
    }

    pub(crate) fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout.restart();

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => None,
        }
    }

    pub(crate) fn process_one_step_sync(
        &mut self,
        sync: OneStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("One-Step Sync");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism
            .record_one_step_sync(sync, ingress_timestamp);
        if let Some(sample) = self.delay_mechanism.sample() {
            let state = self.port.local_clock().discipline(sample);

            match state {
                ServoState::Locked => Some(StateDecision::MasterClockSelected),
                _ => None,
            }
        } else {
            None
        }
    }

    pub(crate) fn process_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("Two-Step Sync");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism
            .record_two_step_sync(sync, ingress_timestamp);

        None
    }

    pub(crate) fn process_follow_up(
        &mut self,
        follow_up: FollowUpMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("FollowUp");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism.record_follow_up(follow_up);
        if let Some(sample) = self.delay_mechanism.sample() {
            let servo_state = self.port.local_clock().discipline(sample);
            match servo_state {
                ServoState::Locked => Some(StateDecision::MasterClockSelected),
                _ => None,
            }
        } else {
            None
        }
    }

    pub(crate) fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        egress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("DelayReq");
        self.delay_mechanism
            .record_delay_request(req, egress_timestamp);

        None
    }

    pub(crate) fn process_delay_response(
        &mut self,
        resp: DelayResponseMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("DelayResp");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism.record_delay_response(resp);

        None
    }

    pub(crate) fn send_delay_request(&mut self) -> SendResult {
        let delay_request = self.delay_mechanism.delay_request();
        self.port
            .send_event(EventMessage::DelayReq(delay_request))?;
        self.log.message_sent("DelayReq");
        Ok(())
    }

    pub(crate) fn master_clock_selected(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::MasterClockSelected {
            parent: self.bmca.parent(),
        });
        self.profile
            .slave(self.port, self.bmca, self.delay_mechanism, self.log)
    }

    pub(crate) fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::AnnounceReceiptTimeout);
        let local_tracking_bmca = LocalMasterTrackingBmca::new(self.bmca.into_inner());
        self.profile
            .master(self.port, local_tracking_bmca, self.log)
    }

    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedMaster);

        let bmca = LocalMasterTrackingBmca::new(self.bmca.into_inner());

        decision.apply(|qualification_timeout_policy, steps_removed| {
            self.port.update_steps_removed(steps_removed);
            self.profile
                .pre_master(self.port, bmca, self.log, qualification_timeout_policy)
        })
    }

    pub(crate) fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        decision.apply(|parent_port_identity, steps_removed| {
            self.log.port_event(PortEvent::RecommendedSlave {
                parent: parent_port_identity,
            });

            let new_parent_tracking_bmca =
                ParentTrackingBmca::new(self.bmca.into_inner(), parent_port_identity);

            // Update steps removed as per IEEE 1588-2019 Section 9.3.5, Table 16
            self.port.update_steps_removed(steps_removed);

            self.profile
                .uncalibrated(self.port, new_parent_tracking_bmca, self.log)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{
        BmcaMasterDecision, BmcaMasterDecisionPoint, DefaultDS, ForeignClockDS, ForeignClockRecord,
        IncrementalBmca,
    };
    use crate::clock::{ClockIdentity, LocalClock, StepsRemoved};
    use crate::e2e::DelayCycle;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{DelayRequestMessage, DelayResponseMessage, SystemMessage, TimeScale};
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakePort, FakeTimerHost, FakeTimestamping};
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    type UncalibratedTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakePort, &'a FakeTimerHost, FakeTimestamping>;

    type UncalibratedTestPort<'a> = UncalibratedPort<
        UncalibratedTestDomainPort<'a>,
        IncrementalBmca<SortedForeignClockRecordsVec>,
        NoopPortLog,
    >;

    struct UncalibratedPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
    }

    impl UncalibratedPortTestSetup {
        fn new(default_ds: DefaultDS, steps_removed: StepsRemoved) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    default_ds,
                    steps_removed,
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
            }
        }

        fn port_under_test(
            &self,
            parent_port: PortIdentity,
            records: &[ForeignClockRecord],
        ) -> UncalibratedTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            );

            let announce_receipt_timeout = AnnounceReceiptTimeout::new(
                domain_port.timeout(SystemMessage::AnnounceReceiptTimeout),
                Duration::from_secs(5),
            );

            let delay_timeout = domain_port.timeout(SystemMessage::DelayRequestTimeout);
            let delay_cycle = DelayCycle::new(0.into(), delay_timeout, LogInterval::new(0));

            UncalibratedPort::new(
                domain_port,
                ParentTrackingBmca::new(
                    IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(records)),
                    ParentPortIdentity::new(parent_port),
                ),
                announce_receipt_timeout,
                EndToEndDelayMechanism::new(delay_cycle),
                NoopPortLog,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn uncalibrated_port_test_setup_is_side_effect_free() {
        let setup =
            UncalibratedPortTestSetup::new(DefaultDS::low_grade_test_clock(), StepsRemoved::new(0));

        let _uncalibrated = setup.port_under_test(PortIdentity::fake(), &[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn uncalibrated_port_produces_slave_recommendation_with_new_parent() {
        let setup =
            UncalibratedPortTestSetup::new(DefaultDS::low_grade_test_clock(), StepsRemoved::new(0));

        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = ForeignClockDS::mid_grade_test_clock();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let mut uncalibrated = setup.port_under_test(parent_port, &prior_records);

        // Receive two better announces from another parent port
        let new_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );
        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock(),
                TimeScale::Ptp,
            ),
            new_parent,
            Instant::from_secs(0),
        );
        assert!(decision.is_none()); // first announce from new parent is ignored

        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock(),
                TimeScale::Ptp,
            ),
            new_parent,
            Instant::from_secs(0),
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
    fn uncalibrated_port_becomes_slave_on_next_sync_from_parent() {
        let setup =
            UncalibratedPortTestSetup::new(DefaultDS::mid_grade_test_clock(), StepsRemoved::new(0));

        let parent_port = PortIdentity::fake();
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let mut uncalibrated = setup.port_under_test(parent_port, &prior_records);

        // pre-feed the delay mechanism with delay req/resp messages so it can calibrate
        let decision = uncalibrated
            .process_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));
        assert!(decision.is_none());
        let decision = uncalibrated.process_delay_response(
            DelayResponseMessage::new(
                42.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            parent_port,
        );
        assert!(decision.is_none());

        let decision = uncalibrated.process_one_step_sync(
            OneStepSyncMessage::new(0.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );

        assert!(matches!(decision, Some(StateDecision::MasterClockSelected)));
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let setup = UncalibratedPortTestSetup::new(
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let uncalibrated = setup.port_under_test(PortIdentity::fake(), &[]);

        let master = uncalibrated.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn uncalibrated_port_produces_m1_master_recommendation_when_local_better_than_foreign() {
        let setup =
            UncalibratedPortTestSetup::new(DefaultDS::gm_grade_test_clock(), StepsRemoved::new(0));

        let parent_port = PortIdentity::fake();
        let mut uncalibrated = setup.port_under_test(parent_port, &[]);

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );

        // First announce qualifies the foreign record but yields no decision yet.
        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        // Second announce from the same foreign clock drives BMCA to a Master(M1) decision.
        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );

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
        let setup =
            UncalibratedPortTestSetup::new(DefaultDS::mid_grade_test_clock(), StepsRemoved::new(0));

        let parent_port = PortIdentity::fake();
        let mut uncalibrated = setup.port_under_test(parent_port, &[]);

        let foreign_clock = ForeignClockDS::low_grade_test_clock();
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );

        assert_eq!(
            decision,
            Some(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0)
            )))
        );
    }
}
