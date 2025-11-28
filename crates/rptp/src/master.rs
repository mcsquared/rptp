use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::PortLog;
use crate::message::{
    AnnounceMessage, DelayRequestMessage, EventMessage, GeneralMessage, SequenceId,
    TwoStepSyncMessage,
};
use crate::port::{Port, PortIdentity, PortTimingPolicy, Timeout};
use crate::portstate::{PortState, StateDecision};
use crate::time::{Duration, Instant, LogInterval, TimeStamp};

pub struct MasterPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: LocalMasterTrackingBmca<B>,
    announce_cycle: AnnounceCycle<P::Timeout>,
    sync_cycle: SyncCycle<P::Timeout>,
    log: L,
    timing_policy: PortTimingPolicy,
}

impl<P: Port, B: Bmca, L: PortLog> MasterPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: LocalMasterTrackingBmca<B>,
        announce_cycle: AnnounceCycle<P::Timeout>,
        sync_cycle: SyncCycle<P::Timeout>,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        Self {
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            log,
            timing_policy,
        }
    }

    pub fn send_announce(&mut self) {
        let announce_message = self.announce_cycle.announce(&self.port.local_clock());
        self.port
            .send_general(GeneralMessage::Announce(announce_message));
        self.announce_cycle.next();
        self.log.message_sent("Announce");
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
    ) -> Option<StateDecision> {
        self.log.message_received("DelayReq");
        self.port
            .send_general(GeneralMessage::DelayResp(req.response(ingress_timestamp)));

        None
    }

    pub fn send_sync(&mut self) {
        let sync_message = self.sync_cycle.two_step_sync();
        self.port
            .send_event(EventMessage::TwoStepSync(sync_message));
        self.sync_cycle.next(self.timing_policy.sync_interval());
        self.log.message_sent("TwoStepSync");
    }

    pub fn send_follow_up(
        &mut self,
        sync: TwoStepSyncMessage,
        egress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.port
            .send_general(GeneralMessage::FollowUp(sync.follow_up(egress_timestamp)));
        self.log.message_sent("FollowUp");
        None
    }

    pub fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log
            .state_transition("Master", "Pre-Master", "Recommended Master");

        decision.apply(
            self.port,
            self.bmca.into_inner(),
            self.log,
            self.timing_policy,
        )
    }

    pub fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        self.log.state_transition(
            "Master",
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

#[derive(Debug, PartialEq, Eq)]
pub struct AnnounceCycle<T: Timeout> {
    sequence_id: SequenceId,
    log_interval: LogInterval,
    timeout: T,
}

impl<T: Timeout> AnnounceCycle<T> {
    pub fn new(start: SequenceId, log_interval: LogInterval, timeout: T) -> Self {
        Self {
            sequence_id: start,
            log_interval,
            timeout,
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
}

impl<T: Timeout> SyncCycle<T> {
    pub fn new(start: SequenceId, timeout: T) -> Self {
        Self {
            sequence_id: start,
            timeout,
        }
    }

    pub fn next(&mut self, interval: Duration) {
        self.timeout.restart(interval);
        self.sequence_id = self.sequence_id.next();
    }

    pub fn two_step_sync(&self) -> TwoStepSyncMessage {
        TwoStepSyncMessage::new(self.sequence_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{DefaultDS, ForeignClockDS, ForeignClockRecord, IncrementalBmca};
    use crate::clock::{LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::{
        DelayResponseMessage, EventMessage, FollowUpMessage, GeneralMessage, SystemMessage,
        TwoStepSyncMessage,
    };
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::test_support::{FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping};
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    #[test]
    fn master_port_answers_delay_request_with_delay_response() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
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
            LogInterval::new(0),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        timer_host.take_system_messages();

        master.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                0.into(),
                TimeStamp::new(0, 0)
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

        let mut master = PortState::master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        master.dispatch_system(SystemMessage::SyncTimeout);

        let messages = port.take_event_messages();
        assert!(
            messages.contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(
                0.into()
            )))
        );
    }

    #[test]
    fn master_port_schedules_next_sync() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
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

        let mut master = PortState::master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
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
            LogInterval::new(0),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        timer_host.take_system_messages();

        master.send_follow_up(TwoStepSyncMessage::new(0.into()), TimeStamp::new(0, 0));

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::FollowUp(FollowUpMessage::new(
                0.into(),
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

        let mut master = PortState::master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
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

        let mut master = PortState::master(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::Announce(AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock()
            )))
        );
    }

    #[test]
    fn master_port_to_uncalibrated_transition_on_two_announces() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
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
            LogInterval::new(0),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let decision = master.process_announce(
            AnnounceMessage::new(42.into(), LogMessageInterval::new(0), foreign_clock_ds),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(matches!(decision, None));

        let decision = master.process_announce(
            AnnounceMessage::new(43.into(), LogMessageInterval::new(0), foreign_clock_ds),
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
        );
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(
            PortIdentity::fake(),
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )
        .qualify()];
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
            LogInterval::new(0),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(
                SortedForeignClockRecordsVec::from_records(&prior_records),
            )),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Drain any setup timers
        timer_host.take_system_messages();

        let decision = master.process_announce(
            AnnounceMessage::new(42.into(), LogMessageInterval::new(0), foreign_clock_ds),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(matches!(decision, None));
        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_stays_master_on_undecided_bmca() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
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
            LogInterval::new(0),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Drain any setup timers
        timer_host.take_system_messages();

        let transition = master.process_announce(
            AnnounceMessage::new(42.into(), LogMessageInterval::new(0), foreign_clock_ds),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(matches!(transition, None));
        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_does_not_recommend_master_when_local_clock_unchanged_but_still_best() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );
        let parent_port = PortIdentity::fake();
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )
        .qualify()];
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
            LogInterval::new(0),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(
                SortedForeignClockRecordsVec::from_records(&prior_records),
            )),
            announce_cycle,
            sync_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        // Receive a better announce (but still lower quality than local high-grade clock)
        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::mid_grade_test_clock(),
            ),
            parent_port,
            Instant::from_secs(0),
        );

        // expect no state change - master stays master when receiving worse announces
        assert!(matches!(decision, None));
    }

    #[test]
    fn announce_cycle_produces_announce_messages_with_monotonic_sequence_ids() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let mut cycle = AnnounceCycle::new(
            0.into(),
            LogInterval::new(0),
            FakeTimeout::new(SystemMessage::AnnounceSendTimeout),
        );
        let msg1 = cycle.announce(&local_clock);
        cycle.next();
        let msg2 = cycle.announce(&local_clock);

        assert_eq!(
            msg1,
            AnnounceMessage::new(
                0.into(),
                LogInterval::new(0).log_message_interval(),
                ForeignClockDS::high_grade_test_clock()
            )
        );
        assert_eq!(
            msg2,
            AnnounceMessage::new(
                1.into(),
                LogInterval::new(0).log_message_interval(),
                ForeignClockDS::high_grade_test_clock()
            )
        );
    }

    #[test]
    fn sync_cycle_message_produces_two_step_sync_message() {
        let sync_cycle = SyncCycle::new(0.into(), FakeTimeout::new(SystemMessage::SyncTimeout));
        let two_step_sync = sync_cycle.two_step_sync();

        assert_eq!(two_step_sync, TwoStepSyncMessage::new(0.into()));
    }

    #[test]
    fn sync_cycle_next() {
        let mut sync_cycle = SyncCycle::new(0.into(), FakeTimeout::new(SystemMessage::SyncTimeout));
        sync_cycle.next(Duration::from_secs(1));

        assert_eq!(
            sync_cycle,
            SyncCycle::new(1.into(), FakeTimeout::new(SystemMessage::SyncTimeout))
        );
    }

    #[test]
    fn sync_cycle_next_wraps() {
        let mut sync_cycle = SyncCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
        );
        sync_cycle.next(Duration::from_secs(1));

        assert_eq!(
            sync_cycle,
            SyncCycle::new(0.into(), FakeTimeout::new(SystemMessage::SyncTimeout))
        );
    }
}
