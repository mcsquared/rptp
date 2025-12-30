use core::panic;

use crate::bmca::{
    BmcaMasterDecision, GrandMasterTrackingBmca, ListeningBmca, ParentTrackingBmca,
    QualificationTimeoutPolicy, SortedForeignClockRecords,
};
use crate::e2e::{DelayCycle, EndToEndDelayMechanism};
use crate::faulty::FaultyPort;
use crate::initializing::InitializingPort;
use crate::listening::ListeningPort;
use crate::master::{AnnounceCycle, MasterPort, SyncCycle};
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::port::{AnnounceReceiptTimeout, ParentPortIdentity, Port, PortIdentity, Timeout};
use crate::premaster::PreMasterPort;
use crate::slave::SlavePort;
use crate::time::{Duration, Instant, LogInterval, TimeStamp};
use crate::uncalibrated::UncalibratedPort;

// Possible decisions that move the port state machine from one state
// to another as defined in IEEE 1588 Section 9.2.5, figure 24
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StateDecision {
    Initialized,
    MasterClockSelected,
    RecommendedSlave(ParentPortIdentity),
    RecommendedMaster(BmcaMasterDecision),
    FaultDetected,
    QualificationTimeoutExpired,
    AnnounceReceiptTimeoutExpired,
    SynchronizationFault,
}

// Port states as defined in IEEE 1588 Section 9.2.5, figure 24
#[allow(clippy::large_enum_variant)]
pub enum PortState<P: Port, S: SortedForeignClockRecords> {
    Initializing(InitializingPort<P, S>),
    Listening(ListeningPort<P, S>),
    Slave(SlavePort<P, S>),
    Master(MasterPort<P, S>),
    PreMaster(PreMasterPort<P, S>),
    Uncalibrated(UncalibratedPort<P, S>),
    Faulty(FaultyPort<P, S>),
}

impl<P: Port, S: SortedForeignClockRecords> PortState<P, S> {
    pub(crate) fn apply(self, decision: StateDecision) -> Self {
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
                PortState::PreMaster(pre_master) => pre_master.recommended_master(decision),
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
            StateDecision::SynchronizationFault => match self {
                PortState::Slave(slave) => slave.synchronization_fault(),
                _ => panic!("SynchronizationFault can only be applied in Slave state"),
            },
            StateDecision::FaultDetected => PortState::Faulty(FaultyPort::default()),
        }
    }

    pub(crate) fn dispatch_event(
        &mut self,
        msg: EventMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        use EventMessage::*;
        use PortState::*;

        match (self, msg) {
            (Uncalibrated(port), OneStepSync(msg)) => {
                port.process_one_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Uncalibrated(port), TwoStepSync(msg)) => {
                port.process_two_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Slave(port), OneStepSync(msg)) => {
                port.process_one_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Slave(port), TwoStepSync(msg)) => {
                port.process_two_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Master(port), DelayReq(msg)) => {
                match port.process_delay_request(msg, ingress_timestamp, source_port_identity) {
                    Ok(()) => None,
                    Err(_) => Some(StateDecision::FaultDetected),
                }
            }
            _ => None,
        }
    }

    pub(crate) fn dispatch_general(
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
            (Uncalibrated(port), FollowUp(msg)) => {
                port.process_follow_up(msg, source_port_identity)
            }
            (Uncalibrated(port), DelayResp(msg)) => {
                port.process_delay_response(msg, source_port_identity)
            }
            (Slave(port), FollowUp(msg)) => port.process_follow_up(msg, source_port_identity),
            (Slave(port), DelayResp(msg)) => port.process_delay_response(msg, source_port_identity),
            _ => None,
        }
    }

    pub(crate) fn dispatch_system(&mut self, msg: SystemMessage) -> Option<StateDecision> {
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
            (Uncalibrated(port), DelayRequestTimeout) => match port.send_delay_request() {
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
            (Uncalibrated(port), Timestamp(msg)) => match msg.event_msg {
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

pub struct PortProfile {
    announce_receipt_timeout_interval: Duration,
    log_announce_interval: LogInterval,
    log_sync_interval: LogInterval,
    log_min_delay_request_interval: LogInterval,
}

impl PortProfile {
    pub fn new(
        announce_receipt_timeout_interval: Duration,
        log_announce_interval: LogInterval,
        log_sync_interval: LogInterval,
        log_min_delay_request_interval: LogInterval,
    ) -> Self {
        Self {
            announce_receipt_timeout_interval,
            log_announce_interval,
            log_sync_interval,
            log_min_delay_request_interval,
        }
    }
}

impl Default for PortProfile {
    fn default() -> Self {
        Self {
            announce_receipt_timeout_interval: Duration::from_secs(5),
            log_announce_interval: LogInterval::new(0),
            log_sync_interval: LogInterval::new(0),
            log_min_delay_request_interval: LogInterval::new(0),
        }
    }
}

impl PortProfile {
    pub(crate) fn log_min_delay_request_interval(&self) -> LogInterval {
        self.log_min_delay_request_interval
    }

    pub fn initializing<P: Port, S: SortedForeignClockRecords>(
        self,
        port: P,
        sorted_foreign_clock_records: S,
    ) -> PortState<P, S> {
        PortState::Initializing(InitializingPort::new(
            port,
            sorted_foreign_clock_records,
            self,
        ))
    }

    pub(crate) fn listening<P: Port, S: SortedForeignClockRecords>(
        self,
        port: P,
        bmca: ListeningBmca<S>,
    ) -> PortState<P, S> {
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            port.timeout(SystemMessage::AnnounceReceiptTimeout),
            self.announce_receipt_timeout_interval,
        );
        announce_receipt_timeout.restart();

        PortState::Listening(ListeningPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            self,
        ))
    }

    pub fn master<P: Port, S: SortedForeignClockRecords>(
        self,
        port: P,
        bmca: GrandMasterTrackingBmca<S>,
    ) -> PortState<P, S> {
        let announce_send_timeout = port.timeout(SystemMessage::AnnounceSendTimeout);
        announce_send_timeout.restart(Duration::from_secs(0));
        let announce_cycle =
            AnnounceCycle::new(0.into(), announce_send_timeout, self.log_announce_interval);
        let sync_timeout = port.timeout(SystemMessage::SyncTimeout);
        sync_timeout.restart(Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout, self.log_sync_interval);

        PortState::Master(MasterPort::new(
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            self,
        ))
    }

    pub fn slave<P: Port, S: SortedForeignClockRecords>(
        self,
        port: P,
        bmca: ParentTrackingBmca<S>,
        delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
    ) -> PortState<P, S> {
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            port.timeout(SystemMessage::AnnounceReceiptTimeout),
            self.announce_receipt_timeout_interval,
        );
        announce_receipt_timeout.restart();

        PortState::Slave(SlavePort::new(
            port,
            bmca,
            announce_receipt_timeout,
            delay_mechanism,
            self,
        ))
    }

    pub(crate) fn pre_master<P: Port, S: SortedForeignClockRecords>(
        self,
        port: P,
        bmca: GrandMasterTrackingBmca<S>,
        qualification_timeout_policy: QualificationTimeoutPolicy,
    ) -> PortState<P, S> {
        let qualification_timeout = port.timeout(SystemMessage::QualificationTimeout);
        qualification_timeout
            .restart(qualification_timeout_policy.duration(self.log_announce_interval));

        PortState::PreMaster(PreMasterPort::new(port, bmca, qualification_timeout, self))
    }

    pub(crate) fn uncalibrated<P: Port, S: SortedForeignClockRecords>(
        self,
        port: P,
        bmca: ParentTrackingBmca<S>,
    ) -> PortState<P, S> {
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            port.timeout(SystemMessage::AnnounceReceiptTimeout),
            self.announce_receipt_timeout_interval,
        );
        announce_receipt_timeout.restart();

        let delay_timeout = port.timeout(SystemMessage::DelayRequestTimeout);
        delay_timeout.restart(Duration::from_secs(0));

        let delay_cycle =
            DelayCycle::new(0.into(), delay_timeout, self.log_min_delay_request_interval);

        PortState::Uncalibrated(UncalibratedPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(delay_cycle),
            self,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{
        BestForeignRecord, BmcaMasterDecision, BmcaMasterDecisionPoint, ClockDS, ListeningBmca,
    };
    use crate::clock::{ClockIdentity, LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{DelayRequestMessage, TimestampMessage, TwoStepSyncMessage};
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FailingPort, FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping,
        TestClockCatalog,
    };
    use crate::time::{LogMessageInterval, TimeStamp};

    type PortStateTestDomainPort<'a> =
        DomainPort<'a, FakeClock, FakeTimerHost, FakeTimestamping, NoopPortLog>;

    struct PortStateTestSetup {
        local_clock: LocalClock<FakeClock>,
        physical_port: FakePort,
    }

    impl PortStateTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    ds,
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                physical_port: FakePort::new(),
            }
        }

        fn domain_port<'a>(&'a self) -> PortStateTestDomainPort<'a> {
            DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            )
        }

        fn local_clock_identity(&self) -> &ClockIdentity {
            self.local_clock.identity()
        }

        fn initializing_port(
            &self,
        ) -> PortState<PortStateTestDomainPort<'_>, SortedForeignClockRecordsVec> {
            PortProfile::default()
                .initializing(self.domain_port(), SortedForeignClockRecordsVec::new())
        }

        fn listening_port(
            &self,
        ) -> PortState<PortStateTestDomainPort<'_>, SortedForeignClockRecordsVec> {
            PortProfile::default().listening(
                self.domain_port(),
                ListeningBmca::new(BestForeignRecord::new(SortedForeignClockRecordsVec::new())),
            )
        }

        fn slave_port(
            &self,
        ) -> PortState<PortStateTestDomainPort<'_>, SortedForeignClockRecordsVec> {
            PortProfile::default().slave(
                self.domain_port(),
                ParentTrackingBmca::new(
                    BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                    ParentPortIdentity::new(PortIdentity::fake()),
                ),
                EndToEndDelayMechanism::new(DelayCycle::new(
                    0.into(),
                    FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                    LogInterval::new(0),
                )),
            )
        }

        fn master_port(
            &self,
        ) -> PortState<PortStateTestDomainPort<'_>, SortedForeignClockRecordsVec> {
            let grandmaster_id = *self.local_clock.identity();
            PortProfile::default().master(
                self.domain_port(),
                GrandMasterTrackingBmca::new(
                    BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                    grandmaster_id,
                ),
            )
        }

        fn pre_master_port(
            &self,
        ) -> PortState<PortStateTestDomainPort<'_>, SortedForeignClockRecordsVec> {
            let grandmaster_id = *self.local_clock.identity();
            PortProfile::default().pre_master(
                self.domain_port(),
                GrandMasterTrackingBmca::new(
                    BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                    grandmaster_id,
                ),
                QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
            )
        }

        fn uncalibrated_port(
            &self,
        ) -> PortState<PortStateTestDomainPort<'_>, SortedForeignClockRecordsVec> {
            PortProfile::default().uncalibrated(
                self.domain_port(),
                ParentTrackingBmca::new(
                    BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                    ParentPortIdentity::new(PortIdentity::fake()),
                ),
            )
        }
    }

    #[test]
    fn portstate_listening_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let listening = setup.listening_port();

        let master = listening.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_slave_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let slave = setup.slave_port();

        let master = slave.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_pre_master_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let pre_master = setup.pre_master_port();

        let master = pre_master.apply(StateDecision::QualificationTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let uncalibrated = setup.uncalibrated_port();

        let master = uncalibrated.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_slave_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let uncalibrated = setup.uncalibrated_port();

        let slave = uncalibrated.apply(StateDecision::MasterClockSelected);

        assert!(matches!(slave, PortState::Slave(_)));
    }

    #[test]
    fn portstate_listening_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let listening = setup.listening_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let uncalibrated = listening.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_master_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let master = setup.master_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let uncalibrated = master.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_listening_to_pre_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let listening = setup.listening_port();

        let pre_master =
            listening.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0),
                *setup.local_clock_identity(),
            )));

        assert!(matches!(pre_master, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_initializing_to_listening_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let initializing = setup.initializing_port();

        let listening = initializing.apply(StateDecision::Initialized);

        assert!(matches!(listening, PortState::Listening(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_master_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let initializing = setup.initializing_port();

        initializing.apply(StateDecision::AnnounceReceiptTimeoutExpired);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_master_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let master = setup.master_port();

        master.apply(StateDecision::AnnounceReceiptTimeoutExpired);
    }

    // Tests for illegal ToSlave transitions

    #[test]
    #[should_panic]
    fn portstate_initializing_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let initializing = setup.initializing_port();

        initializing.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_listening_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let listening = setup.listening_port();

        listening.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_slave_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let slave = setup.slave_port();

        slave.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let master = setup.master_port();

        master.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_pre_master_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let pre_master = setup.pre_master_port();

        pre_master.apply(StateDecision::MasterClockSelected);
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_uncalibrated_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let initializing = setup.initializing_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        initializing.apply(StateDecision::RecommendedSlave(parent_port_identity));
    }

    #[test]
    fn portstate_slave_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let slave = setup.slave_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = slave.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_pre_master_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let pre_master = setup.pre_master_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = pre_master.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let uncalibrated = setup.uncalibrated_port();

        let result = uncalibrated.apply(StateDecision::RecommendedSlave(ParentPortIdentity::new(
            PortIdentity::fake(),
        )));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_pre_master_illegal_transition_goes_to_faulty() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let initializing = setup.initializing_port();

        let result = initializing.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
            *setup.local_clock_identity(),
        )));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_pre_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let slave = setup.slave_port();

        let result = slave.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
            *setup.local_clock_identity(),
        )));

        assert!(matches!(result, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_master_to_pre_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let master = setup.master_port();

        let result = master.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
            *setup.local_clock_identity(),
        )));

        assert!(matches!(result, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_pre_master_to_pre_master_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let pre_master = setup.pre_master_port();

        let result = pre_master.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
            *setup.local_clock_identity(),
        )));

        assert!(matches!(result, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_pre_master_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let uncalibrated = setup.uncalibrated_port();

        let result = uncalibrated.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
            *setup.local_clock_identity(),
        )));

        assert!(matches!(result, PortState::PreMaster(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_listening_to_listening_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let listening = setup.listening_port();

        listening.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_slave_to_listening_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let slave = setup.slave_port();

        slave.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_listening_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let master = setup.master_port();

        master.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_pre_master_to_listening_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let pre_master = setup.pre_master_port();

        pre_master.apply(StateDecision::Initialized);
    }

    // Tests for explicit ToFaulty transitions from every state

    #[test]
    fn portstate_initializing_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let initializing = setup.initializing_port();

        let result = initializing.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_listening_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let listening = setup.listening_port();

        let result = listening.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let slave = setup.slave_port();

        let result = slave.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let master = setup.master_port();

        let result = master.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_announce_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            TestClockCatalog::default_high_grade().default_ds(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let mut master = PortProfile::default().master(
            DomainPort::new(
                &local_clock,
                &FailingPort, // <-- Failing port to simulate send failure <--
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            GrandMasterTrackingBmca::new(
                BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                *local_clock.identity(),
            ),
        );

        let transition = master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_delay_response_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            TestClockCatalog::default_high_grade().default_ds(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let mut master = PortProfile::default().master(
            DomainPort::new(
                &local_clock,
                &FailingPort,
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            GrandMasterTrackingBmca::new(
                BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                *local_clock.identity(),
            ),
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
            TestClockCatalog::default_high_grade().default_ds(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let mut master = PortProfile::default().master(
            DomainPort::new(
                &local_clock,
                &FailingPort, // <-- failing port to simulate send failure <--
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            GrandMasterTrackingBmca::new(
                BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                *local_clock.identity(),
            ),
        );

        let sync_msg = TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0));
        let ts_msg =
            TimestampMessage::new(EventMessage::TwoStepSync(sync_msg), TimeStamp::new(0, 0));

        let transition = master.dispatch_system(SystemMessage::Timestamp(ts_msg));

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_sync_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            TestClockCatalog::default_high_grade().default_ds(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let mut master = PortProfile::default().master(
            DomainPort::new(
                &local_clock,
                &FailingPort,
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            GrandMasterTrackingBmca::new(
                BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
                *local_clock.identity(),
            ),
        );

        let transition = master.dispatch_system(SystemMessage::SyncTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_slave_enters_faulty_on_delay_request_send_failure() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            TestClockCatalog::default_mid_grade().default_ds(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let domain_port = DomainPort::new(
            &local_clock,
            &FailingPort,
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            NoopPortLog,
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let bmca = ParentTrackingBmca::new(
            BestForeignRecord::new(SortedForeignClockRecordsVec::new()),
            parent_port_identity,
        );

        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            domain_port.timeout(SystemMessage::AnnounceReceiptTimeout),
            Duration::from_secs(10),
        );
        let delay_timeout = domain_port.timeout(SystemMessage::DelayRequestTimeout);
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout, LogInterval::new(0));

        let mut slave = PortState::Slave(SlavePort::new(
            domain_port,
            bmca,
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(delay_cycle),
            PortProfile::default(),
        ));

        let transition = slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_pre_master_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let pre_master = setup.pre_master_port();

        let result = pre_master.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let uncalibrated = setup.uncalibrated_port();

        let result = uncalibrated.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_faulty_to_faulty_transition() {
        let faulty: PortState<
            DomainPort<FakeClock, FakeTimerHost, FakeTimestamping, NoopPortLog>,
            SortedForeignClockRecordsVec,
        > = PortState::Faulty(FaultyPort::default());

        let result = faulty.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }
}
