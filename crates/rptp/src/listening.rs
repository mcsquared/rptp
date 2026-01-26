//! Port state: Listening.
//!
//! This module implements the `LISTENING` state of the IEEE 1588 port state machine
//! (IEEE 1588-2019 §9.2.5).
//!
//! In `rptp`, a port in `Listening`:
//! - receives and evaluates Announce messages,
//! - maintains the Announce receipt timeout, and
//! - drives BMCA via [`ListeningBmca`] until it can recommend a transition.
//!
//! ## Transitions
//!
//! - On [`StateDecision::RecommendedSlave`], transition to `UNCALIBRATED` with parent tracking
//!   enabled (see [`ListeningPort::recommended_slave`]).
//! - On [`StateDecision::RecommendedMaster`], transition to `PRE_MASTER` with grandmaster tracking
//!   enabled and a qualification timeout (see [`ListeningPort::recommended_master`]).
//! - On Announce receipt timeout expiry, transition to `MASTER` (see
//!   [`ListeningPort::announce_receipt_timeout_expired`]).
//!
//! Announce reception is the only message-processing responsibility of this state; event message
//! processing (Sync/DelayReq/…) is handled in other states.

use crate::bmca::{
    BestForeignSnapshot, Bmca, BmcaMasterDecision, ForeignClockRecords, ListeningBmca,
};
use crate::log::PortEvent;
use crate::message::AnnounceMessage;
use crate::port::{AnnounceReceiptTimeout, ParentPortIdentity, Port, PortIdentity};
use crate::portstate::{PortState, StateDecision};
use crate::profile::PortProfile;
use crate::time::Instant;

/// Port role for the `LISTENING` state.
///
/// The state is entered from `INITIALIZING` once infrastructure signals readiness and is typically
/// constructed by [`PortProfile::listening`].
///
/// This type owns:
/// - the `Port` boundary (send, timers, logging),
/// - a [`ListeningBmca`] instance for Announce-driven recommendation, and
/// - an [`AnnounceReceiptTimeout`] that is restarted on each received Announce.
pub struct ListeningPort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: ListeningBmca<'a, S>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> ListeningPort<'a, P, S> {
    /// Create a new `ListeningPort`.
    ///
    /// `announce_receipt_timeout` is expected to be configured with the profile’s
    /// AnnounceReceiptTimeout interval and typically started by the profile before entering this
    /// state.
    pub(crate) fn new(
        port: P,
        bmca: ListeningBmca<'a, S>,
        announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become ListeningPort"));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            profile,
        }
    }

    /// Apply a BMCA recommendation to become a slave of `parent`.
    ///
    /// This transitions into `UNCALIBRATED` and switches the BMCA wrapper to parent tracking so
    /// that subsequent decisions and message acceptance can be gated by the selected parent.
    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let parent_tracking_bmca = self.bmca.into_parent_tracking(parent);

        self.profile.uncalibrated(self.port, parent_tracking_bmca)
    }

    /// Apply a BMCA recommendation to become (pre-)master.
    ///
    /// This transitions into `PRE_MASTER` and switches the BMCA wrapper to grandmaster tracking.
    /// The `BmcaMasterDecision` carries the qualification timeout policy that determines how long
    /// the port must remain in `PRE_MASTER` before becoming `MASTER`.
    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedMaster);

        decision.apply(|qualification_timeout_policy, grandmaster_id| {
            let bmca = self.bmca.into_grandmaster_tracking(grandmaster_id);

            self.profile
                .pre_master(self.port, bmca, qualification_timeout_policy)
        })
    }

    /// Apply a BMCA recommendation to become passive.
    ///
    /// This transitions into `PASSIVE` while preserving the BMCA state.
    pub(crate) fn recommended_passive(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::Static("RecommendedPassive"));
        let bmca = self.bmca.into_passive();
        self.profile.passive(self.port, bmca)
    }

    /// Handle Announce receipt timeout expiry while in `LISTENING`.
    ///
    /// This transitions to `MASTER` using “current grandmaster tracking”, which (in current
    /// single-port setups) resolves to the local grandmaster identity.
    pub(crate) fn announce_receipt_timeout_expired(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::AnnounceReceiptTimeout);
        self.bmca.trigger_state_decision_event();
        let bmca = self.bmca.into_current_grandmaster_tracking();
        self.profile.master(self.port, bmca)
    }

    /// Process an incoming Announce message.
    ///
    /// This:
    /// - restarts the Announce receipt timeout,
    /// - feeds the message into BMCA (which triggers state decision event if e_rbest changed).
    pub(crate) fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) {
        self.port.log(PortEvent::MessageReceived("Announce"));
        self.announce_receipt_timeout.restart();

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);
    }

    /// Transition to `FAULTY` upon fault detection.
    pub(crate) fn fault_detected(self) -> PortState<'a, P, S> {
        let (bmca, best_foreign, sde) = self.bmca.into_parts();
        self.profile.faulty(self.port, bmca, best_foreign, sde)
    }

    /// Process a state decision event.
    pub(crate) fn state_decision_event(
        &self,
        best_master_clock: &BestForeignSnapshot,
    ) -> Option<StateDecision> {
        match self.bmca.decision(best_master_clock) {
            Some(decision) => decision.to_state_decision(),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{
        BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, ClockDS, ListeningBmca,
    };
    use crate::clock::{LocalClock, TimeScale};
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeStateDecisionEvent, FakeTimerHost, FakeTimestamping, TestClockDS,
    };
    use crate::time::{Duration, Instant, LogMessageInterval};

    type ListeningTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type ListeningTestPort<'a> =
        ListeningPort<'a, ListeningTestDomainPort<'a>, ForeignClockRecordsVec>;

    struct ListeningPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        state_decision_event: FakeStateDecisionEvent,
    }

    impl ListeningPortTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::default(),
                    *ds.identity(),
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                default_ds: ds,
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
                state_decision_event: FakeStateDecisionEvent::new(),
            }
        }

        fn port_under_test(&self, port_number: PortNumber) -> ListeningTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                port_number,
            );

            let announce_receipt_timeout = AnnounceReceiptTimeout::new(
                domain_port.timeout(SystemMessage::AnnounceReceiptTimeout),
                Duration::from_secs(5),
            );

            ListeningPort::new(
                domain_port,
                ListeningBmca::new(
                    BestMasterClockAlgorithm::new(&self.default_ds, port_number),
                    BestForeignRecord::new(port_number, ForeignClockRecordsVec::new()),
                    &self.state_decision_event,
                ),
                announce_receipt_timeout,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn listening_port_test_setup_is_side_effect_free() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let _listening = setup.port_under_test(PortNumber::new(1));

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
        assert!(setup.state_decision_event.take_events().is_empty());
    }

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let listening = setup.port_under_test(PortNumber::new(1));

        let master = listening.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn listening_port_triggers_state_decision_event_on_announce_receipt_timeout() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let listening = setup.port_under_test(PortNumber::new(1));

        listening.announce_receipt_timeout_expired();

        // Verify that a state decision event was triggered
        let events = setup.state_decision_event.take_events();
        assert_eq!(events.len(), 1, "Expected exactly one state decision event");
        assert_eq!(events[0].0, PortNumber::new(1));
        // Expect empty snapshot when running into timeout without any announces
        assert_eq!(events[0].1, BestForeignSnapshot::Empty);
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let mut listening = setup.port_under_test(PortNumber::new(1));

        let foreign_clock = TestClockDS::default_mid_grade().dataset();

        listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        // First announce does NOT trigger a state decision event because e_rbest remains
        // Empty (initial state is Empty, and no record should be qualified yet)
        let events = setup.state_decision_event.take_events();
        assert!(
            events.is_empty(),
            "No state decision event should be triggered (e_rbest stays Empty)"
        );

        // Verify that the announce receipt timeout was restarted as a side effect
        let system_messages = setup.timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_triggers_state_decision_event_on_two_announces() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let mut listening = setup.port_under_test(PortNumber::new(1));

        let foreign_clock = TestClockDS::default_mid_grade().dataset();
        let foreign_port = PortIdentity::fake();

        listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );

        // First announce doesn't trigger event (e_rbest stays Empty)
        assert!(setup.state_decision_event.take_events().is_empty());

        listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(1),
        );

        // Second announce qualifies the foreign master and triggers event
        // (e_rbest: Empty → Qualified)
        let events = setup.state_decision_event.take_events();
        assert_eq!(events.len(), 1);

        let (port_number, snapshot) = &events[0];
        assert_eq!(*port_number, PortNumber::new(1));

        // Verify the snapshot contains the qualified foreign master
        assert_eq!(
            *snapshot,
            BestForeignSnapshot::Qualified {
                ds: foreign_clock,
                source_port_identity: foreign_port,
                received_on_port: PortNumber::new(1),
            }
        );
    }
}
