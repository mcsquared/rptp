use std::time::Duration;

use crate::bmca::Bmca;
use crate::faulty::FaultyPort;
use crate::initializing::InitializingPort;
use crate::listening::ListeningPort;
use crate::log::Log;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::port::{ParentPortIdentity, Port, PortIdentity};
use crate::premaster::PreMasterPort;
use crate::slave::{DelayCycle, SlavePort};
use crate::time::TimeStamp;
use crate::uncalibrated::UncalibratedPort;

pub enum StateDecision {
    Initialized,
    MasterClockSelected(ParentPortIdentity),
    RecommendedSlave,
    RecommendedMaster,
    FaultDetected,
    QualificationTimeoutExpired,
    AnnounceReceiptTimeoutExpired,
}

pub enum PortState<P: Port, B: Bmca, L: Log> {
    Initializing(InitializingPort<P, B, L>),
    Listening(ListeningPort<P, B, L>),
    Slave(SlavePort<P, B, L>),
    Master(MasterPort<P, B, L>),
    PreMaster(PreMasterPort<P, B, L>),
    Uncalibrated(UncalibratedPort<P, B, L>),
    Faulty(FaultyPort<P, B, L>),
}

impl<P: Port, B: Bmca, L: Log> PortState<P, B, L> {
    pub fn initializing(port: P, bmca: B, log: L) -> Self {
        PortState::Initializing(InitializingPort::new(port, bmca, log))
    }

    pub fn master(port: P, bmca: B, log: L) -> Self {
        let announce_send_timeout =
            port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0));
        let announce_cycle = AnnounceCycle::new(0.into(), announce_send_timeout);
        let sync_timeout = port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout);

        PortState::Master(MasterPort::new(port, bmca, announce_cycle, sync_cycle, log))
    }

    pub fn slave(port: P, bmca: B, parent_port_identity: ParentPortIdentity, log: L) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
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
        ))
    }

    pub fn pre_master(port: P, bmca: B, log: L) -> Self {
        let qualification_timeout =
            port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        PortState::PreMaster(PreMasterPort::new(port, bmca, qualification_timeout, log))
    }

    pub fn listening(port: P, bmca: B, log: L) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        PortState::Listening(ListeningPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            log,
        ))
    }

    pub fn uncalibrated(port: P, bmca: B, log: L) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        PortState::Uncalibrated(UncalibratedPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            log,
        ))
    }

    pub fn transit(self, transition: StateDecision) -> Self {
        match transition {
            StateDecision::AnnounceReceiptTimeoutExpired => match self {
                PortState::Listening(listening) => {
                    PortState::Master(listening.announce_receipt_timeout_expired())
                }
                PortState::Slave(slave) => {
                    PortState::Master(slave.announce_receipt_timeout_expired())
                }
                PortState::Uncalibrated(uncalibrated) => {
                    PortState::Master(uncalibrated.announce_receipt_timeout_expired())
                }
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::MasterClockSelected(parent_port_identity) => match self {
                PortState::Uncalibrated(uncalibrated) => {
                    PortState::Slave(uncalibrated.master_clock_selected(parent_port_identity))
                }
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::RecommendedSlave => match self {
                PortState::Listening(listening) => {
                    PortState::Uncalibrated(listening.recommended_slave())
                }
                PortState::Master(master) => PortState::Uncalibrated(master.recommended_slave()),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::RecommendedMaster => match self {
                PortState::Listening(listening) => {
                    PortState::PreMaster(listening.recommended_master())
                }
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::Initialized => match self {
                PortState::Initializing(initializing) => {
                    PortState::Listening(initializing.initialized())
                }
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateDecision::QualificationTimeoutExpired => match self {
                PortState::PreMaster(pre_master) => PortState::Master(pre_master.qualified()),
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

    use crate::bmca::{FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopLog;
    use crate::port::test_support::{FakePort, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};

    #[test]
    fn portstate_listening_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let master = listening.transit(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_slave_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

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
            NoopLog,
        );

        let master = slave.transit(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_pre_master_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let master = pre_master.transit(StateDecision::QualificationTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let master = uncalibrated.transit(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_slave_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let slave = uncalibrated.transit(StateDecision::MasterClockSelected(parent));

        assert!(matches!(slave, PortState::Slave(_)));
    }

    #[test]
    fn portstate_listening_to_uncalibrated_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let uncalibrated = listening.transit(StateDecision::RecommendedSlave);

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_master_to_uncalibrated_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let uncalibrated = master.transit(StateDecision::RecommendedSlave);

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_listening_to_pre_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let pre_master = listening.transit(StateDecision::RecommendedMaster);

        assert!(matches!(pre_master, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_initializing_to_listening_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let listening = initializing.transit(StateDecision::Initialized);

        assert!(matches!(listening, PortState::Listening(_)));
    }

    // Tests for unimplemented/illegal ToMaster transitions

    #[test]
    fn portstate_initializing_to_master_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateDecision::AnnounceReceiptTimeoutExpired);
        // TODO:: do we need to check for all possible transition types from illegal to faulty?

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_master_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToSlave transitions

    #[test]
    fn portstate_initializing_to_slave_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = initializing.transit(StateDecision::MasterClockSelected(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_listening_to_slave_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = listening.transit(StateDecision::MasterClockSelected(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_slave_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

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
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = slave.transit(StateDecision::MasterClockSelected(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_slave_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = master.transit(StateDecision::MasterClockSelected(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_slave_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = pre_master.transit(StateDecision::MasterClockSelected(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToUncalibrated transitions

    #[test]
    fn portstate_initializing_to_uncalibrated_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateDecision::RecommendedSlave);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_uncalibrated_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

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
            NoopLog,
        );

        let result = slave.transit(StateDecision::RecommendedSlave);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_uncalibrated_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateDecision::RecommendedSlave);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_uncalibrated_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = uncalibrated.transit(StateDecision::RecommendedSlave);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToPreMaster transitions

    #[test]
    fn portstate_initializing_to_pre_master_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateDecision::RecommendedMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_pre_master_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

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
            NoopLog,
        );

        let result = slave.transit(StateDecision::RecommendedMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_pre_master_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateDecision::RecommendedMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_pre_master_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateDecision::RecommendedMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_pre_master_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = uncalibrated.transit(StateDecision::RecommendedMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToListening transitions

    #[test]
    fn portstate_listening_to_listening_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = listening.transit(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_listening_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

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
            NoopLog,
        );

        let result = slave.transit(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_listening_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_listening_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateDecision::Initialized);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for explicit ToFaulty transitions from every state

    #[test]
    fn portstate_initializing_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_listening_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = listening.transit(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

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
            NoopLog,
        );

        let result = slave.transit(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = uncalibrated.transit(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_faulty_to_faulty_transition() {
        let faulty: PortState<
            DomainPort<FakeClock, FakePort, FakeTimerHost>,
            FullBmca<SortedForeignClockRecordsVec>,
            NoopLog,
        > = PortState::Faulty(FaultyPort::new());

        let result = faulty.transit(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }
}
