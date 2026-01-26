//! Port state: Uncalibrated.
//!
//! This module implements the `UNCALIBRATED` state of the IEEE 1588 port state machine
//! (IEEE 1588-2019 §9.2.5).
//!
//! In `rptp`, `UNCALIBRATED` is the intermediate state on the path to `SLAVE` after BMCA has
//! selected a parent. It is responsible for:
//! - processing Announce messages and running BMCA while tracking the selected parent,
//! - accepting synchronization-related messages (Sync/FollowUp/DelayResp) only from that parent,
//! - driving the end-to-end delay mechanism ([`EndToEndDelayMechanism`]) to produce
//!   [`crate::servo::ServoSample`]s, and
//! - feeding samples into the local clock discipline boundary (`LocalClock::discipline`).
//!
//! When the servo reports a locked state for a completed sample, this state emits
//! [`StateDecision::MasterClockSelected`], which transitions the port into `SLAVE`.
//!
//! Periodic DelayReq transmission is triggered by `SystemMessage::DelayRequestTimeout` as handled
//! by `PortState::dispatch_system`, which calls [`UncalibratedPort::send_delay_request`]. The
//! egress timestamp of that DelayReq is later reported back via `SystemMessage::Timestamp` and
//! recorded using [`UncalibratedPort::process_delay_request`].

use crate::bmca::{
    BestForeignSnapshot, Bmca, BmcaMasterDecision, ForeignClockRecords, ParentTrackingBmca,
};
use crate::e2e::EndToEndDelayMechanism;
use crate::log::PortEvent;
use crate::message::{
    AnnounceMessage, DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
    OneStepSyncMessage, TwoStepSyncMessage,
};
use crate::port::{AnnounceReceiptTimeout, ParentPortIdentity, Port, PortIdentity, SendResult};
use crate::portstate::{PortState, StateDecision};
use crate::profile::PortProfile;
use crate::servo::ServoState;
use crate::time::{Instant, TimeStamp};

/// Port role for the `UNCALIBRATED` state.
///
/// This state is typically assembled by [`PortProfile::uncalibrated`] after a
/// [`StateDecision::RecommendedSlave`] transition from `LISTENING`, `MASTER`, `PRE_MASTER`, or
/// `SLAVE`.
///
/// ## Parent tracking and message acceptance
///
/// The port tracks a [`ParentPortIdentity`] via [`ParentTrackingBmca`]. Messages that affect
/// synchronization (Sync/FollowUp/DelayResp) are ignored unless they originate from the tracked
/// parent.
///
/// ## Transition to `SLAVE`
///
/// Once the E2E delay mechanism yields a complete sample and the servo reports
/// [`ServoState::Locked`], the port emits [`StateDecision::MasterClockSelected`] and transitions
/// into `SLAVE`.
pub struct UncalibratedPort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: ParentTrackingBmca<'a, S>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> UncalibratedPort<'a, P, S> {
    /// Create a new `UncalibratedPort`.
    ///
    /// `announce_receipt_timeout` is expected to be configured with the profile’s
    /// AnnounceReceiptTimeout interval and started by the profile before entering this state.
    pub(crate) fn new(
        port: P,
        bmca: ParentTrackingBmca<'a, S>,
        announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
        delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become UncalibratedPort"));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            delay_mechanism,
            profile,
        }
    }

    /// Process an incoming Announce message while in `UNCALIBRATED`.
    ///
    /// This restarts the Announce receipt timeout and feeds the message into BMCA
    /// (which triggers state decision event if e_rbest changed).
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

    /// Process a received one-step Sync event message.
    ///
    /// The message is ignored unless it originates from the currently tracked parent. When the E2E
    /// delay mechanism yields a complete sample, it is fed into the local clock discipline
    /// pipeline. If the servo reports [`ServoState::Locked`], this emits
    /// [`StateDecision::MasterClockSelected`] to transition into `SLAVE`.
    pub(crate) fn process_one_step_sync(
        &mut self,
        sync: OneStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("One-Step Sync"));
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism
            .record_one_step_sync(sync, ingress_timestamp);
        if let Some(sample) = self.delay_mechanism.sample() {
            let state = self.port.local_clock().discipline(sample);

            match state {
                ServoState::Locked => Some(StateDecision::MasterClockSelected(
                    ParentPortIdentity::new(source_port_identity),
                )),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Process a received two-step Sync event message.
    ///
    /// The message is ignored unless it originates from the currently tracked parent. This records
    /// the Sync half of the two-step exchange; the corresponding FollowUp is handled separately.
    pub(crate) fn process_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("Two-Step Sync"));
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism
            .record_two_step_sync(sync, ingress_timestamp);

        None
    }

    /// Process a received FollowUp general message for a previously received two-step Sync.
    ///
    /// The message is ignored unless it originates from the currently tracked parent. If the E2E
    /// delay mechanism yields a complete sample after recording the FollowUp, it is fed into the
    /// local clock discipline pipeline. If the servo reports [`ServoState::Locked`], this emits
    /// [`StateDecision::MasterClockSelected`] to transition into `SLAVE`.
    pub(crate) fn process_follow_up(
        &mut self,
        follow_up: FollowUpMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("FollowUp"));
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism.record_follow_up(follow_up);
        if let Some(sample) = self.delay_mechanism.sample() {
            let servo_state = self.port.local_clock().discipline(sample);
            match servo_state {
                ServoState::Locked => Some(StateDecision::MasterClockSelected(
                    ParentPortIdentity::new(source_port_identity),
                )),
                _ => None,
            }
        } else {
            None
        }
    }

    /// Record the egress timestamp for a locally sent DelayReq.
    ///
    /// This is invoked via `SystemMessage::Timestamp` after [`send_delay_request`](Self::send_delay_request)
    /// has successfully sent an event DelayReq and infrastructure has associated an egress
    /// timestamp with that message.
    pub(crate) fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        egress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("DelayReq"));
        self.delay_mechanism
            .record_delay_request(req, egress_timestamp);

        None
    }

    /// Process a received DelayResp general message.
    ///
    /// The message is ignored unless it originates from the currently tracked parent.
    pub(crate) fn process_delay_response(
        &mut self,
        resp: DelayResponseMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.port.log(PortEvent::MessageReceived("DelayResp"));
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism.record_delay_response(resp);

        None
    }

    /// Send a DelayReq event message.
    ///
    /// The delay-request sequencing and scheduling is owned by the E2E delay mechanism. The egress
    /// timestamp for the sent message is later recorded via
    /// [`process_delay_request`](Self::process_delay_request).
    pub(crate) fn send_delay_request(&mut self) -> SendResult {
        let delay_request = self.delay_mechanism.delay_request();
        self.port
            .send_event(EventMessage::DelayReq(delay_request))?;
        self.port.log(PortEvent::MessageSent("DelayReq"));
        Ok(())
    }

    /// Transition from `UNCALIBRATED` to `SLAVE` after the servo reports `Locked`.
    ///
    /// This is invoked by `PortState::apply(StateDecision::MasterClockSelected(_))`. The provided
    /// `parent` is logged; the state already carries the tracked parent via its BMCA wrapper.
    pub(crate) fn master_clock_selected(self, parent: ParentPortIdentity) -> PortState<'a, P, S> {
        self.port.log(PortEvent::MasterClockSelected { parent });
        self.profile
            .slave(self.port, self.bmca, self.delay_mechanism)
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

    /// Handle Announce receipt timeout expiry while in `UNCALIBRATED`.
    ///
    /// This transitions to `MASTER` using "current grandmaster tracking", which (in current
    /// single-port setups) resolves to the local grandmaster identity.
    pub(crate) fn announce_receipt_timeout_expired(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::AnnounceReceiptTimeout);
        self.bmca.trigger_state_decision_event();

        let bmca = self.bmca.into_current_grandmaster_tracking();
        self.profile.master(self.port, bmca)
    }

    /// Apply a BMCA recommendation to become (pre-)master.
    ///
    /// This transitions into `PRE_MASTER` and switches the BMCA wrapper to grandmaster tracking.
    pub(crate) fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedMaster);

        decision.apply(|qualification_timeout_policy, grandmaster_id| {
            let bmca = self.bmca.into_grandmaster_tracking(grandmaster_id);

            self.profile
                .pre_master(self.port, bmca, qualification_timeout_policy)
        })
    }

    /// Apply a BMCA recommendation to become a slave of `parent`.
    ///
    /// This remains in `UNCALIBRATED`, but updates the tracked parent identity.
    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let bmca = self.bmca.with_parent(parent);

        self.profile.uncalibrated(self.port, bmca)
    }

    /// Apply a BMCA recommendation to become passive.
    ///
    /// This transitions into `PASSIVE` while preserving the BMCA state.
    pub(crate) fn recommended_passive(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::Static("RecommendedPassive"));
        let bmca = self.bmca.into_passive();
        self.profile.passive(self.port, bmca)
    }

    /// Transition to `FAULTY` upon fault detection.
    pub(crate) fn fault_detected(self) -> PortState<'a, P, S> {
        let (bmca, best_foreign, sde) = self.bmca.into_parts();
        self.profile.faulty(self.port, bmca, best_foreign, sde)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{
        BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, ClockDS,
        ForeignClockRecord,
    };
    use crate::clock::{ClockIdentity, LocalClock, TimeScale};
    use crate::e2e::DelayCycle;
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{DelayRequestMessage, DelayResponseMessage, SystemMessage};
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortIdentity, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeStateDecisionEvent, FakeTimerHost, FakeTimestamping, TestClockDS,
    };
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    type UncalibratedTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type UncalibratedTestPort<'a> =
        UncalibratedPort<'a, UncalibratedTestDomainPort<'a>, ForeignClockRecordsVec>;

    struct UncalibratedPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        state_decision_event: FakeStateDecisionEvent,
    }

    impl UncalibratedPortTestSetup {
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

        fn port_under_test(
            &self,
            port_number: PortNumber,
            parent_port: PortIdentity,
            records: &[ForeignClockRecord],
        ) -> UncalibratedTestPort<'_> {
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

            let delay_timeout = domain_port.timeout(SystemMessage::DelayRequestTimeout);
            let delay_cycle = DelayCycle::new(0.into(), delay_timeout, LogInterval::new(0));

            // Initialize initial state from the provided records
            let best_foreign_record =
                BestForeignRecord::new(port_number, ForeignClockRecordsVec::from_records(records));
            let current_e_rbest_snapshot = best_foreign_record.snapshot();

            UncalibratedPort::new(
                domain_port,
                ParentTrackingBmca::new(
                    BestMasterClockAlgorithm::new(&self.default_ds, port_number),
                    best_foreign_record.with_current_e_rbest(current_e_rbest_snapshot),
                    ParentPortIdentity::new(parent_port),
                    &self.state_decision_event,
                ),
                announce_receipt_timeout,
                EndToEndDelayMechanism::new(delay_cycle),
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn uncalibrated_port_test_setup_is_side_effect_free() {
        let setup = UncalibratedPortTestSetup::new(TestClockDS::default_low_grade().dataset());

        let _uncalibrated = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn uncalibrated_port_triggers_state_decision_event_with_new_parent() {
        let setup = UncalibratedPortTestSetup::new(TestClockDS::default_low_grade().dataset());

        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = TestClockDS::default_mid_grade().dataset();
        let prior_records = [ForeignClockRecord::new_qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let mut uncalibrated =
            setup.port_under_test(PortNumber::new(1), parent_port, &prior_records);

        // Receive two better announces from another parent port
        let new_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );

        // First announce - not yet qualified
        uncalibrated.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                TestClockDS::default_high_grade().dataset(),
                TimeScale::Ptp,
            ),
            new_parent,
            Instant::from_secs(0),
        );

        // No event on first announce (not qualified yet)
        assert!(
            setup.state_decision_event.take_events().is_empty(),
            "No event expected on first announce from new parent"
        );

        // Second announce qualifies the new parent
        uncalibrated.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                TestClockDS::default_high_grade().dataset(),
                TimeScale::Ptp,
            ),
            new_parent,
            Instant::from_secs(1),
        );

        // State decision event should be triggered (new parent is now best and qualified)
        let events = setup.state_decision_event.take_events();
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        assert_eq!(
            events[0].1,
            BestForeignSnapshot::Qualified {
                ds: TestClockDS::default_high_grade().dataset(),
                source_port_identity: new_parent,
                received_on_port: PortNumber::new(1),
            }
        );
    }

    #[test]
    fn uncalibrated_port_becomes_slave_on_next_sync_from_parent() {
        let setup = UncalibratedPortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let parent_port = PortIdentity::fake();
        let foreign_clock_ds = TestClockDS::default_high_grade().dataset();
        let prior_records = [ForeignClockRecord::new_qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let mut uncalibrated =
            setup.port_under_test(PortNumber::new(1), parent_port, &prior_records);

        // Pre-feed the delay mechanism with delay req/resp messages so it can calibrate
        let decision = uncalibrated
            .process_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));
        assert!(decision.is_none(), "No decision expected on DelayReq");

        let decision = uncalibrated.process_delay_response(
            DelayResponseMessage::new(
                42.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            parent_port,
        );
        assert!(decision.is_none(), "No decision expected on DelayResp");

        // Process one-step sync from parent - with delay mechanism ready, this should
        // return MasterClockSelected decision (synchronous decision, not via event)
        let decision = uncalibrated.process_one_step_sync(
            OneStepSyncMessage::new(0.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            parent_port,
            TimeStamp::new(1, 0),
        );

        assert!(
            matches!(decision, Some(StateDecision::MasterClockSelected(_))),
            "Expected MasterClockSelected decision when servo locks"
        );
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let setup = UncalibratedPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let uncalibrated = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        let master = uncalibrated.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn uncalibrated_port_triggers_state_decision_event_on_announce_receipt_timeout() {
        let setup = UncalibratedPortTestSetup::new(TestClockDS::default_high_grade().dataset());

        let uncalibrated = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        let _master = uncalibrated.announce_receipt_timeout_expired();

        // Verify that StateDecisionEvent was triggered
        let events = setup.state_decision_event.take_events();
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        // Snapshot should be Empty (no qualified foreign masters)
        assert_eq!(events[0].1, BestForeignSnapshot::Empty);
    }

    #[test]
    fn uncalibrated_port_triggers_state_decision_event_when_local_better_than_foreign() {
        let setup = UncalibratedPortTestSetup::new(TestClockDS::gps_grandmaster().dataset());

        let parent_port = PortIdentity::fake();
        let mut uncalibrated = setup.port_under_test(PortNumber::new(1), parent_port, &[]);

        let foreign_clock = TestClockDS::default_mid_grade().dataset();
        let foreign_port = PortIdentity::new(
            TestClockDS::default_mid_grade().clock_identity(),
            PortNumber::new(1),
        );

        // First announce - not yet qualified
        uncalibrated.process_announce(
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

        // Second announce from the same foreign clock qualifies it
        uncalibrated.process_announce(
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
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        assert_eq!(
            events[0].1,
            BestForeignSnapshot::Qualified {
                ds: foreign_clock,
                source_port_identity: foreign_port,
                received_on_port: PortNumber::new(1),
            }
        );
    }

    #[test]
    fn uncalibrated_port_triggers_state_decision_event_when_non_gm_local_better_than_foreign() {
        let setup = UncalibratedPortTestSetup::new(TestClockDS::default_mid_grade().dataset());

        let parent_port = PortIdentity::fake();
        let mut uncalibrated = setup.port_under_test(PortNumber::new(1), parent_port, &[]);

        let foreign_clock = TestClockDS::default_low_grade_slave_only().dataset();
        let foreign_port = PortIdentity::new(
            TestClockDS::default_low_grade_slave_only().clock_identity(),
            PortNumber::new(1),
        );

        // First announce - not yet qualified
        uncalibrated.process_announce(
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

        // Second announce qualifies the foreign clock
        uncalibrated.process_announce(
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
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        assert_eq!(
            events[0].1,
            BestForeignSnapshot::Qualified {
                ds: foreign_clock,
                source_port_identity: foreign_port,
                received_on_port: PortNumber::new(1),
            }
        );
    }
}
