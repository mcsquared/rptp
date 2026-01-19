//! Port state machine glue.
//!
//! This module defines the `rptp` port state machine as:
//! - a sum type [`PortState`] of behaviour-rich state implementations, and
//! - a small set of state transition inputs ([`StateDecision`]).
//!
//! Port states are modeled as distinct types (`InitializingPort`, `ListeningPort`, `MasterPort`,
//! `SlavePort`, …). Each state type owns its state-specific collaborators (timeouts, BMCA wrappers,
//! delay mechanism, etc.) and exposes message handlers.
//!
//! [`PortState`] provides the “routing layer”:
//! - `dispatch_*` selects the correct state-specific handler for an incoming message, and
//! - `apply` applies a [`StateDecision`] by performing an explicit state transition.
//!
//! ## Ingress integration
//!
//! `MessageIngress` parses datagrams into typed messages and dispatches them into a `PortMap`.
//! `PortIngress for Option<PortState<...>>` (see `crate::port`) then uses these methods to drive
//! the state machine: `dispatch_*` → `StateDecision` → `apply`.
//!
//! ## Illegal transitions
//!
//! Some state transitions are illegal by construction. `apply` will `panic!` if asked to apply a
//! decision to a state where it cannot occur (these are internal invariants guarded by tests).
//!
//! ## Fail-fast note
//!
//! At the moment, `rptp` follows a fail-fast approach for internal invariants: if the state
//! machine is driven in a way that violates its transition contract, we prefer to crash loudly
//! during early-stage development rather than silently recover into an faulty state.
//!
//! This is a deliberate choice and not carved in stone. As the integration boundaries mature,
//! these panics may be replaced by structured error handling. For broader context, see
//! `docs/architecture-overview.md`.

use core::panic;

use crate::bmca::{BmcaMasterDecision, ForeignClockRecords};
use crate::faulty::FaultyPort;
use crate::initializing::InitializingPort;
use crate::listening::ListeningPort;
use crate::master::MasterPort;
use crate::message::{EventMessage, GeneralMessage, SystemMessage};
use crate::passive::PassivePort;
use crate::port::{ParentPortIdentity, Port, PortIdentity};
use crate::premaster::PreMasterPort;
use crate::slave::SlavePort;
use crate::time::{Instant, TimeStamp};
use crate::uncalibrated::UncalibratedPort;

/// Transition inputs for the port state machine.
///
/// These values are produced by state-specific message handlers (`process_*`) and by
/// `dispatch_system` when it interprets [`SystemMessage`]s.
///
/// The decision set follows the IEEE 1588 state chart terminology closely, but it is intentionally
/// expressed in terms of domain meaning (e.g. “recommended slave of this parent”).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StateDecision {
    /// Infrastructure signaled that initialization completed (`INITIALIZING → LISTENING`).
    Initialized,
    /// The servo reported that the master clock has been selected (used for `UNCALIBRATED → SLAVE`).
    MasterClockSelected(ParentPortIdentity),
    /// BMCA recommended transitioning toward slave with the selected parent.
    RecommendedSlave(ParentPortIdentity),
    /// BMCA recommended transitioning toward master/pre-master.
    RecommendedMaster(BmcaMasterDecision),
    /// BMCA recommended transitioning toward passive.
    RecommendedPassive,
    /// A fault was detected (send failure, explicit fault condition, …).
    FaultDetected,
    /// A previously detected fault has been cleared (`FAULTY → INITIALIZING`).
    FaultCleared,
    /// Qualification timeout expired (`PRE_MASTER → MASTER`).
    QualificationTimeoutExpired,
    /// Announce receipt timeout expired (used to leave listening/slave/uncalibrated).
    AnnounceReceiptTimeoutExpired,
    /// Servo reported a synchronization fault while in slave mode (`SLAVE → UNCALIBRATED`).
    SynchronizationFault,
}

/// Port state machine states (IEEE 1588-2019 §9.2.5).
///
/// Each variant wraps a concrete state type that owns its collaborators and implements
/// state-specific message handling.
#[allow(clippy::large_enum_variant)]
pub enum PortState<'a, P: Port, S: ForeignClockRecords> {
    Initializing(InitializingPort<'a, P, S>),
    Listening(ListeningPort<'a, P, S>),
    Slave(SlavePort<'a, P, S>),
    Master(MasterPort<'a, P, S>),
    PreMaster(PreMasterPort<'a, P, S>),
    Uncalibrated(UncalibratedPort<'a, P, S>),
    Passive(PassivePort<'a, P, S>),
    Faulty(FaultyPort<'a, P, S>),
}

impl<'a, P: Port, S: ForeignClockRecords> PortState<'a, P, S> {
    /// Apply a transition decision, producing the next port state.
    ///
    /// This is the only place where cross-state transition rules are centralized. Most decisions
    /// are only legal in a subset of states; applying an illegal decision will `panic!`.
    pub(crate) fn apply(self, decision: StateDecision) -> Self {
        match decision {
            StateDecision::AnnounceReceiptTimeoutExpired => match self {
                PortState::Listening(listening) => listening.announce_receipt_timeout_expired(),
                PortState::Slave(slave) => slave.announce_receipt_timeout_expired(),
                PortState::Uncalibrated(uncalibrated) => {
                    uncalibrated.announce_receipt_timeout_expired()
                }
                PortState::Passive(passive) => passive.announce_receipt_timeout_expired(),
                _ => panic!(
                    "AnnounceReceiptTimeoutExpired can only be applied in Listening, Slave, Uncalibrated, or Passive states"
                ),
            },
            StateDecision::MasterClockSelected(parent) => match self {
                PortState::Uncalibrated(uncalibrated) => uncalibrated.master_clock_selected(parent),
                _ => panic!("MasterClockSelected can only be applied in Uncalibrated state"),
            },
            StateDecision::RecommendedSlave(decision) => match self {
                PortState::Listening(listening) => listening.recommended_slave(decision),
                PortState::Master(master) => master.recommended_slave(decision),
                PortState::PreMaster(pre_master) => pre_master.recommended_slave(decision),
                PortState::Slave(slave) => slave.recommended_slave(decision),
                PortState::Uncalibrated(uncalibrated) => uncalibrated.recommended_slave(decision),
                PortState::Passive(passive) => passive.recommended_slave(decision),
                _ => panic!(
                    "RecommendedSlave can only be applied in Listening, Master, PreMaster, Slave, Uncalibrated, or Passive states"
                ),
            },
            StateDecision::RecommendedMaster(decision) => match self {
                PortState::Listening(listening) => listening.recommended_master(decision),
                PortState::Uncalibrated(uncalibrated) => uncalibrated.recommended_master(decision),
                PortState::Master(master) => master.recommended_master(decision),
                PortState::Slave(slave) => slave.recommended_master(decision),
                PortState::PreMaster(pre_master) => pre_master.recommended_master(decision),
                PortState::Passive(passive) => passive.recommended_master(decision),
                _ => panic!(
                    "RecommendedMaster can only be applied in Listening, Uncalibrated, Master, PreMaster, Slave, or Passive states"
                ),
            },
            StateDecision::RecommendedPassive => match self {
                PortState::Listening(listening) => listening.recommended_passive(),
                PortState::Master(master) => master.recommended_passive(),
                PortState::PreMaster(pre_master) => pre_master.recommended_passive(),
                PortState::Slave(slave) => slave.recommended_passive(),
                PortState::Uncalibrated(uncalibrated) => uncalibrated.recommended_passive(),
                _ => panic!(
                    "RecommendedPassive can only be applied in Listening, Master, PreMaster, Slave, or Uncalibrated states"
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
            StateDecision::FaultDetected => match self {
                PortState::Listening(listening) => listening.fault_detected(),
                PortState::Slave(slave) => slave.fault_detected(),
                PortState::Master(master) => master.fault_detected(),
                PortState::PreMaster(pre_master) => pre_master.fault_detected(),
                PortState::Uncalibrated(uncalibrated) => uncalibrated.fault_detected(),
                PortState::Passive(passive) => passive.fault_detected(),
                PortState::Faulty(faulty) => PortState::Faulty(faulty),
                PortState::Initializing(_) => {
                    panic!("FaultDetected cannot be applied in Initializing state")
                }
            },
            StateDecision::FaultCleared => match self {
                PortState::Faulty(faulty) => faulty.fault_cleared(),
                _ => panic!("FaultCleared can only be applied in Faulty state"),
            },
        }
    }

    /// Dispatch a parsed event message (timestamped message class) to the current state.
    ///
    /// `ingress_timestamp` is the receiver-observed event timestamp provided by infrastructure.
    /// Returning `Some(StateDecision)` indicates that the state machine should apply a transition.
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
            (Passive(_), _) => None,
            _ => None,
        }
    }

    /// Dispatch a parsed general message (non-timestamped message class) to the current state.
    ///
    /// `now` is a local monotonic time instant used by the domain for time-window logic (e.g.
    /// foreign master qualification / staleness).
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
            (Passive(port), Announce(msg)) => port.process_announce(msg, source_port_identity, now),
            _ => None,
        }
    }

    /// Dispatch a system/internal message (timeouts, timestamp feedback, initialization signal).
    ///
    /// These messages are not received from the network. They are delivered by infrastructure and
    /// interpreted here as either:
    /// - an instruction to perform an action (e.g. send Announce/Sync/DelayReq), or
    /// - a transition decision (e.g. `AnnounceReceiptTimeoutExpired`, `QualificationTimeoutExpired`).
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
            (
                Listening(_) | Slave(_) | Master(_) | Uncalibrated(_) | Passive(_),
                AnnounceReceiptTimeout,
            ) => Some(StateDecision::AnnounceReceiptTimeoutExpired),
            (PreMaster(_), QualificationTimeout) => {
                Some(StateDecision::QualificationTimeoutExpired)
            }
            (Faulty(_), FaultCleared) => Some(StateDecision::FaultCleared),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::cell::Cell;

    use crate::bmca::{
        BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, BmcaMasterDecision,
        BmcaMasterDecisionPoint, ClockDS, GrandMasterTrackingBmca, ListeningBmca,
        ParentTrackingBmca, PassiveBmca, QualificationTimeoutPolicy,
    };
    use crate::clock::{ClockIdentity, LocalClock, StepsRemoved};
    use crate::e2e::{DelayCycle, EndToEndDelayMechanism};
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{DelayRequestMessage, TimestampMessage, TwoStepSyncMessage};
    use crate::port::{
        AnnounceReceiptTimeout, DomainNumber, DomainPort, ParentPortIdentity, PortNumber,
    };
    use crate::profile::PortProfile;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FailingPort, FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping, TestClockDS,
    };
    use crate::time::{Duration, LogInterval, LogMessageInterval, TimeStamp};

    type PortStateTestDomainPort<'a> =
        DomainPort<'a, FakeClock, FakeTimerHost, FakeTimestamping, NoopPortLog>;

    struct PortStateTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        foreign_candidates: Cell<BestForeignSnapshot>,
    }

    impl PortStateTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    *ds.identity(),
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                default_ds: ds,
                physical_port: FakePort::new(),
                foreign_candidates: Cell::new(BestForeignSnapshot::Empty),
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
        ) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            PortProfile::default().initializing(
                self.domain_port(),
                BestMasterClockAlgorithm::new(
                    &self.default_ds,
                    &self.foreign_candidates,
                    PortNumber::new(1),
                ),
                BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
            )
        }

        fn listening_port(
            &self,
        ) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            PortProfile::default().listening(
                self.domain_port(),
                ListeningBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                ),
            )
        }

        fn slave_port(&self) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            PortProfile::default().slave(
                self.domain_port(),
                ParentTrackingBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
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
        ) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            let grandmaster_id = *self.local_clock.identity();
            PortProfile::default().master(
                self.domain_port(),
                GrandMasterTrackingBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                    grandmaster_id,
                ),
            )
        }

        fn pre_master_port(
            &self,
        ) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            let grandmaster_id = *self.local_clock.identity();
            PortProfile::default().pre_master(
                self.domain_port(),
                GrandMasterTrackingBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                    grandmaster_id,
                ),
                QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(0)),
            )
        }

        fn uncalibrated_port(
            &self,
        ) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            PortProfile::default().uncalibrated(
                self.domain_port(),
                ParentTrackingBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                    ParentPortIdentity::new(PortIdentity::fake()),
                ),
            )
        }

        fn faulty_port(
            &self,
        ) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            PortProfile::default().faulty(
                self.domain_port(),
                BestMasterClockAlgorithm::new(
                    &self.default_ds,
                    &self.foreign_candidates,
                    PortNumber::new(1),
                ),
                BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
            )
        }

        fn passive_port(
            &self,
        ) -> PortState<'_, PortStateTestDomainPort<'_>, ForeignClockRecordsVec> {
            PortProfile::default().passive(
                self.domain_port(),
                PassiveBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                ),
            )
        }
    }

    #[test]
    fn portstate_listening_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let listening = setup.listening_port();

        let master = listening.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_slave_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let slave = setup.slave_port();

        let master = slave.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_pre_master_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.pre_master_port();

        let master = pre_master.apply(StateDecision::QualificationTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let uncalibrated = setup.uncalibrated_port();

        let master = uncalibrated.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_slave_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let uncalibrated = setup.uncalibrated_port();

        let slave = uncalibrated.apply(StateDecision::MasterClockSelected(
            ParentPortIdentity::new(PortIdentity::fake()),
        ));

        assert!(matches!(slave, PortState::Slave(_)));
    }

    #[test]
    fn portstate_listening_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let listening = setup.listening_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let uncalibrated = listening.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_master_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let master = setup.master_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let uncalibrated = master.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_listening_to_pre_master_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

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
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let initializing = setup.initializing_port();

        let listening = initializing.apply(StateDecision::Initialized);

        assert!(matches!(listening, PortState::Listening(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_master_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let initializing = setup.initializing_port();

        initializing.apply(StateDecision::AnnounceReceiptTimeoutExpired);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_master_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let master = setup.master_port();

        master.apply(StateDecision::AnnounceReceiptTimeoutExpired);
    }

    // Tests for illegal ToSlave transitions

    #[test]
    #[should_panic]
    fn portstate_initializing_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let initializing = setup.initializing_port();

        initializing.apply(StateDecision::MasterClockSelected(ParentPortIdentity::new(
            PortIdentity::fake(),
        )));
    }

    #[test]
    #[should_panic]
    fn portstate_listening_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let listening = setup.listening_port();

        listening.apply(StateDecision::MasterClockSelected(ParentPortIdentity::new(
            PortIdentity::fake(),
        )));
    }

    #[test]
    #[should_panic]
    fn portstate_slave_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let slave = setup.slave_port();

        slave.apply(StateDecision::MasterClockSelected(ParentPortIdentity::new(
            PortIdentity::fake(),
        )));
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let master = setup.master_port();

        master.apply(StateDecision::MasterClockSelected(ParentPortIdentity::new(
            PortIdentity::fake(),
        )));
    }

    #[test]
    #[should_panic]
    fn portstate_pre_master_to_slave_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.pre_master_port();

        pre_master.apply(StateDecision::MasterClockSelected(ParentPortIdentity::new(
            PortIdentity::fake(),
        )));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_uncalibrated_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let initializing = setup.initializing_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        initializing.apply(StateDecision::RecommendedSlave(parent_port_identity));
    }

    #[test]
    fn portstate_slave_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let slave = setup.slave_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = slave.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_pre_master_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.pre_master_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let result = pre_master.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let uncalibrated = setup.uncalibrated_port();

        let result = uncalibrated.apply(StateDecision::RecommendedSlave(ParentPortIdentity::new(
            PortIdentity::fake(),
        )));

        assert!(matches!(result, PortState::Uncalibrated(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_pre_master_illegal_transition_goes_to_faulty() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

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
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

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
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

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
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

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
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

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
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let listening = setup.listening_port();

        listening.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_slave_to_listening_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let slave = setup.slave_port();

        slave.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_master_to_listening_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let master = setup.master_port();

        master.apply(StateDecision::Initialized);
    }

    #[test]
    #[should_panic]
    fn portstate_pre_master_to_listening_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.pre_master_port();

        pre_master.apply(StateDecision::Initialized);
    }

    // Tests for explicit ToFaulty transitions from every state

    #[test]
    #[should_panic]
    fn portstate_initializing_to_faulty_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let initializing = setup.initializing_port();

        let _ = initializing.apply(StateDecision::FaultDetected);
    }

    #[test]
    fn portstate_listening_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let listening = setup.listening_port();

        let result = listening.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let slave = setup.slave_port();

        let result = slave.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let master = setup.master_port();

        let result = master.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_announce_send_failure() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_high_grade().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
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
                BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
                BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                *local_clock.identity(),
            ),
        );

        let transition = master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_master_enters_faulty_on_delay_response_send_failure() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_high_grade().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
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
                BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
                BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
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
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_high_grade().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
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
                BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
                BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
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
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_high_grade().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
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
                BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
                BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                *local_clock.identity(),
            ),
        );

        let transition = master.dispatch_system(SystemMessage::SyncTimeout);

        assert!(matches!(transition, Some(StateDecision::FaultDetected)));
    }

    #[test]
    fn portstate_slave_enters_faulty_on_delay_request_send_failure() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
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
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
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
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.pre_master_port();

        let result = pre_master.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let uncalibrated = setup.uncalibrated_port();

        let result = uncalibrated.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_faulty_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let faulty = setup.faulty_port();

        let result = faulty.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_faulty_to_initializing_on_fault_cleared() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let faulty = setup.faulty_port();

        let result = faulty.apply(StateDecision::FaultCleared);

        assert!(matches!(result, PortState::Initializing(_)));
    }

    #[test]
    fn portstate_faulty_dispatch_system_fault_cleared_requests_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let mut faulty = setup.faulty_port();

        let decision = faulty.dispatch_system(SystemMessage::FaultCleared);

        assert!(matches!(decision, Some(StateDecision::FaultCleared)));
    }

    #[test]
    fn portstate_listening_to_passive_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let listening = setup.listening_port();

        let passive = listening.apply(StateDecision::RecommendedPassive);

        assert!(matches!(passive, PortState::Passive(_)));
    }

    #[test]
    fn portstate_master_to_passive_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let master = setup.master_port();

        let passive = master.apply(StateDecision::RecommendedPassive);

        assert!(matches!(passive, PortState::Passive(_)));
    }

    #[test]
    fn portstate_pre_master_to_passive_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.pre_master_port();

        let passive = pre_master.apply(StateDecision::RecommendedPassive);

        assert!(matches!(passive, PortState::Passive(_)));
    }

    #[test]
    fn portstate_slave_to_passive_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let slave = setup.slave_port();

        let passive = slave.apply(StateDecision::RecommendedPassive);

        assert!(matches!(passive, PortState::Passive(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_passive_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let uncalibrated = setup.uncalibrated_port();

        let passive = uncalibrated.apply(StateDecision::RecommendedPassive);

        assert!(matches!(passive, PortState::Passive(_)));
    }

    #[test]
    fn portstate_passive_to_pre_master_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let passive = setup.passive_port();

        let pre_master = passive.apply(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
            BmcaMasterDecisionPoint::M1,
            StepsRemoved::new(0),
            *setup.local_clock_identity(),
        )));

        assert!(matches!(pre_master, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_passive_to_uncalibrated_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let passive = setup.passive_port();

        let parent_port_identity = ParentPortIdentity::new(PortIdentity::fake());
        let uncalibrated = passive.apply(StateDecision::RecommendedSlave(parent_port_identity));

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_passive_to_master_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_high_grade().dataset());

        let passive = setup.passive_port();

        let master = passive.apply(StateDecision::AnnounceReceiptTimeoutExpired);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_passive_to_faulty_transition() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let passive = setup.passive_port();

        let result = passive.apply(StateDecision::FaultDetected);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    #[should_panic]
    fn portstate_initializing_to_passive_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let initializing = setup.initializing_port();

        initializing.apply(StateDecision::RecommendedPassive);
    }

    #[test]
    #[should_panic]
    fn portstate_faulty_to_passive_illegal_transition_panics() {
        let setup = PortStateTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let faulty = setup.faulty_port();

        faulty.apply(StateDecision::RecommendedPassive);
    }
}
