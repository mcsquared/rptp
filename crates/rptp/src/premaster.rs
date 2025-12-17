use crate::bmca::{
    Bmca, BmcaDecision, BmcaSlaveDecision, LocalMasterTrackingBmca, ParentTrackingBmca,
};
use crate::log::{PortEvent, PortLog};
use crate::message::AnnounceMessage;
use crate::port::{Port, PortIdentity};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::time::Instant;

pub struct PreMasterPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: LocalMasterTrackingBmca<B>,
    _qualification_timeout: P::Timeout,
    log: L,
    profile: PortProfile,
}

impl<P: Port, B: Bmca, L: PortLog> PreMasterPort<P, B, L> {
    pub(crate) fn new(
        port: P,
        bmca: LocalMasterTrackingBmca<B>,
        _qualification_timeout: P::Timeout,
        log: L,
        profile: PortProfile,
    ) -> Self {
        log.port_event(PortEvent::Static("Become PreMasterPort"));

        Self {
            port,
            bmca,
            _qualification_timeout,
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

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => None,
        }
    }

    pub(crate) fn qualified(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::QualifiedMaster);
        self.profile.master(self.port, self.bmca, self.log)
    }

    pub(crate) fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        decision.apply(|parent_port_identity, steps_removed| {
            self.log.port_event(PortEvent::RecommendedSlave {
                parent: parent_port_identity,
            });

            let parent_tracking_bmca =
                ParentTrackingBmca::new(self.bmca.into_inner(), parent_port_identity);

            // Update steps removed as per IEEE 1588-2019 Section 9.3.5, Table 16
            self.port.update_steps_removed(steps_removed);

            self.profile
                .uncalibrated(self.port, parent_tracking_bmca, self.log)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{DefaultDS, IncrementalBmca};
    use crate::clock::{LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{SystemMessage, TimeScale};
    use crate::port::{DomainNumber, DomainPort, PortNumber, Timeout};
    use crate::portstate::PortState;
    use crate::portstate::StateDecision;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakePort, FakeTimerHost, FakeTimestamping};
    use crate::time::{Duration, Instant, LogMessageInterval};

    #[test]
    fn pre_master_port_schedules_qualification_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let qualification_timeout = domain_port.timeout(SystemMessage::QualificationTimeout);
        qualification_timeout.restart(Duration::from_secs(5));

        let _ = PreMasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            qualification_timeout,
            NoopPortLog,
            PortProfile::default(),
        );

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_port_to_master_transition_on_qualification_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let qualification_timeout = domain_port.timeout(SystemMessage::QualificationTimeout);
        qualification_timeout.restart(Duration::from_secs(5));

        let mut pre_master = PortState::PreMaster(PreMasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            qualification_timeout,
            NoopPortLog,
            PortProfile::default(),
        ));

        let transition = pre_master.dispatch_system(SystemMessage::QualificationTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::QualificationTimeoutExpired)
        ));
    }

    #[test]
    fn pre_master_port_produces_slave_recommendation_on_two_better_announces() {
        use crate::bmca::{BmcaSlaveDecision, ForeignClockDS};
        use crate::clock::ClockIdentity;
        use crate::message::AnnounceMessage;
        use crate::port::{ParentPortIdentity, PortIdentity, PortNumber};

        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let better_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let qualification_timeout = domain_port.timeout(SystemMessage::QualificationTimeout);
        qualification_timeout.restart(Duration::from_secs(5));

        let mut pre_master = PreMasterPort::new(
            domain_port,
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            qualification_timeout,
            NoopPortLog,
            PortProfile::default(),
        );

        // Receive first better announce
        let decision = pre_master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock(),
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
                ForeignClockDS::high_grade_test_clock(),
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
