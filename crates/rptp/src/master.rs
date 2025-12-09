use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::{PortEvent, PortLog};
use crate::message::{
    AnnounceMessage, DelayRequestMessage, EventMessage, GeneralMessage, SequenceId,
    TwoStepSyncMessage,
};
use crate::port::{Port, PortIdentity, SendResult, Timeout};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::time::{Instant, LogInterval, TimeStamp};

pub struct MasterPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: LocalMasterTrackingBmca<B>,
    announce_cycle: AnnounceCycle<P::Timeout>,
    sync_cycle: SyncCycle<P::Timeout>,
    log: L,
    profile: PortProfile,
}

impl<P: Port, B: Bmca, L: PortLog> MasterPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: LocalMasterTrackingBmca<B>,
        announce_cycle: AnnounceCycle<P::Timeout>,
        sync_cycle: SyncCycle<P::Timeout>,
        log: L,
        profile: PortProfile,
    ) -> Self {
        log.port_event(PortEvent::Static("Become MasterPort"));

        Self {
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            log,
            profile,
        }
    }

    pub fn send_announce(&mut self) -> SendResult {
        let announce_message = self.announce_cycle.announce(self.port.local_clock());
        self.port
            .send_general(GeneralMessage::Announce(announce_message))?;
        self.announce_cycle.next();
        self.log.message_sent("Announce");
        Ok(())
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.log.message_received("Announce");

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Undecided => None,
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
        }
    }

    pub fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        ingress_timestamp: TimeStamp,
        requesting_port_identity: PortIdentity,
    ) -> SendResult {
        self.log.message_received("DelayReq");

        let response = req.response(
            self.profile
                .log_min_delay_request_interval()
                .log_message_interval(),
            ingress_timestamp,
            requesting_port_identity,
        );

        let result = self.port.send_general(GeneralMessage::DelayResp(response));
        if result.is_ok() {
            self.log.message_sent("DelayResp");
        }
        result
    }

    pub fn send_sync(&mut self) -> SendResult {
        let sync_message = self.sync_cycle.two_step_sync();
        self.port
            .send_event(EventMessage::TwoStepSync(sync_message))?;
        self.sync_cycle.next();
        self.log.message_sent("TwoStepSync");
        Ok(())
    }

    pub fn send_follow_up(
        &mut self,
        sync: TwoStepSyncMessage,
        egress_timestamp: TimeStamp,
    ) -> SendResult {
        self.port
            .send_general(GeneralMessage::FollowUp(sync.follow_up(egress_timestamp)))?;
        self.log.message_sent("FollowUp");
        Ok(())
    }

    pub fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedMaster);
        decision.apply(self.port, self.bmca.into_inner(), self.log, self.profile)
    }

    pub fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedSlave {
            parent: *decision.parent_port_identity(),
        });

        decision.apply(self.port, self.bmca.into_inner(), self.log, self.profile)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AnnounceCycle<T: Timeout> {
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
pub struct SyncCycle<T: Timeout> {
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

    use crate::bmca::{DefaultDS, ForeignClockDS, ForeignClockRecord, IncrementalBmca};
    use crate::clock::{LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{
        DelayResponseMessage, EventMessage, FollowUpMessage, GeneralMessage, SystemMessage,
        TimeScale, TwoStepSyncMessage,
    };
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping};
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    #[test]
    fn master_port_answers_delay_request_with_delay_response() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortProfile::default(),
        );

        timer_host.take_system_messages();

        assert!(
            master
                .process_delay_request(
                    DelayRequestMessage::new(0.into()),
                    TimeStamp::new(0, 0),
                    PortIdentity::fake()
                )
                .is_ok()
        );

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                TimeStamp::new(0, 0),
                PortIdentity::fake()
            )))
        );

        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_answers_sync_timeout_with_sync() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortProfile::default().master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
        );

        master.dispatch_system(SystemMessage::SyncTimeout);

        let messages = port.take_event_messages();
        assert!(
            messages.contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(
                0.into(),
                LogMessageInterval::new(0)
            )))
        );
    }

    #[test]
    fn master_port_schedules_next_sync() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortProfile::default().master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
        );

        // Drain messages that could have been sent during initialization.
        timer_host.take_system_messages();

        master.dispatch_system(SystemMessage::SyncTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncTimeout));
    }

    #[test]
    fn master_port_answers_timestamped_sync_with_follow_up() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortProfile::default(),
        );

        timer_host.take_system_messages();

        assert!(
            master
                .send_follow_up(
                    TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0)),
                    TimeStamp::new(0, 0)
                )
                .is_ok()
        );

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::FollowUp(FollowUpMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                TimeStamp::new(0, 0)
            )))
        );

        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_schedules_next_announce() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortProfile::default().master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
        );

        timer_host.take_system_messages();

        master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_port_sends_announce_on_send_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortProfile::default().master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
        );

        master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::Announce(AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock(),
                TimeScale::Ptp,
            )))
        );
    }

    #[test]
    fn master_port_to_uncalibrated_transition_on_two_announces() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortProfile::default(),
        );

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
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records = [ForeignClockRecord::qualified(
            PortIdentity::fake(),
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(
                SortedForeignClockRecordsVec::from_records(&prior_records),
            )),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortProfile::default(),
        );

        // Drain any setup timers
        timer_host.take_system_messages();

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
        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_stays_master_on_undecided_bmca() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortProfile::default(),
        );

        // Drain any setup timers
        timer_host.take_system_messages();

        let transition = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(transition.is_none());
        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_does_not_recommend_master_when_local_clock_unchanged_but_still_best() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let parent_port = PortIdentity::fake();
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
            LogInterval::new(0),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(
                SortedForeignClockRecordsVec::from_records(&prior_records),
            )),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortProfile::default(),
        );

        // Receive a better announce (but still lower quality than local high-grade clock)
        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::mid_grade_test_clock(),
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
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
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
                ForeignClockDS::high_grade_test_clock(),
                TimeScale::Ptp,
            )
        );
        assert_eq!(
            msg2,
            AnnounceMessage::new(
                1.into(),
                LogInterval::new(0).log_message_interval(),
                ForeignClockDS::high_grade_test_clock(),
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
