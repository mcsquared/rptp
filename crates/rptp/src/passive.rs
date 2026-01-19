//! Port state: Passive.
//!
//! This module implements the `PASSIVE` state of the IEEE 1588 port state machine
//! (IEEE 1588-2019 §9.2.5).
//!
//! In `rptp`, a port in `Passive`:
//! - receives and evaluates Announce messages,
//! - maintains the Announce receipt timeout, and
//! - drives BMCA via [`PassiveBmca`] until it can recommend a transition.
//!
//! A passive port does not send any messages (Sync, FollowUp, DelayReq, DelayResp, or Announce)
//! and does not participate in time synchronization.
//!
//! ## Transitions
//!
//! - On [`StateDecision::RecommendedSlave`], transition to `UNCALIBRATED` while preserving the
//!   BMCA state (see [`PassivePort::recommended_slave`]).
//! - On [`StateDecision::RecommendedMaster`], transition to `PRE_MASTER` while preserving the
//!   BMCA state (see [`PassivePort::recommended_master`]).
//! - On Announce receipt timeout expiry, transition to `MASTER` (see
//!   [`PassivePort::announce_receipt_timeout_expired`]).
//!
//! Announce reception is the only message-processing responsibility of this state; event message
//! processing (Sync/DelayReq/…) is handled in other states.

use crate::bmca::{Bmca, BmcaMasterDecision, ForeignClockRecords, PassiveBmca};
use crate::log::PortEvent;
use crate::message::AnnounceMessage;
use crate::port::{AnnounceReceiptTimeout, ParentPortIdentity, Port, PortIdentity};
use crate::portstate::{PortState, StateDecision};
use crate::profile::PortProfile;
use crate::time::Instant;

/// Port role for the `PASSIVE` state.
///
/// The state is entered from other states when BMCA recommends passive and is typically
/// constructed by [`PortProfile::passive`].
///
/// This type owns:
/// - the `Port` boundary (logging and message reception),
/// - a [`PassiveBmca`] instance for Announce-driven recommendation, and
/// - an [`AnnounceReceiptTimeout`] that is restarted on each received Announce.
pub struct PassivePort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: PassiveBmca<'a, S>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> PassivePort<'a, P, S> {
    /// Create a new `PassivePort`.
    ///
    /// `announce_receipt_timeout` is expected to be configured with the profile's
    /// AnnounceReceiptTimeout interval and typically started by the profile before entering this
    /// state.
    pub(crate) fn new(
        port: P,
        bmca: PassiveBmca<'a, S>,
        announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become PassivePort"));

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

        let bmca = self.bmca.into_parent_tracking(parent);

        self.profile.uncalibrated(self.port, bmca)
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

    /// Handle Announce receipt timeout expiry while in `PASSIVE`.
    ///
    /// This transitions to `MASTER` using "current grandmaster tracking", which (in current
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
        BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, ClockDS,
        ForeignClockRecord, PassiveBmca,
    };
    use crate::clock::{ClockIdentity, LocalClock, TimeScale};
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, FakePort, FakeTimerHost, FakeTimestamping, TestClockDS};
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    type PassiveTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type PassiveTestPort<'a> = PassivePort<'a, PassiveTestDomainPort<'a>, ForeignClockRecordsVec>;

    struct PassivePortTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        foreign_candidates: Cell<BestForeignSnapshot>,
    }

    impl PassivePortTestSetup {
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

        fn port_under_test(&self, records: &[ForeignClockRecord]) -> PassiveTestPort<'_> {
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

            PassivePort::new(
                domain_port,
                PassiveBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(
                        PortNumber::new(1),
                        ForeignClockRecordsVec::from_records(records),
                    ),
                ),
                announce_receipt_timeout,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn passive_port_test_setup_is_side_effect_free() {
        let setup = PassivePortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let _passive = setup.port_under_test(&[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn passive_port_to_master_transition_on_announce_receipt_timeout() {
        let setup = PassivePortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let passive = setup.port_under_test(&[]);

        let master = passive.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn passive_port_recommends_master_when_local_is_better() {
        let setup = PassivePortTestSetup::new(TestClockDS::default_high_grade().dataset());

        // Start with a qualified foreign clock record (mid grade, worse than local high grade)
        let foreign_port = PortIdentity::fake();
        let foreign_clock_ds = TestClockDS::default_mid_grade().dataset();
        let prior_records = [ForeignClockRecord::qualified(
            foreign_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut passive = setup.port_under_test(&prior_records);

        // Process another announce from the same foreign clock
        let decision = passive.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(1),
        );

        // Local (high grade) is better than foreign (mid grade), so BMCA returns Master
        // PassiveBmca passes through Master decisions
        assert!(matches!(
            decision,
            Some(StateDecision::RecommendedMaster(_))
        ));
    }

    #[test]
    fn passive_port_recommends_slave_when_foreign_is_better() {
        let setup = PassivePortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        // Start with a qualified foreign clock record (high grade, better than local mid grade)
        let foreign_port = PortIdentity::fake();
        let foreign_clock_ds = TestClockDS::default_high_grade().dataset();
        let prior_records = [ForeignClockRecord::qualified(
            foreign_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut passive = setup.port_under_test(&prior_records);

        // Process another announce from the same foreign clock
        let decision = passive.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(1),
        );

        // Foreign (high grade) is better than local (mid grade), so BMCA returns Slave
        // PassiveBmca passes through Slave decisions
        assert!(matches!(decision, Some(StateDecision::RecommendedSlave(_))));
    }

    #[test]
    fn passive_port_does_not_recommend_passive() {
        // Use local NTP grandmaster to ensure we get a passive decision
        let setup = PassivePortTestSetup::new(TestClockDS::ntp_grandmaster().dataset());

        // Use foreign GPS grandmaster to ensure we get a passive decision, because it's better
        // than a local NTP grandmaster
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = TestClockDS::gps_grandmaster().dataset();
        let prior_records = [ForeignClockRecord::qualified(
            foreign_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut passive = setup.port_under_test(&prior_records);

        let decision = passive.process_announce(
            AnnounceMessage::new(
                1.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            foreign_port,
            Instant::from_secs(1),
        );

        assert!(decision.is_none());
    }
}
