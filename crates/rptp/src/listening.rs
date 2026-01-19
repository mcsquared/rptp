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

use crate::bmca::{Bmca, BmcaMasterDecision, ForeignClockRecords, ListeningBmca};
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
        let bmca = self.bmca.into_current_grandmaster_tracking();
        self.profile.master(self.port, bmca)
    }

    /// Process an incoming Announce message.
    ///
    /// This:
    /// - restarts the Announce receipt timeout,
    /// - feeds the message into BMCA, and
    /// - returns a [`StateDecision`] if BMCA recommends a transition.
    pub(crate) fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("Announce"));
        self.announce_receipt_timeout.restart();

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision() {
            Some(decision) => decision.to_state_decision(),
            None => None,
        }
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
        BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, BmcaMasterDecisionPoint,
        ClockDS, ListeningBmca, Priority1,
    };
    use crate::clock::{ClockIdentity, LocalClock, StepsRemoved, TimeScale};
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakePort, FakeTimerHost, FakeTimestamping, TestClockDS};
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
        foreign_candidates: Cell<BestForeignSnapshot>,
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
                foreign_candidates: Cell::new(BestForeignSnapshot::Empty),
            }
        }

        fn local_clock_identity(&self) -> &ClockIdentity {
            self.local_clock.identity()
        }

        fn port_under_test(&self) -> ListeningTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            );

            let announce_receipt_timeout = AnnounceReceiptTimeout::new(
                domain_port.timeout(SystemMessage::AnnounceReceiptTimeout),
                Duration::from_secs(5),
            );

            ListeningPort::new(
                domain_port,
                ListeningBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
                ),
                announce_receipt_timeout,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn listening_port_test_setup_is_side_effect_free() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let _listening = setup.port_under_test();

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let listening = setup.port_under_test();

        let master = listening.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockDS::default_mid_grade().dataset();

        let decision = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(decision.is_none());

        let system_messages = setup.timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_recommends_master_on_two_announces() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockDS::default_mid_grade().dataset();

        let decision = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(matches!(
            decision,
            Some(StateDecision::RecommendedMaster(_))
        ));

        let system_messages = setup.timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_recommends_slave_on_two_announces() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockDS::default_high_grade().dataset();

        let decision = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(matches!(decision, Some(StateDecision::RecommendedSlave(_))));

        let system_messages = setup.timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_m1_master_recommendation() {
        let setup = ListeningPortTestSetup::new(TestClockDS::gps_grandmaster().dataset());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockDS::default_mid_grade().dataset();

        let _ = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let master_decision = match decision {
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            master_decision,
            BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0),
                *setup.local_clock_identity()
            )
        );

        assert!(matches!(
            listening.recommended_master(master_decision),
            PortState::PreMaster(_)
        ));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_m2_master_recommendation() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockDS::default_low_grade_slave_only().dataset();

        let _ = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let master_decision = match decision {
            Some(StateDecision::RecommendedMaster(decision)) => decision,
            _ => panic!("expected RecommendedMaster decision"),
        };

        assert_eq!(
            master_decision,
            BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0),
                *setup.local_clock_identity()
            )
        );

        assert!(matches!(
            listening.recommended_master(master_decision),
            PortState::PreMaster(_)
        ));
    }

    #[test]
    fn listening_port_updates_steps_removed_on_s1_slave_recommendation() {
        let setup = ListeningPortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let mut listening = setup.port_under_test();

        let foreign_clock = TestClockDS::default_high_grade().dataset();

        setup.timer_host.take_system_messages();

        let _ = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let transition = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        let decision = match transition {
            Some(StateDecision::RecommendedSlave(decision)) => decision,
            _ => panic!("expected RecommendedSlave decision"),
        };

        assert!(matches!(
            listening.recommended_slave(decision),
            PortState::Uncalibrated(_)
        ));
    }

    #[test]
    fn listening_port_recommends_passive_when_foreign_better_by_tuple() {
        // Local GPS grandmaster, but foreign has lower priority1 making it better overall
        let setup = ListeningPortTestSetup::new(TestClockDS::gps_grandmaster().dataset());

        let mut listening = setup.port_under_test();

        // Foreign uses lower priority1 so it is better, even though clock class is worse
        let foreign = TestClockDS::default_low_grade_slave_only().with_priority1(Priority1::new(1));
        let foreign_port = PortIdentity::new(foreign.clock_identity(), PortNumber::new(1));

        let decision = listening.process_announce(
            AnnounceMessage::new(
                0.into(),
                LogMessageInterval::new(0),
                foreign.dataset(),
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = listening.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign.dataset(),
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(0),
        );

        assert!(matches!(decision, Some(StateDecision::RecommendedPassive)));
    }
}
