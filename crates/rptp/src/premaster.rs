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

use crate::bmca::{Bmca, BmcaMasterDecision, ForeignClockRecords, GrandMasterTrackingBmca};
use crate::log::PortEvent;
use crate::message::AnnounceMessage;
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
pub struct PreMasterPort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: GrandMasterTrackingBmca<'a, S>,
    _qualification_timeout: P::Timeout,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> PreMasterPort<'a, P, S> {
    /// Create a new `PreMasterPort`.
    ///
    /// `PortProfile::pre_master` is the usual constructor call site; it is responsible for
    /// restarting `_qualification_timeout` according to the active qualification policy.
    pub(crate) fn new(
        port: P,
        bmca: GrandMasterTrackingBmca<'a, S>,
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
    /// This feeds the Announce into BMCA. If BMCA produces a recommendation (e.g. “become slave”),
    /// it is returned as a [`StateDecision`] for the port state machine to apply.
    pub(crate) fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("Announce"));

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision() {
            Some(decision) => decision.to_state_decision(),
            None => None,
        }
    }

    /// Transition from `PRE_MASTER` to `MASTER` after the qualification timeout expires.
    pub(crate) fn qualified(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::QualifiedMaster);
        self.profile.master(self.port, self.bmca)
    }

    /// Apply a BMCA recommendation to become (pre-)master.
    ///
    /// This remains in `PRE_MASTER`, but updates the tracked grandmaster identity and (via the
    /// profile) restarts the qualification timeout according to the decision policy.
    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedMaster);

        decision.apply(|qualification_timeout_policy, grandmaster_id| {
            let bmca = self.bmca.with_grandmaster_id(grandmaster_id);

            self.profile
                .pre_master(self.port, bmca, qualification_timeout_policy)
        })
    }

    /// Apply a BMCA recommendation to become a slave of `parent`.
    ///
    /// This transitions into `UNCALIBRATED` and switches the BMCA wrapper to parent tracking so
    /// that message acceptance and subsequent decisions can be gated by the selected parent.
    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let bmca = self.bmca.into_parent_tracking(parent);

        self.profile.uncalibrated(self.port, bmca)
    }

    /// Transition to `FAULTY` upon fault detection.
    pub(crate) fn fault_detected(self) -> PortState<'a, P, S> {
        let (bmca, best_foreign) = self.bmca.into_parts();
        self.profile.faulty(self.port, bmca, best_foreign)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::cell::Cell;

    use crate::bmca::{
        BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, ClockDS,
        ForeignClockRecord, GrandMasterTrackingBmca,
    };
    use crate::clock::{LocalClock, TimeScale};
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::portstate::PortState;
    use crate::portstate::StateDecision;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakePort, FakeTimerHost, FakeTimestamping, TestClockDS};
    use crate::time::{Instant, LogMessageInterval};

    type PreMasterTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type PreMasterTestPort<'a> =
        PreMasterPort<'a, PreMasterTestDomainPort<'a>, ForeignClockRecordsVec>;

    struct PreMasterPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        foreign_candidates: Cell<BestForeignSnapshot>,
    }

    impl PreMasterPortTestSetup {
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
                foreign_candidates: Cell::new(BestForeignSnapshot::Empty),
            }
        }

        fn port_under_test(&self, records: &[ForeignClockRecord]) -> PreMasterTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            );

            let qualification_timeout = domain_port.timeout(SystemMessage::QualificationTimeout);
            let grandmaster_id = *self.local_clock.identity();

            PreMasterPort::new(
                domain_port,
                GrandMasterTrackingBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(
                        PortNumber::new(1),
                        ForeignClockRecordsVec::from_records(records),
                    ),
                    grandmaster_id,
                ),
                qualification_timeout,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn pre_master_port_test_setup_is_side_effect_free() {
        let setup = PreMasterPortTestSetup::new(TestClockDS::default_low_grade().dataset());

        let _pre_master = setup.port_under_test(&[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn pre_master_port_to_master_on_qualified() {
        let setup = PreMasterPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let pre_master = setup.port_under_test(&[]);

        let master = pre_master.qualified();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn pre_master_port_produces_slave_recommendation_on_two_better_announces() {
        use crate::message::AnnounceMessage;
        use crate::port::{ParentPortIdentity, PortIdentity, PortNumber};

        let setup = PreMasterPortTestSetup::new(TestClockDS::default_low_grade().dataset());

        let foreign_clock = TestClockDS::default_high_grade().dataset();
        let better_port = PortIdentity::new(
            TestClockDS::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );
        let mut pre_master = setup.port_under_test(&[]);

        // Receive first better announce
        let decision = pre_master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            better_port,
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        // Receive second better announce
        let decision = pre_master.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            better_port,
            Instant::from_secs(0),
        );

        // expect a slave recommendation
        assert_eq!(
            decision,
            Some(StateDecision::RecommendedSlave(ParentPortIdentity::new(
                better_port
            )))
        );
    }
}
