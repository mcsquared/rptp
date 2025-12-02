use core::panic;

use crate::bmca::{
    Bmca, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca, ParentTrackingBmca,
    QualificationTimeoutPolicy,
};
use crate::faulty::FaultyPort;
use crate::initializing::InitializingPort;
use crate::listening::ListeningPort;
use crate::log::PortLog;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::port::{Port, PortIdentity, PortTimingPolicy};
use crate::premaster::PreMasterPort;
use crate::slave::{DelayCycle, SlavePort};
use crate::time::{Duration, Instant, TimeStamp};
use crate::uncalibrated::UncalibratedPort;

// Possible decisions that move the port state machine from one state
// to another as defined in IEEE 1588 Section 9.2.5, figure 24
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateDecision {
    Initialized,
    MasterClockSelected,
    RecommendedSlave(BmcaSlaveDecision),
    RecommendedMaster(BmcaMasterDecision),
    FaultDetected,
    QualificationTimeoutExpired,
    AnnounceReceiptTimeoutExpired,
}

// Port states as defined in IEEE 1588 Section 9.2.5, figure 24
pub enum PortState<P: Port, B: Bmca, L: PortLog> {
    Initializing(InitializingPort<P, B, L>),
    Listening(ListeningPort<P, B, L>),
    Slave(SlavePort<P, B, L>),
    Master(MasterPort<P, B, L>),
    PreMaster(PreMasterPort<P, B, L>),
    Uncalibrated(UncalibratedPort<P, B, L>),
    Faulty(FaultyPort<P, B, L>),
}

impl<P: Port, B: Bmca, L: PortLog> PortState<P, B, L> {
    pub fn initializing(port: P, bmca: B, log: L, timing_policy: PortTimingPolicy) -> Self {
        PortState::Initializing(InitializingPort::new(port, bmca, log, timing_policy))
    }

    pub fn master(
        port: P,
        bmca: LocalMasterTrackingBmca<B>,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        let announce_send_timeout =
            port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0));
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            timing_policy.log_announce_interval(),
            announce_send_timeout,
        );
        let sync_timeout = port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout);

        PortState::Master(MasterPort::new(
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            log,
            timing_policy,
        ))
    }

    pub fn slave(
        port: P,
        bmca: ParentTrackingBmca<B>,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            timing_policy.announce_receipt_timeout_interval(),
        );

        let delay_cycle = DelayCycle::new(
            0.into(),
            port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0)),
        );

        PortState::Slave(SlavePort::new(
            port,
            bmca,
            announce_receipt_timeout,
            delay_cycle,
            log,
            timing_policy,
        ))
    }

    pub fn pre_master(
        port: P,
        bmca: LocalMasterTrackingBmca<B>,
        log: L,
        timing_policy: PortTimingPolicy,
        qualification_timeout_policy: QualificationTimeoutPolicy,
    ) -> Self {
        let qualification_timeout = port.timeout(
            SystemMessage::QualificationTimeout,
            timing_policy.qualification_interval(qualification_timeout_policy),
        );

        PortState::PreMaster(PreMasterPort::new(
            port,
            bmca,
            qualification_timeout,
            log,
            timing_policy,
        ))
    }

    pub fn listening(port: P, bmca: B, log: L, timing_policy: PortTimingPolicy) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            timing_policy.announce_receipt_timeout_interval(),
        );

        PortState::Listening(ListeningPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            log,
            timing_policy,
        ))
    }

    pub fn uncalibrated(
        port: P,
        bmca: ParentTrackingBmca<B>,
        log: L,
        timing_policy: PortTimingPolicy,
    ) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            timing_policy.announce_receipt_timeout_interval(),
        );

        PortState::Uncalibrated(UncalibratedPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            log,
            timing_policy,
        ))
    }

    pub fn apply(self, decision: StateDecision) -> Self {
        match decision {
            StateDecision::AnnounceReceiptTimeoutExpired => match self {
                PortState::Listening(listening) => listening.announce_receipt_timeout_expired(),
                PortState::Slave(slave) => slave.announce_receipt_timeout_expired(),
                PortState::Uncalibrated(uncalibrated) => {
                    uncalibrated.announce_receipt_timeout_expired()
                }
                _ => panic!(
                    "AnnounceReceiptTimeoutExpired can only be applied in Listening, Slave, or Uncalibrated states"
                ),
            },
            StateDecision::MasterClockSelected => match self {
                PortState::Uncalibrated(uncalibrated) => uncalibrated.master_clock_selected(),
                _ => panic!("MasterClockSelected can only be applied in Uncalibrated state"),
            },
            StateDecision::RecommendedSlave(decision) => match self {
                PortState::Listening(listening) => listening.recommended_slave(decision),
                PortState::Master(master) => master.recommended_slave(decision),
                PortState::PreMaster(pre_master) => pre_master.recommended_slave(decision),
                PortState::Slave(slave) => slave.recommended_slave(decision),
                PortState::Uncalibrated(uncalibrated) => uncalibrated.recommended_slave(decision),
                _ => panic!(
                    "RecommendedSlave can only be applied in Listening, Master, PreMaster, Slave, or Uncalibrated states"
                ),
            },
            StateDecision::RecommendedMaster(decision) => match self {
                PortState::Listening(listening) => listening.recommended_master(decision),
                PortState::Uncalibrated(uncalibrated) => uncalibrated.recommended_master(decision),
                PortState::Master(master) => master.recommended_master(decision),
                PortState::Slave(slave) => slave.recommended_master(decision),
                _ => panic!(
                    "RecommendedMaster can only be applied in Listening, Uncalibrated, Master, or Slave states"
                ),
            },
            StateDecision::Initialized => match self {
                PortState::Initializing(initializing) => initializing.initialized(),
                _ => panic!("Initialized can only be applied in Initializing state"),
            },
            StateDecision::QualificationTimeoutExpired => match self {
                PortState::PreMaster(pre_master) => pre_master.qualified(),
                _ => panic!("QualificationTimeoutExpired can only be applied in PreMaster state"),
            },
            StateDecision::FaultDetected => PortState::Faulty(FaultyPort::new()),
        }
    }

    pub fn dispatch_event(
        &mut self,
        msg: EventMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        use EventMessage::*;
        use PortState::*;

        match (self, msg) {
            (Slave(port), OneStepSync(msg)) => {
                port.process_one_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Slave(port), TwoStepSync(msg)) => {
                port.process_two_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Master(port), DelayReq(msg)) => {
                match port.process_delay_request(msg, ingress_timestamp) {
                    Ok(()) => None,
                    Err(_) => Some(StateDecision::FaultDetected),
                }
            }
            _ => None,
        }
    }

    pub fn dispatch_general(
        &mut self,
        msg: GeneralMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        use GeneralMessage::*;
        use PortState::*;

        match (self, msg) {
            (Listening(port), Announce(msg)) => {
                port.process_announce(msg, source_port_identity, now)
            }
            (Slave(port), Announce(msg)) => port.process_announce(msg, source_port_identity, now),
            (Master(port), Announce(msg)) => port.process_announce(msg, source_port_identity, now),
            (PreMaster(port), Announce(msg)) => {
                port.process_announce(msg, source_port_identity, now)
            }
            (Uncalibrated(port), Announce(msg)) => {
                port.process_announce(msg, source_port_identity, now)
            }
            (Slave(port), FollowUp(msg)) => port.process_follow_up(msg, source_port_identity),
            (Slave(port), DelayResp(msg)) => port.process_delay_response(msg, source_port_identity),
            _ => None,
        }
    }

    pub fn dispatch_system(&mut self, msg: SystemMessage) -> Option<StateDecision> {
        use PortState::*;
        use SystemMessage::*;

        match (self, msg) {
            (Master(port), AnnounceSendTimeout) => match port.send_announce() {
                Ok(()) => None,
                Err(_) => Some(StateDecision::FaultDetected),
            },
            (Slave(port), DelayRequestTimeout) => match port.send_delay_request() {
                Ok(()) => None,
                Err(_) => Some(StateDecision::FaultDetected),
            },
            (Master(port), SyncTimeout) => match port.send_sync() {
                Ok(()) => None,
                Err(_) => Some(StateDecision::FaultDetected),
            },
            (Master(port), Timestamp(msg)) => match msg.event_msg {
                EventMessage::TwoStepSync(sync_msg) => {
                    match port.send_follow_up(sync_msg, msg.egress_timestamp) {
                        Ok(()) => None,
                        Err(_) => Some(StateDecision::FaultDetected),
                    }
                }
                _ => None,
            },
            (Slave(port), Timestamp(msg)) => match msg.event_msg {
                EventMessage::DelayReq(req_msg) => {
                    port.process_delay_request(req_msg, msg.egress_timestamp)
                }
                _ => None,
            },
            (Initializing(_), SystemMessage::Initialized) => Some(StateDecision::Initialized),
            (Listening(_) | Slave(_) | Master(_) | Uncalibrated(_), AnnounceReceiptTimeout) => {
                Some(StateDecision::AnnounceReceiptTimeoutExpired)
            }
            (PreMaster(_), QualificationTimeout) => {
                Some(StateDecision::QualificationTimeoutExpired)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{BmcaMasterDecisionPoint, DefaultDS, IncrementalBmca, NoopBmca};
    use crate::clock::{LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::{DelayRequestMessage, TimestampMessage, TwoStepSyncMessage};
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::test_support::{FailingPort, FakeClock, FakePort, FakeTimerHost, FakeTimestamping};
    use crate::time::TimeStamp;

    #[test]
    fn portstate_listening_to_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let master = listening.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_slave_to_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let master = slave.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_pre_master_to_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        let master = pre_master.apply(StateDecision::QualificationTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let master = uncalibrated.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_slave_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let slave = uncalibrated.apply(StateDecision::MasterClockSelected);

        assert!(matches!(slave, PortState::Slave(_)));
    }

    #[test]
    fn portstate_listening_to_uncalibrated_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let uncalibrated = listening.apply(StateDecision::RecommendedSlave(
            BmcaSlaveDecision::new(parent_port_identity, StepsRemoved::new(0)),
        ));

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_master_to_uncalibrated_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let uncalibrated = master.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            parent_port_identity,
            StepsRemoved::new(0),
        )));

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_listening_to_pre_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let pre_master = listening.apply(StateDecision::RecommendedMaster(
            BmcaMasterDecision::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        ));

        assert!(matches!(pre_master, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_initializing_to_listening_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let listening = initializing.apply(StateDecision::Initialized);

        assert!(matches!(listening, PortState::Listening(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_master_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        initializing.apply(StateDecision::AnnounceReceiptTimeoutExpired);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_master_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        master.apply(StateDecision::AnnounceReceiptTimeoutExpired);
    }

    // Tests for illegal ToSlave transitions

    #[test]
    #[should_panic]
    fn portstate_initializing_to_slave_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        initializing.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_listening_to_slave_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        listening.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_slave_to_slave_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        slave.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_slave_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        master.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_pre_master_to_slave_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        pre_master.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_uncalibrated_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        initializing.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            parent_port_identity,
            StepsRemoved::new(0),
        )));
    }

    #[test]
    fn portstate_slave_to_uncalibrated_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = slave.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            parent_port_identity,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_pre_master_to_uncalibrated_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = pre_master.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            parent_port_identity,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_uncalibrated_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = uncalibrated.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            ParentPortIdentity::new(PortIdentity::fake()),
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_pre_master_illegal_transition_goes_to_faulty() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = initializing.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_pre_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = slave.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_master_to_pre_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = master.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::PreMaster(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_pre_master_to_pre_master_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        pre_master.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));
    }

    #[test]
    fn portstate_uncalibrated_to_pre_master_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = uncalibrated.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::PreMaster(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_listening_to_listening_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        listening.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_slave_to_listening_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        slave.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_listening_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        master.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_pre_master_to_listening_illegal_transition_panics() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        pre_master.apply(StateDecision::Initialized);
    }

    // Tests for explicit ToFaulty transitions from every state

    #[test]
    fn portstate_initializing_to_faulty_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = initializing.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_listening_to_faulty_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = listening.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_faulty_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = slave.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_faulty_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = master.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_announce_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let mut master = PortState::master(
            DomainPort::new(
                &local_clock,
                FailingPort, // <-- Failing port to simulate send failure <--
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(NoopBmca),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let transition = master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_delay_response_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let mut master = PortState::master(
            DomainPort::new(
                &local_clock,
                FailingPort,
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(NoopBmca),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let transition = master.dispatch_event(
            EventMessage::DelayReq(DelayRequestMessage::new(0.into())),
            PortIdentity::fake(),
            TimeStamp::new(0, 0),
        );

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_follow_up_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let mut master = PortState::master(
            DomainPort::new(
                &local_clock,
                FailingPort, // <-- failing port to simulate send failure <--
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(NoopBmca),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let sync_msg = TwoStepSyncMessage::new(0.into());
        let ts_msg =
            TimestampMessage::new(EventMessage::TwoStepSync(sync_msg), TimeStamp::new(0, 0));

        let transition = master.dispatch_system(SystemMessage::Timestamp(ts_msg));

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_sync_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let mut master = PortState::master(
            DomainPort::new(
                &local_clock,
                FailingPort,
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(NoopBmca),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let transition = master.dispatch_system(SystemMessage::SyncTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_slave_enters_faulty_on_delay_request_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let domain_port = DomainPort::new(
            &local_clock,
            FailingPort,
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let bmca = ParentTrackingBmca::new(
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            parent_port_identity,
        );

        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(10),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        let mut slave = PortState::Slave(SlavePort::new(
            domain_port,
            bmca,
            announce_receipt_timeout,
            delay_cycle,
            NoopPortLog,
            PortTimingPolicy::default(),
        ));

        let transition = slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_pre_master_to_faulty_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new())),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        let result = pre_master.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_faulty_transition() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
        );

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = uncalibrated.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_faulty_to_faulty_transition() {
        let faulty: PortState<
            DomainPort<FakeClock, FakePort, FakeTimerHost, FakeTimestamping>,
            IncrementalBmca<SortedForeignClockRecordsVec>,
            NoopPortLog,
        > = PortState::Faulty(FaultyPort::new());

        let result = faulty.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }
}
