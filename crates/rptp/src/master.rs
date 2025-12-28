use crate::bmca::{
    Bmca, BmcaDecision, GrandMasterTrackingBmca, QualificationTimeoutPolicy,
    SortedForeignClockRecords,
};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::PortEvent;
use crate::message::{
    AnnounceMessage, DelayRequestMessage, EventMessage, GeneralMessage, SequenceId,
    TwoStepSyncMessage,
};
use crate::port::{ParentPortIdentity, Port, PortIdentity, SendResult, Timeout};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::time::{Instant, LogInterval, TimeStamp};

pub struct MasterPort<P: Port, S: SortedForeignClockRecords> {
    port: P,
    bmca: GrandMasterTrackingBmca<S>,
    announce_cycle: AnnounceCycle<P::Timeout>,
    sync_cycle: SyncCycle<P::Timeout>,
    profile: PortProfile,
}

impl<P: Port, S: SortedForeignClockRecords> MasterPort<P, S> {
    pub(crate) fn new(
        port: P,
        bmca: GrandMasterTrackingBmca<S>,
        announce_cycle: AnnounceCycle<P::Timeout>,
        sync_cycle: SyncCycle<P::Timeout>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become MasterPort"));

        Self {
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            profile,
        }
    }

    pub(crate) fn send_announce(&mut self) -> SendResult {
        let announce_message = self.announce_cycle.announce(self.port.local_clock());
        self.port
            .send_general(GeneralMessage::Announce(announce_message))?;
        self.announce_cycle.next();
        self.port.log(PortEvent::MessageSent("Announce"));
        Ok(())
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
            // While already in the Master state, a Master BMCA decision does not require any
            // state transition.
            Some(BmcaDecision::Master(_)) => None,
            Some(BmcaDecision::Slave(parent)) => Some(StateDecision::RecommendedSlave(parent)),
            Some(BmcaDecision::Passive) => None, // TODO: Handle Passive transition --- IGNORE ---
            None => None,
        }
    }

    pub(crate) fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        ingress_timestamp: TimeStamp,
        requesting_port_identity: PortIdentity,
    ) -> SendResult {
        self.port.log(PortEvent::MessageReceived("DelayReq"));

        let response = req.response(
            self.profile
                .log_min_delay_request_interval()
                .log_message_interval(),
            ingress_timestamp,
            requesting_port_identity,
        );

        let result = self.port.send_general(GeneralMessage::DelayResp(response));
        if result.is_ok() {
            self.port.log(PortEvent::MessageSent("DelayResp"));
        }
        result
    }

    pub(crate) fn send_sync(&mut self) -> SendResult {
        let sync_message = self.sync_cycle.two_step_sync();
        self.port
            .send_event(EventMessage::TwoStepSync(sync_message))?;
        self.sync_cycle.next();
        self.port.log(PortEvent::MessageSent("TwoStepSync"));
        Ok(())
    }

    pub(crate) fn send_follow_up(
        &mut self,
        sync: TwoStepSyncMessage,
        egress_timestamp: TimeStamp,
    ) -> SendResult {
        self.port
            .send_general(GeneralMessage::FollowUp(sync.follow_up(egress_timestamp)))?;
        self.port.log(PortEvent::MessageSent("FollowUp"));
        Ok(())
    }

    pub(crate) fn recommended_master(
        self,
        qualification_timeout_policy: QualificationTimeoutPolicy,
    ) -> PortState<P, S> {
        self.port.log(PortEvent::RecommendedMaster);

        self.profile
            .pre_master(self.port, self.bmca, qualification_timeout_policy)
    }

    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<P, S> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let bmca = self.bmca.into_parent_tracking(parent);

        self.profile.uncalibrated(self.port, bmca)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AnnounceCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
    log_interval: LogInterval,
}

impl<T: Timeout> AnnounceCycle<T> {
    pub fn new(start: SequenceId, timeout: T, log_interval: LogInterval) -> Self {
        Self {
            sequence_id: start,
            timeout,
            log_interval,
        }
    }

    pub fn next(&mut self) {
        self.timeout.restart(self.log_interval.duration());
        self.sequence_id = self.sequence_id.next();
    }

    pub fn announce<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> AnnounceMessage {
        local_clock.announce(self.sequence_id, self.log_interval.log_message_interval())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SyncCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
    log_interval: LogInterval,
}

impl<T: Timeout> SyncCycle<T> {
    pub fn new(start: SequenceId, timeout: T, log_interval: LogInterval) -> Self {
        Self {
            sequence_id: start,
            timeout,
            log_interval,
        }
    }

    pub fn next(&mut self) {
        self.timeout.restart(self.log_interval.duration());
        self.sequence_id = self.sequence_id.next();
    }

    pub fn two_step_sync(&self) -> TwoStepSyncMessage {
        TwoStepSyncMessage::new(self.sequence_id, self.log_interval.log_message_interval())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{BestForeignRecord, ClockDS, ForeignClockRecord, GrandMasterTrackingBmca};
    use crate::clock::{LocalClock, StepsRemoved, TimeScale};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{
        DelayResponseMessage, EventMessage, FollowUpMessage, GeneralMessage, SystemMessage,
        TwoStepSyncMessage,
    };
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping, TestClockCatalog,
    };
    use crate::time::{Instant, LogInterval, LogMessageInterval};

    type MasterTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type MasterTestPort<'a> = MasterPort<MasterTestDomainPort<'a>, SortedForeignClockRecordsVec>;

    struct MasterPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
    }

    impl MasterPortTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self::new_with_time(ds, TimeStamp::new(0, 0))
        }

        fn new_with_time(ds: ClockDS, now: TimeStamp) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::new(now, TimeScale::Ptp),
                    ds,
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
            }
        }

        fn port_under_test(&self, records: &[ForeignClockRecord]) -> MasterTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            );
            let announce_cycle = AnnounceCycle::new(
                0.into(),
                domain_port.timeout(SystemMessage::AnnounceSendTimeout),
                LogInterval::new(0),
            );
            let sync_cycle = SyncCycle::new(
                0.into(),
                domain_port.timeout(SystemMessage::SyncTimeout),
                LogInterval::new(0),
            );

            MasterPort::new(
                domain_port,
                GrandMasterTrackingBmca::new(BestForeignRecord::new(
                    SortedForeignClockRecordsVec::from_records(records),
                )),
                announce_cycle,
                sync_cycle,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn master_port_test_setup_is_side_effect_free() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let _slave = setup.port_under_test(&[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn master_port_answers_delay_request_with_delay_response() {
        let setup = MasterPortTestSetup::new_with_time(
            TestClockCatalog::default_high_grade().default_ds(),
            TimeStamp::new(0, 0),
        );

        let mut master = setup.port_under_test(&[]);

        master
            .process_delay_request(
                DelayRequestMessage::new(0.into()),
                TimeStamp::new(0, 0),
                PortIdentity::fake(),
            )
            .unwrap();

        assert!(
            setup
                .physical_port
                .contains_general_message(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                    0.into(),
                    LogMessageInterval::new(0),
                    TimeStamp::new(0, 0),
                    PortIdentity::fake()
                )))
        );
    }

    #[test]
    fn master_port_sends_sync() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_sync().unwrap();

        assert!(
            setup
                .physical_port
                .contains_event_message(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(
                    0.into(),
                    LogMessageInterval::new(0)
                )))
        );
    }

    #[test]
    fn master_port_schedules_next_sync_timeout() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_sync().unwrap();

        let messages = setup.timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncTimeout));
    }

    #[test]
    fn master_port_sends_follow_up() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master
            .send_follow_up(
                TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0)),
                TimeStamp::new(0, 0),
            )
            .unwrap();

        assert!(
            setup
                .physical_port
                .contains_general_message(&GeneralMessage::FollowUp(FollowUpMessage::new(
                    0.into(),
                    LogMessageInterval::new(0),
                    TimeStamp::new(0, 0)
                )))
        );
    }

    #[test]
    fn master_port_schedules_next_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_announce().unwrap();

        let messages = setup.timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_port_sends_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_announce().unwrap();

        assert!(
            setup
                .physical_port
                .contains_general_message(&GeneralMessage::Announce(AnnounceMessage::new(
                    0.into(),
                    LogMessageInterval::new(0),
                    TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0)),
                    TimeScale::Ptp,
                )))
        );
    }

    #[test]
    fn master_port_recommends_slave_on_two_better_announces() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let foreign_clock_ds =
            TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));

        let mut master = setup.port_under_test(&[]);

        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = master.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(matches!(decision, Some(StateDecision::RecommendedSlave(_))));
    }

    #[test]
    fn master_port_stays_master_on_subsequent_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let foreign_clock_ds =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));
        let prior_records = [ForeignClockRecord::qualified(
            PortIdentity::fake(),
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut master = setup.port_under_test(&prior_records);

        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(decision.is_none());
        assert!(setup.timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_stays_master_single_new_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let foreign_clock_ds =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

        let mut master = setup.port_under_test(&[]);

        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(decision.is_none());
    }

    #[test]
    fn master_port_does_not_recommend_master_when_local_clock_unchanged_but_still_best() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let parent_port = PortIdentity::fake();
        let foreign_clock_ds =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut master = setup.port_under_test(&prior_records);

        // Receive a better announce (but still lower quality than local high-grade clock)
        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0)),
                TimeScale::Ptp,
            ),
            parent_port,
            Instant::from_secs(0),
        );

        // expect no state change - master stays master when receiving worse announces
        assert!(decision.is_none());
    }

    #[test]
    fn announce_cycle_produces_announce_messages_with_monotonic_sequence_ids() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            TestClockCatalog::default_high_grade().default_ds(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let mut cycle = AnnounceCycle::new(
            0.into(),
            FakeTimeout::new(SystemMessage::AnnounceSendTimeout),
            LogInterval::new(0),
        );
        let msg1 = cycle.announce(&local_clock);
        cycle.next();
        let msg2 = cycle.announce(&local_clock);

        assert_eq!(
            msg1,
            AnnounceMessage::new(
                0.into(),
                LogInterval::new(0).log_message_interval(),
                TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0)),
                TimeScale::Ptp,
            )
        );
        assert_eq!(
            msg2,
            AnnounceMessage::new(
                1.into(),
                LogInterval::new(0).log_message_interval(),
                TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0)),
                TimeScale::Ptp,
            )
        );
    }

    #[test]
    fn sync_cycle_message_produces_two_step_sync_message() {
        let sync_cycle = SyncCycle::new(
            0.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
            LogInterval::new(0),
        );
        let two_step_sync = sync_cycle.two_step_sync();

        assert_eq!(
            two_step_sync,
            TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0))
        );
    }

    #[test]
    fn sync_cycle_next() {
        let mut sync_cycle = SyncCycle::new(
            0.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
            LogInterval::new(0),
        );
        sync_cycle.next();

        assert_eq!(
            sync_cycle,
            SyncCycle::new(
                1.into(),
                FakeTimeout::new(SystemMessage::SyncTimeout),
                LogInterval::new(0)
            )
        );
    }

    #[test]
    fn sync_cycle_next_wraps() {
        let mut sync_cycle = SyncCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
            LogInterval::new(0),
        );
        sync_cycle.next();

        assert_eq!(
            sync_cycle,
            SyncCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::SyncTimeout),
                LogInterval::new(0)
            )
        );
    }
}
