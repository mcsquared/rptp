use std::time::Duration;

use crate::bmca::{Bmca, BmcaMasterDecision, BmcaSlaveDecision, QualificationTimeoutPolicy};
use crate::faulty::FaultyPort;
use crate::initializing::InitializingPort;
use crate::listening::ListeningPort;
use crate::log::PortLog;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::port::{ParentPortIdentity, Port, PortIdentity, PortTimingPolicy};
use crate::premaster::PreMasterPort;
use crate::slave::{DelayCycle, SlavePort};
use crate::time::TimeStamp;
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
    Unimplemented, // Placeholder for unimplemented transitions, to be removed once all transitions are implemented
}

impl<P: Port, B: Bmca, L: PortLog> PortState<P, B, L> {
    pub fn initializing(port: P, bmca: B, log: L, timing_policy: PortTimingPolicy) -> Self {
        PortState::Initializing(InitializingPort::new(port, bmca, log, timing_policy))
    }

    pub fn master(port: P, bmca: B, log: L, timing_policy: PortTimingPolicy) -> Self {
        let announce_send_timeout =
            port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0));
        let announce_cycle = AnnounceCycle::new(0.into(), announce_send_timeout);
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
        bmca: B,
        parent_port_identity: ParentPortIdentity,
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
            parent_port_identity,
            announce_receipt_timeout,
            delay_cycle,
            log,
            timing_policy,
        ))
    }

    pub fn pre_master(
        port: P,
        bmca: B,
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
        bmca: B,
        log: L,
        parent_port_identity: ParentPortIdentity,
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
            parent_port_identity,
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
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::MasterClockSelected => match self {
                PortState::Uncalibrated(uncalibrated) => uncalibrated.master_clock_selected(),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::RecommendedSlave(decision) => match self {
                PortState::Listening(listening) => listening.recommended_slave(decision),
                PortState::Master(master) => master.recommended_slave(decision),
                PortState::PreMaster(_) => PortState::Unimplemented,
                PortState::Slave(_) => PortState::Unimplemented,
                PortState::Uncalibrated(uncalibrated) => uncalibrated.recommended_slave(decision),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::RecommendedMaster(decision) => match self {
                PortState::Listening(listening) => listening.recommended_master(decision),
                PortState::Uncalibrated(_) => PortState::Unimplemented,
                PortState::Master(_) => PortState::Unimplemented,
                PortState::Slave(_) => PortState::Unimplemented,
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::Initialized => match self {
                PortState::Initializing(initializing) => initializing.initialized(),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::QualificationTimeoutExpired => match self {
                PortState::PreMaster(pre_master) => pre_master.qualified(),
                _ => PortState::Faulty(FaultyPort::new()),
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
            (Slave(port), TwoStepSync(msg)) => {
                port.process_two_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Master(port), DelayReq(msg)) => port.process_delay_request(msg, ingress_timestamp),
            _ => None,
        }
    }

    pub fn dispatch_general(
        &mut self,
        msg: GeneralMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        use GeneralMessage::*;
        use PortState::*;

        match (self, msg) {
            (Listening(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Slave(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Master(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Uncalibrated(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Slave(port), FollowUp(msg)) => port.process_follow_up(msg, source_port_identity),
            (Slave(port), DelayResp(msg)) => port.process_delay_response(msg, source_port_identity),
            _ => None,
        }
    }

    pub fn dispatch_system(&mut self, msg: SystemMessage) -> Option<StateDecision> {
        use PortState::*;
        use SystemMessage::*;

        match (self, msg) {
            (Master(port), AnnounceSendTimeout) => {
                port.send_announce();
                None
            }
            (Slave(port), DelayRequestTimeout) => {
                port.send_delay_request();
                None
            }
            (Master(port), SyncTimeout) => {
                port.send_sync();
                None
            }
            (Master(port), Timestamp(msg)) => match msg.event_msg {
                EventMessage::TwoStepSync(sync_msg) => {
                    port.send_follow_up(sync_msg, msg.egress_timestamp)
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

    use crate::bmca::BmcaMasterDecisionPoint;
    use crate::bmca::{DefaultDS, FullBmca};
    use crate::clock::{FakeClock, LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};

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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            ParentPortIdentity::new(PortIdentity::fake()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            ParentPortIdentity::new(PortIdentity::fake()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let listening = initializing.apply(StateDecision::Initialized);

        assert!(matches!(listening, PortState::Listening(_)));
    }

    // Tests for unimplemented/illegal ToMaster transitions

    #[test]
    fn portstate_initializing_to_master_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = initializing.apply(StateDecision::AnnounceReceiptTimeoutExpired);
        // TODO:: do we need to check for all possible transition types from illegal to faulty?

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_master_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = master.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for illegal ToSlave transitions

    #[test]
    fn portstate_initializing_to_slave_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = initializing.apply(StateDecision::MasterClockSelected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_listening_to_slave_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = listening.apply(StateDecision::MasterClockSelected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_slave_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = slave.apply(StateDecision::MasterClockSelected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_slave_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = master.apply(StateDecision::MasterClockSelected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_slave_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        let result = pre_master.apply(StateDecision::MasterClockSelected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToUncalibrated transitions

    #[test]
    fn portstate_initializing_to_uncalibrated_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = initializing.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            parent_port_identity,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_uncalibrated_unimplemented_transition() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = slave.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            parent_port_identity,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Unimplemented));
    }

    #[test]
    fn portstate_pre_master_to_uncalibrated_unimplemented_transition() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = pre_master.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            parent_port_identity,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Unimplemented));
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            ParentPortIdentity::new(PortIdentity::fake()),
            PortTimingPolicy::default(),
        );

        let result = uncalibrated.apply(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
            ParentPortIdentity::new(PortIdentity::fake()),
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    // Tests for unimplemented/illegal ToPreMaster transitions

    #[test]
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
    fn portstate_slave_to_pre_master_unimplemented_transition() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = slave.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Unimplemented));
    }

    #[test]
    fn portstate_master_to_pre_master_unimplemented_transition() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = master.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Unimplemented));
    }

    #[test]
    fn portstate_pre_master_to_pre_master_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        let result = pre_master.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_pre_master_unimplemented_transition() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            ParentPortIdentity::new(PortIdentity::fake()),
            PortTimingPolicy::default(),
        );

        let result = uncalibrated.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
        )));

        assert!(matches!(result, PortState::Unimplemented));
    }

    // Tests for unimplemented/illegal ToListening transitions

    #[test]
    fn portstate_listening_to_listening_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = listening.apply(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_listening_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = slave.apply(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_listening_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = master.apply(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_listening_illegal_transition_goes_to_faulty() {
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
        );

        let result = pre_master.apply(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            PortTimingPolicy::default(),
        );

        let result = master.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
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
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopPortLog,
            ParentPortIdentity::new(PortIdentity::fake()),
            PortTimingPolicy::default(),
        );

        let result = uncalibrated.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_faulty_to_faulty_transition() {
        let faulty: PortState<
            DomainPort<FakeClock, FakePort, FakeTimerHost>,
            FullBmca<SortedForeignClockRecordsVec>,
            NoopPortLog,
        > = PortState::Faulty(FaultyPort::new());

        let result = faulty.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }
}
