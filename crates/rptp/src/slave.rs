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
use crate::uncalibrated::UncalibratedPort;

pub struct SlavePort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: ParentTrackingBmca<B>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
    log: L,
    profile: PortProfile,
}

impl<P: Port, B: Bmca, L: PortLog> SlavePort<P, B, L> {
    pub(crate) fn new(
        port: P,
        bmca: ParentTrackingBmca<B>,
        announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
        delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
        log: L,
        profile: PortProfile,
    ) -> Self {
        log.port_event(PortEvent::Static("Become SlavePort"));

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
            let servo_state = self.port.local_clock().discipline(sample);
            match servo_state {
                ServoState::Locked => None,
                _ => Some(StateDecision::SynchronizationFault),
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
                ServoState::Locked => None,
                _ => Some(StateDecision::SynchronizationFault),
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

    pub(crate) fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::AnnounceReceiptTimeout);
        let bmca = LocalMasterTrackingBmca::new(self.bmca.into_inner());
        self.profile.master(self.port, bmca, self.log)
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

    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedMaster);
        let bmca = LocalMasterTrackingBmca::new(self.bmca.into_inner());

        decision.apply(|qualification_timeout_policy, steps_removed| {
            self.port.update_steps_removed(steps_removed);
            self.profile
                .pre_master(self.port, bmca, self.log, qualification_timeout_policy)
        })
    }

    pub(crate) fn synchronization_fault(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::SynchronizationFault);
        PortState::Uncalibrated(UncalibratedPort::new(
            self.port,
            self.bmca,
            self.announce_receipt_timeout,
            self.delay_mechanism,
            self.log,
            self.profile,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{DefaultDS, ForeignClockDS, ForeignClockRecord, IncrementalBmca};
    use crate::clock::{ClockIdentity, LocalClock, StepsRemoved, TimeScale};
    use crate::e2e::DelayCycle;
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping};
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    type SlaveTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakePort, &'a FakeTimerHost, FakeTimestamping>;

    type SlaveTestPort<'a> = SlavePort<
        SlaveTestDomainPort<'a>,
        IncrementalBmca<SortedForeignClockRecordsVec>,
        NoopPortLog,
    >;

    struct SlavePortTestSetup {
        local_clock: LocalClock<FakeClock>,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
    }

    impl SlavePortTestSetup {
        fn new() -> Self {
            Self::new_with_ds(DefaultDS::mid_grade_test_clock())
        }

        fn new_with_ds(default_ds: DefaultDS) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    default_ds,
                    StepsRemoved::new(0),
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
            }
        }

        fn port_under_test(
            &self,
            parent: PortIdentity,
            records: &[ForeignClockRecord],
        ) -> SlaveTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            );

            let delay_request_timeout = domain_port.timeout(SystemMessage::DelayRequestTimeout);

            SlavePort::new(
                domain_port,
                ParentTrackingBmca::new(
                    IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(records)),
                    ParentPortIdentity::new(parent),
                ),
                AnnounceReceiptTimeout::new(
                    FakeTimeout::new(SystemMessage::AnnounceReceiptTimeout),
                    Duration::from_secs(5),
                ),
                EndToEndDelayMechanism::new(DelayCycle::new(
                    0.into(),
                    delay_request_timeout,
                    LogInterval::new(0),
                )),
                NoopPortLog,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn slave_port_test_setup_is_side_effect_free() {
        let setup = SlavePortTestSetup::new();

        let _slave = setup.port_under_test(PortIdentity::fake(), &[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn slave_port_synchronizes_clock_with_two_step_sync() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortIdentity::fake(), &[]);

        slave.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));
        slave.process_delay_response(
            DelayResponseMessage::new(
                0.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            PortIdentity::fake(),
        );
        slave.process_two_step_sync(
            TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0)),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );
        slave.process_follow_up(
            FollowUpMessage::new(0.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
        );

        assert_eq!(setup.local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_synchronizes_clock_with_one_step_sync() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortIdentity::fake(), &[]);

        slave.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));
        slave.process_delay_response(
            DelayResponseMessage::new(
                0.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            PortIdentity::fake(),
        );
        slave.process_one_step_sync(
            OneStepSyncMessage::new(0.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );

        assert_eq!(setup.local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_schedules_next_delay_request_timeout() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortIdentity::fake(), &[]);

        slave.send_delay_request().unwrap();

        let messages = setup.timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayRequestTimeout));
    }

    #[test]
    fn slave_port_sends_delay_request() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortIdentity::fake(), &[]);

        slave.send_delay_request().unwrap();

        assert!(
            setup
                .physical_port
                .contains_event_message(&EventMessage::DelayReq(DelayRequestMessage::new(
                    0.into()
                )))
        );
    }

    #[test]
    fn slave_port_to_master_transition_on_announce_receipt_timeout() {
        let setup = SlavePortTestSetup::new();

        let slave = setup.port_under_test(PortIdentity::fake(), &[]);

        let master = slave.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn slave_port_ignores_general_messages_from_non_parent() {
        let setup = SlavePortTestSetup::new();

        // Define a parent and a different non-parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );
        let non_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        // Create slave with a chosen parent
        let mut slave = setup.port_under_test(parent, &[]);

        // Record a TwoStepSync from the parent so a matching FollowUp could produce ms_offset
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(0)),
            parent,
            TimeStamp::new(2, 0),
        );
        assert!(transition.is_none());

        // Record a delay request timestamp to allow sm_offset calculation
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(transition.is_none());

        // Send FollowUp and DelayResp from a non-parent; these should be ignored
        let transition = slave.process_follow_up(
            FollowUpMessage::new(1.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            non_parent,
        );
        assert!(transition.is_none());

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(
                2.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            non_parent,
        );
        assert!(transition.is_none());

        // With correct filtering, the local clock should remain unchanged
        assert_eq!(setup.local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_ignores_event_messages_from_non_parent() {
        let setup = SlavePortTestSetup::new();

        // Define a parent and a different non-parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );
        let non_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        // Create slave with chosen parent
        let mut slave = setup.port_under_test(parent, &[]);

        // Send a FollowUp from the parent first (ms offset incomplete without sync)
        let transition = slave.process_follow_up(
            FollowUpMessage::new(1.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            parent,
        );
        assert!(transition.is_none());

        // Now send TwoStepSync from a non-parent; should be ignored
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(0)),
            non_parent,
            TimeStamp::new(2, 0),
        );
        assert!(transition.is_none());

        // Even if delay path completes, delay mechanism should not trigger without accepted sync
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(transition.is_none());

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(
                2.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            parent,
        );
        assert!(transition.is_none());

        // Local clock remains unchanged
        assert_eq!(setup.local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_disciplines_on_matching_conversation() {
        let setup = SlavePortTestSetup::new();

        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xCC, 0xCC, 0xCC]),
            PortNumber::new(1),
        );

        let mut slave = setup.port_under_test(parent, &[]);

        let transition =
            slave.process_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        assert!(transition.is_none());

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(
                43.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            parent,
        );
        assert!(transition.is_none());

        // Matching conversation from the parent (numbers chosen to yield to local time at 2s)
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            parent,
            TimeStamp::new(1, 0),
        );
        assert!(transition.is_none());

        let transition = slave.process_follow_up(
            FollowUpMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            parent,
        );
        assert!(transition.is_none());

        // Local clock disciplined to 2s
        assert_eq!(setup.local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_produces_no_decision_on_updated_same_parent() {
        let setup = SlavePortTestSetup::new_with_ds(DefaultDS::low_grade_test_clock());

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

        let mut slave = setup.port_under_test(parent_port, &prior_records);

        // Receive a better announce from the same parent port
        let decision = slave.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock(),
                TimeScale::Ptp,
            ),
            parent_port,
            Instant::from_secs(0),
        );

        // expect a no decision since parent is unchanged
        assert_eq!(decision, None);
    }

    #[test]
    fn slave_port_produces_slave_recommendation_with_new_parent() {
        use crate::bmca::{ForeignClockDS, ForeignClockRecord};

        let setup = SlavePortTestSetup::new_with_ds(DefaultDS::low_grade_test_clock());

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

        let mut slave = setup.port_under_test(parent_port, &prior_records);

        // Receive two better announces from another parent port
        let new_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );
        let decision = slave.process_announce(
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

        let decision = slave.process_announce(
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
    fn slave_port_produces_master_recommendation_on_worse_announces() {
        let setup = SlavePortTestSetup::new_with_ds(DefaultDS::mid_grade_test_clock());

        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut slave = setup.port_under_test(parent_port, &prior_records);

        // Receive a worse announce from the current parent
        let decision = slave.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::low_grade_test_clock(),
                TimeScale::Ptp,
            ),
            parent_port,
            Instant::from_secs(0),
        );

        // expect a master recommendation since local clock is better
        assert!(matches!(
            decision,
            Some(StateDecision::RecommendedMaster(_))
        ));
    }
}
