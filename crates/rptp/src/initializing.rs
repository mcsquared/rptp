//! Port state: Initializing.
//!
//! This is the entry state of the port state machine.
//!
//! In IEEE 1588 terms, this corresponds to the `INITIALIZING` state in the port state chart
//! (IEEE 1588-2019 §9.2.5). In `rptp`, the state has one main responsibility:
//!
//! - wait for the infrastructure layer to signal that the port is ready by delivering
//!   [`crate::message::SystemMessage::Initialized`],
//! - then transition into [`crate::listening::ListeningPort`] with an appropriately wrapped BMCA
//!   role ([`crate::bmca::ListeningBmca`]).
//!
//! The initialization "signal" is intentionally modeled as a system message (rather than hidden
//! constructor side effects) to keep startup sequencing explicit at the boundary: infrastructure
//! decides when sockets/timers/clocks are ready, and the domain state machine decides what that
//! means for behaviour.

use crate::bmca::{
    BestForeignRecord, BestMasterClockAlgorithm, ForeignClockRecords, ListeningBmca,
    StateDecisionEventTrigger,
};
use crate::log::PortEvent;
use crate::port::Port;
use crate::portstate::PortState;
use crate::profile::PortProfile;

/// Port role for the `INITIALIZING` state.
///
/// This state is intentionally small: it mainly exists to model the explicit transition from
/// "constructed, but not yet started" into the steady-state message-processing world.
///
/// ## Owned collaborators
///
/// - A `Port` implementation (I/O + timers + logging boundary).
/// - The core BMCA evaluator ([`BestMasterClockAlgorithm`]) and per-port best-foreign record store
///   ([`BestForeignRecord`]).
/// - A [`StateDecisionEventTrigger`] for change detection and event triggering.
/// - A [`PortProfile`] that knows how to assemble the next state with the right timing policies.
///
/// On transition to `Listening`, the BMCA pieces are wrapped into [`ListeningBmca`] to encode the
/// first decision gating rule ("only decide once a qualified foreign dataset exists").
pub struct InitializingPort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
    state_decision_trigger: StateDecisionEventTrigger<'a>,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> InitializingPort<'a, P, S> {
    /// Create a new `InitializingPort` role.
    ///
    /// This is typically constructed by [`PortProfile::initializing`] as part of wiring an
    /// ordinary clock/port. The returned value is instantly ready-to-use: it is fully wired and
    /// ready to transition as soon as infrastructure reports that initialization has completed.
    pub(crate) fn new(
        port: P,
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        state_decision_trigger: StateDecisionEventTrigger<'a>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become InitializingPort"));

        Self {
            port,
            bmca,
            best_foreign,
            state_decision_trigger,
            profile,
        }
    }

    /// Transition to `Listening` after receiving `SystemMessage::Initialized`.
    ///
    /// This applies the IEEE 1588 "INITIALIZING → LISTENING" transition by switching to the
    /// listening BMCA role and letting the [`PortProfile`] assemble the resulting
    /// [`crate::listening::ListeningPort`].
    pub(crate) fn initialized(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::Initialized);

        let bmca = ListeningBmca::new(self.bmca, self.best_foreign, self.state_decision_trigger);

        self.profile.listening(self.port, bmca)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::clock::LocalClock;
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeStateDecisionEvent, FakeTimerHost, FakeTimestamping, TestClockDS,
    };

    #[test]
    fn initializing_port_to_listening_transition() {
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let physical_port = FakePort::new();
        let state_decision_event = FakeStateDecisionEvent::new();
        let port_number = PortNumber::new(1);
        let best_foreign = BestForeignRecord::new(port_number, ForeignClockRecordsVec::new());
        let state_decision_trigger = StateDecisionEventTrigger::new(
            &state_decision_event,
            best_foreign.snapshot(),
            port_number,
        );
        let initializing = InitializingPort::new(
            DomainPort::new(
                &local_clock,
                &physical_port,
                FakeTimerHost::new(),
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                port_number,
            ),
            BestMasterClockAlgorithm::new(&default_ds, port_number),
            best_foreign,
            state_decision_trigger,
            PortProfile::default(),
        );

        let listening = initializing.initialized();

        assert!(matches!(listening, PortState::Listening(_)));
    }
}
