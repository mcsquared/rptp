//! Port state: PreMaster.
//!
//! This module implements the `PRE_MASTER` state of the IEEE 1588 port state machine
//! (IEEE 1588-2019 §9.2.5).
//!
//! `PRE_MASTER` is an intermediate state on the path to `MASTER`. It exists to model the
//! spec-defined “qualification” delay: after BMCA recommends that the port should act as master,
//! the port waits for a (profile-defined) qualification timeout before entering `MASTER`.
//!
//! While waiting, the port continues to process Announce messages and runs BMCA. If a better
//! master is detected during that window, the port can still transition away (e.g. to
//! `UNCALIBRATED`).

use crate::bmca::{Bmca, BmcaMasterDecision, GrandMasterTrackingBmca, StateDecisionEvent};
use crate::foreign::ForeignClockRecords;
use crate::log::PortEvent;
use crate::message::{AnnounceMessage, StateDecisionSnapshot};
use crate::port::{ParentPortIdentity, Port, PortIdentity};
use crate::portstate::{PortState, StateDecision};
use crate::profile::PortProfile;
use crate::time::Instant;

/// Port role for the `PRE_MASTER` state.
///
/// This state is typically assembled by [`PortProfile::pre_master`] after a
/// [`StateDecision::RecommendedMaster`] transition from `LISTENING`, `MASTER`, `SLAVE`, or
/// `UNCALIBRATED`.
///
/// ## Qualification timeout
///
/// The `PRE_MASTER → MASTER` transition is driven by `SystemMessage::QualificationTimeout` as
/// handled by `PortState::dispatch_system`. `PortProfile::pre_master` configures and starts that
/// timeout when constructing this state.
///
/// The `_qualification_timeout` field is intentionally stored (even if not read directly) so the
/// underlying timeout handle is kept alive for the lifetime of the state.
pub struct PreMasterPort<P: Port, R: ForeignClockRecords, SDE: StateDecisionEvent> {
    port: P,
    bmca: GrandMasterTrackingBmca<R, SDE>,
    _qualification_timeout: P::Timeout,
    profile: PortProfile,
}

impl<P: Port, R: ForeignClockRecords, SDE: StateDecisionEvent> PreMasterPort<P, R, SDE> {
    /// Create a new `PreMasterPort`.
    ///
    /// `PortProfile::pre_master` is the usual constructor call site; it is responsible for
    /// restarting `_qualification_timeout` according to the active qualification policy.
    pub(crate) fn new(
        port: P,
        bmca: GrandMasterTrackingBmca<R, SDE>,
        _qualification_timeout: P::Timeout,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become PreMasterPort"));

        Self {
            port,
            bmca,
            _qualification_timeout,
            profile,
        }
    }

    /// Process an incoming Announce message while in `PRE_MASTER`.
    ///
    /// This feeds the Announce into BMCA (which triggers state decision event if e_rbest changed).
    pub(crate) fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) {
        self.port.log(PortEvent::MessageReceived("Announce"));

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);
    }

    /// Transition from `PRE_MASTER` to `MASTER` after the qualification timeout expires.
    pub(crate) fn qualified(self) -> PortState<P, R, SDE> {
        self.port.log(PortEvent::QualifiedMaster);
        self.profile.master(self.port, self.bmca)
    }

    /// Apply a BMCA recommendation to become (pre-)master.
    ///
    /// This remains in `PRE_MASTER`, but updates the tracked grandmaster dataset and (via the
    /// profile) restarts the qualification timeout according to the decision policy.
    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, R, SDE> {
        self.port.log(PortEvent::RecommendedMaster);

        decision.apply(|qualification_timeout_policy, grandmaster_ds| {
            let bmca = self.bmca.with_grandmaster_ds(grandmaster_ds);

            self.profile
                .pre_master(self.port, bmca, qualification_timeout_policy)
        })
    }

    /// Apply a BMCA recommendation to become a slave of `parent`.
    ///
    /// This transitions into `UNCALIBRATED` and switches the BMCA wrapper to parent tracking so
    /// that message acceptance and subsequent decisions can be gated by the selected parent.
    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<P, R, SDE> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let bmca = self.bmca.into_parent_tracking(parent);

        self.profile.uncalibrated(self.port, bmca)
    }

    /// Apply a BMCA recommendation to become passive.
    ///
    /// This transitions into `PASSIVE` while preserving the BMCA state.
    pub(crate) fn recommended_passive(self) -> PortState<P, R, SDE> {
        self.port.log(PortEvent::Static("RecommendedPassive"));
        let bmca = self.bmca.into_passive();
        self.profile.passive(self.port, bmca)
    }

    /// Transition to `FAULTY` upon fault detection.
    pub(crate) fn fault_detected(self) -> PortState<P, R, SDE> {
        let (bmca, best_foreign, state_decision_trigger) = self.bmca.into_parts();
        self.profile
            .faulty(self.port, bmca, best_foreign, state_decision_trigger)
    }

    /// Process a state decision event.
    pub(crate) fn state_decision_event(
        &mut self,
        snapshot: StateDecisionSnapshot,
    ) -> Option<StateDecision> {
        match self.bmca.decision(snapshot.d_0, &snapshot.e_best) {
            Some(decision) => decision.to_state_decision(),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{
        BestMasterClockAlgorithm, GrandMasterTrackingBmca, StateDecisionEventTrigger,
    };
    use crate::clock::{ClockIdentity, TimeScale};
    use crate::dataset::{ClockDS, Priority1};
    use crate::foreign::{BestForeignRecord, BestForeignSnapshot, ForeignClockRecord};
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::NoopPortLog;
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::test_support::{
        FakeClock, FakePort, FakeStateDecisionEvent, FakeTimerHost, FakeTimestamping, TestClockDS,
    };
    use crate::time::{Instant, LogInterval, LogMessageInterval};

    type PreMasterTestDomainPort<'a> =
        DomainPort<&'a FakeClock, &'a FakePort, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type PreMasterTestPort<'a> = PreMasterPort<
        PreMasterTestDomainPort<'a>,
        ForeignClockRecordsVec,
        &'a FakeStateDecisionEvent,
    >;

    struct PreMasterPortTestSetup {
        clock: FakeClock,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        state_decision_event: FakeStateDecisionEvent,
    }

    impl PreMasterPortTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self {
                clock: FakeClock::default(),
                default_ds: ds,
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
                state_decision_event: FakeStateDecisionEvent::new(),
            }
        }

        fn port_under_test(
            &self,
            port_number: PortNumber,
            records: &[ForeignClockRecord],
        ) -> PreMasterTestPort<'_> {
            let port_identity = PortIdentity::new(self.default_ds.identity(), port_number);
            let domain_port = DomainPort::new(
                &self.clock,
                port_identity,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
            );

            let qualification_timeout = domain_port.timeout(SystemMessage::QualificationTimeout);

            // Initialize initial state from the provided records
            let best_foreign_record = BestForeignRecord::new(
                port_identity,
                ForeignClockRecordsVec::from_records(records),
            );
            let state_decision_trigger = StateDecisionEventTrigger::new(
                &self.state_decision_event,
                best_foreign_record.snapshot(),
                port_number,
            );

            PreMasterPort::new(
                domain_port,
                GrandMasterTrackingBmca::new(
                    BestMasterClockAlgorithm::new(self.default_ds, port_number),
                    best_foreign_record,
                    self.default_ds,
                    state_decision_trigger,
                ),
                qualification_timeout,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn pre_master_port_test_setup_is_side_effect_free() {
        let setup = PreMasterPortTestSetup::new(TestClockDS::default_low_grade().dataset());

        let _pre_master = setup.port_under_test(PortNumber::new(1), &[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn pre_master_port_to_master_on_qualified() {
        let setup = PreMasterPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.port_under_test(PortNumber::new(1), &[]);

        let master = pre_master.qualified();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn premaster_port_triggers_state_decision_event_when_better_foreign_qualifies() {
        use crate::message::AnnounceMessage;
        use crate::port::{PortIdentity, PortNumber};

        let setup = PreMasterPortTestSetup::new(TestClockDS::default_low_grade().dataset());

        let foreign_clock = TestClockDS::default_high_grade().dataset();
        let foreign_port = PortIdentity::new(
            TestClockDS::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );
        let mut pre_master = setup.port_under_test(PortNumber::new(1), &[]);

        // Receive first better announce
        pre_master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );

        // No event on first announce (not qualified yet)
        assert!(
            setup.state_decision_event.take_events().is_empty(),
            "No event expected on first announce"
        );

        // Receive second better announce
        pre_master.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(1),
        );

        // State decision event should be triggered (Empty → Qualified)
        let events = setup.state_decision_event.take_events();
        let receiver = PortIdentity::new(setup.default_ds.identity(), PortNumber::new(1));
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        assert_eq!(
            events[0].1,
            BestForeignSnapshot::Qualified {
                ds: foreign_clock,
                source_port_identity: foreign_port,
                receiver,
            }
        );
    }

    #[test]
    fn premaster_port_stays_premaster_on_unchanged_foreign() {
        // Local GPS grandmaster, but foreign has lower priority1 making it better overall
        let setup = PreMasterPortTestSetup::new(TestClockDS::gps_grandmaster().dataset());

        // Start with a qualified foreign clock record
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        // Foreign uses lower priority1 so it is better, even though clock class is worse
        let foreign = TestClockDS::default_low_grade_slave_only().with_priority1(Priority1::new(1));
        let prior_records = [ForeignClockRecord::new_qualified(
            foreign_port,
            foreign.dataset(),
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut pre_master = setup.port_under_test(PortNumber::new(1), &prior_records);

        // Process another announce from the same foreign clock with same dataset
        pre_master.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign.dataset(),
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(1),
        );

        // No state decision event should be triggered because e_rbest doesn't change
        // (foreign master was already qualified with the same dataset)
        assert!(
            setup.state_decision_event.take_events().is_empty(),
            "No event expected when e_rbest unchanged"
        );
    }
}
