//! Port state: Slave.
//!
//! This module implements the `SLAVE` state of the IEEE 1588 port state machine
//! (IEEE 1588-2019 §9.2.5).
//!
//! In `rptp`, a port in `Slave` is responsible for:
//! - processing Announce messages and running BMCA while tracking a selected parent,
//! - accepting and processing Sync/FollowUp/DelayResp only from the currently tracked parent,
//! - driving the end-to-end delay mechanism ([`EndToEndDelayMechanism`]) to produce
//!   [`crate::servo::ServoSample`]s, and
//! - feeding samples into the local clock discipline boundary (`LocalClock::discipline`).
//!
//! Periodic DelayReq transmission is triggered by `SystemMessage::DelayRequestTimeout` as handled
//! by `PortState::dispatch_system`, which calls [`SlavePort::send_delay_request`]. The egress
//! timestamp of that DelayReq is later reported back via `SystemMessage::Timestamp` and recorded
//! using [`SlavePort::process_delay_request`].

use crate::bmca::{
    BestForeignDataset, Bmca, BmcaMasterDecision, ForeignClockRecords, ParentTrackingBmca,
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
use crate::uncalibrated::UncalibratedPort;

/// Port role for the `SLAVE` state.
///
/// This state is typically assembled by [`PortProfile::slave`] (via `UNCALIBRATED` once the port
/// has observed a Sync from the selected parent).
///
/// ## Parent tracking and message acceptance
///
/// The slave tracks a [`ParentPortIdentity`] via [`ParentTrackingBmca`]. Event/general messages that
/// affect synchronization (Sync/FollowUp/DelayResp) are ignored unless they originate from the
/// currently tracked parent.
///
/// Announce messages are still processed to keep BMCA updated; if BMCA recommends a different
/// state (new parent, become master, etc.), the state machine applies the returned
/// [`StateDecision`].
///
/// ## Clock discipline
///
/// The E2E delay mechanism combines the Sync-side and Delay-side exchanges into a [`ServoSample`].
/// Once a sample becomes available, it is fed into `LocalClock::discipline`. If the servo reports a
/// non-locked state, a [`StateDecision::SynchronizationFault`] is produced.
pub struct SlavePort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: ParentTrackingBmca<'a, S>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> SlavePort<'a, P, S> {
    /// Create a new `SlavePort`.
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
        port.log(PortEvent::Static("Become SlavePort"));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            delay_mechanism,
            profile,
        }
    }

    /// Process an incoming Announce message while in `SLAVE`.
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
    /// pipeline.
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
            let servo_state = self.port.local_clock().discipline(sample);
            match servo_state {
                ServoState::Locked => None,
                _ => Some(StateDecision::SynchronizationFault),
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
    /// local clock discipline pipeline.
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
                ServoState::Locked => None,
                _ => Some(StateDecision::SynchronizationFault),
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

    /// Process a state decision event.
    pub(crate) fn state_decision_event(
        &self,
        best_master_clock: BestForeignDataset,
    ) -> Option<StateDecision> {
        match self.bmca.decision(best_master_clock) {
            Some(decision) => decision.to_state_decision(),
            None => None,
        }
    }

    /// Send a DelayReq event message.
    ///
    /// The delay-request sequencing and scheduling is owned by the E2E delay mechanism. The egress
    /// timestamp for the sent message is later recorded via [`process_delay_request`](Self::process_delay_request).
    pub(crate) fn send_delay_request(&mut self) -> SendResult {
        let delay_request = self.delay_mechanism.delay_request();
        self.port
            .send_event(EventMessage::DelayReq(delay_request))?;
        self.port.log(PortEvent::MessageSent("DelayReq"));
        Ok(())
    }

    /// Handle Announce receipt timeout expiry while in `SLAVE`.
    ///
    /// This transitions to `MASTER` using "current grandmaster tracking", which (in current
    /// single-port setups) resolves to the local grandmaster identity.
    pub(crate) fn announce_receipt_timeout_expired(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::AnnounceReceiptTimeout);
        self.bmca.trigger_state_decision_event();

        let bmca = self.bmca.into_current_grandmaster_tracking();
        self.profile.master(self.port, bmca)
    }

    /// Apply a BMCA recommendation to become a slave of `parent`.
    ///
    /// This transitions into `UNCALIBRATED` while keeping parent tracking enabled.
    pub(crate) fn recommended_slave(self, parent: ParentPortIdentity) -> PortState<'a, P, S> {
        self.port.log(PortEvent::RecommendedSlave { parent });

        let bmca = self.bmca.with_parent(parent);

        self.profile.uncalibrated(self.port, bmca)
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

    /// Apply a BMCA recommendation to become passive.
    ///
    /// This transitions into `PASSIVE` while preserving the BMCA state.
    pub(crate) fn recommended_passive(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::Static("RecommendedPassive"));
        let bmca = self.bmca.into_passive();
        self.profile.passive(self.port, bmca)
    }

    /// Handle a synchronization fault reported by the servo.
    ///
    /// This transitions from `SLAVE` to `UNCALIBRATED` while preserving the currently tracked
    /// parent, the announce receipt timeout, and the delay mechanism state.
    pub(crate) fn synchronization_fault(self) -> PortState<'a, P, S> {
        self.port.log(PortEvent::SynchronizationFault);
        PortState::Uncalibrated(UncalibratedPort::new(
            self.port,
            self.bmca,
            self.announce_receipt_timeout,
            self.delay_mechanism,
            self.profile,
        ))
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
    use crate::message::SystemMessage;
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeStateDecisionEvent, FakeTimeout, FakeTimerHost, FakeTimestamping,
        TestClockDS,
    };
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    type SlaveTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type SlaveTestPort<'a> = SlavePort<'a, SlaveTestDomainPort<'a>, ForeignClockRecordsVec>;

    struct SlavePortTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        state_decision_event: FakeStateDecisionEvent,
    }

    impl SlavePortTestSetup {
        fn new() -> Self {
            Self::new_with_ds(TestClockDS::default_mid_grade().dataset())
        }

        fn new_with_ds(ds: ClockDS) -> Self {
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
            parent: PortIdentity,
            records: &[ForeignClockRecord],
        ) -> SlaveTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                port_number,
            );

            let delay_request_timeout = domain_port.timeout(SystemMessage::DelayRequestTimeout);

            // Initialize initial state from the provided records
            let best_foreign_record =
                BestForeignRecord::new(port_number, ForeignClockRecordsVec::from_records(records));
            let current_e_rbest_snapshot = best_foreign_record.current_e_rbest_snapshot();

            SlavePort::new(
                domain_port,
                ParentTrackingBmca::new(
                    BestMasterClockAlgorithm::new(&self.default_ds, port_number),
                    best_foreign_record.with_current_e_rbest(current_e_rbest_snapshot),
                    ParentPortIdentity::new(parent),
                    &self.state_decision_event,
                ),
                AnnounceReceiptTimeout::new(
                    FakeTimeout::new(SystemMessage::AnnounceReceiptTimeout),
                    Duration::from_secs(5),
                ),
                EndToEndDelayMechanism::new(DelayCycle::new(
                    0.into(),
                    delay_request_timeout,
                    LogInterval::new(0),
                )),
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn slave_port_test_setup_is_side_effect_free() {
        let setup = SlavePortTestSetup::new();

        let _slave = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn slave_port_synchronizes_clock_with_two_step_sync() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        slave.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));
        slave.process_delay_response(
            DelayResponseMessage::new(
                0.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            PortIdentity::fake(),
        );
        slave.process_two_step_sync(
            TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0)),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );
        slave.process_follow_up(
            FollowUpMessage::new(0.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
        );

        assert_eq!(setup.local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_synchronizes_clock_with_one_step_sync() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        slave.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));
        slave.process_delay_response(
            DelayResponseMessage::new(
                0.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            PortIdentity::fake(),
        );
        slave.process_one_step_sync(
            OneStepSyncMessage::new(0.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );

        assert_eq!(setup.local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_schedules_next_delay_request_timeout() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        slave.send_delay_request().unwrap();

        let messages = setup.timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayRequestTimeout));
    }

    #[test]
    fn slave_port_sends_delay_request() {
        let setup = SlavePortTestSetup::new();

        let mut slave = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        slave.send_delay_request().unwrap();

        assert!(
            setup
                .physical_port
                .contains_event_message(&EventMessage::DelayReq(DelayRequestMessage::new(
                    0.into()
                )))
        );
    }

    #[test]
    fn slave_port_to_master_transition_on_announce_receipt_timeout() {
        let setup = SlavePortTestSetup::new();

        let slave = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        let master = slave.announce_receipt_timeout_expired();

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn slave_port_triggers_state_decision_event_on_announce_receipt_timeout() {
        let setup = SlavePortTestSetup::new();

        let slave = setup.port_under_test(PortNumber::new(1), PortIdentity::fake(), &[]);

        let _master = slave.announce_receipt_timeout_expired();

        // Verify that StateDecisionEvent was triggered
        let events = setup.state_decision_event.take_events();
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        // Snapshot should be Empty (no qualified foreign masters)
        assert!(
            matches!(events[0].1, BestForeignSnapshot::Empty),
            "Expected Empty snapshot on timeout with no qualified foreign masters"
        );
    }

    #[test]
    fn slave_port_ignores_general_messages_from_non_parent() {
        let setup = SlavePortTestSetup::new();

        // Define a parent and a different non-parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );
        let non_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        // Create slave with a chosen parent
        let mut slave = setup.port_under_test(PortNumber::new(1), parent, &[]);

        // Record a TwoStepSync from the parent so a matching FollowUp could produce ms_offset
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(0)),
            parent,
            TimeStamp::new(2, 0),
        );
        assert!(transition.is_none());

        // Record a delay request timestamp to allow sm_offset calculation
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(transition.is_none());

        // Send FollowUp and DelayResp from a non-parent; these should be ignored
        let transition = slave.process_follow_up(
            FollowUpMessage::new(1.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            non_parent,
        );
        assert!(transition.is_none());

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(
                2.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            non_parent,
        );
        assert!(transition.is_none());

        // With correct filtering, the local clock should remain unchanged
        assert_eq!(setup.local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_ignores_event_messages_from_non_parent() {
        let setup = SlavePortTestSetup::new();

        // Define a parent and a different non-parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );
        let non_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        // Create slave with chosen parent
        let mut slave = setup.port_under_test(PortNumber::new(1), parent, &[]);

        // Send a FollowUp from the parent first (ms offset incomplete without sync)
        let transition = slave.process_follow_up(
            FollowUpMessage::new(1.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            parent,
        );
        assert!(transition.is_none());

        // Now send TwoStepSync from a non-parent; should be ignored
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into(), LogMessageInterval::new(0)),
            non_parent,
            TimeStamp::new(2, 0),
        );
        assert!(transition.is_none());

        // Even if delay path completes, delay mechanism should not trigger without accepted sync
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(transition.is_none());

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(
                2.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            parent,
        );
        assert!(transition.is_none());

        // Local clock remains unchanged
        assert_eq!(setup.local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_disciplines_on_matching_conversation() {
        let setup = SlavePortTestSetup::new();

        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xCC, 0xCC, 0xCC]),
            PortNumber::new(1),
        );

        let mut slave = setup.port_under_test(PortNumber::new(1), parent, &[]);

        let transition =
            slave.process_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        assert!(transition.is_none());

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(
                43.into(),
                LogMessageInterval::new(2),
                TimeStamp::new(2, 0),
                PortIdentity::fake(),
            ),
            parent,
        );
        assert!(transition.is_none());

        // Matching conversation from the parent (numbers chosen to yield to local time at 2s)
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(42.into(), LogMessageInterval::new(0)),
            parent,
            TimeStamp::new(1, 0),
        );
        assert!(transition.is_none());

        let transition = slave.process_follow_up(
            FollowUpMessage::new(42.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            parent,
        );
        assert!(transition.is_none());

        // Local clock disciplined to 2s
        assert_eq!(setup.local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn slave_port_triggers_state_decision_event_on_updated_parent() {
        let setup = SlavePortTestSetup::new_with_ds(TestClockDS::default_low_grade().dataset());

        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = TestClockDS::default_mid_grade().dataset();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut slave = setup.port_under_test(PortNumber::new(1), parent_port, &prior_records);

        // Receive a better announce from the same parent port (dataset changes)
        slave.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                TestClockDS::default_high_grade().dataset(),
                TimeScale::Ptp,
            ),
            parent_port,
            Instant::from_secs(1),
        );

        // State decision event should be triggered because the dataset changed
        let events = setup.state_decision_event.take_events();
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        assert_eq!(
            events[0].1,
            BestForeignSnapshot::Qualified {
                ds: TestClockDS::default_high_grade().dataset(),
                source_port_identity: parent_port,
                received_on_port: PortNumber::new(1),
            }
        );
    }

    #[test]
    fn slave_port_triggers_state_decision_event_with_new_parent() {
        let setup = SlavePortTestSetup::new_with_ds(TestClockDS::default_low_grade().dataset());

        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = TestClockDS::default_mid_grade().dataset();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut slave = setup.port_under_test(PortNumber::new(1), parent_port, &prior_records);

        // Receive two better announces from another parent port
        let new_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );

        // First announce - not yet qualified
        slave.process_announce(
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
        slave.process_announce(
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
    fn slave_port_triggers_state_decision_event_on_worse_parent() {
        let setup = SlavePortTestSetup::new_with_ds(TestClockDS::default_mid_grade().dataset());

        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = TestClockDS::default_high_grade().dataset();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut slave = setup.port_under_test(PortNumber::new(1), parent_port, &prior_records);

        // Receive a worse announce from the current parent (dataset degrades)
        slave.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                TestClockDS::default_low_grade_slave_only().dataset(),
                TimeScale::Ptp,
            ),
            parent_port,
            Instant::from_secs(1),
        );

        // State decision event should be triggered because the parent dataset changed
        let events = setup.state_decision_event.take_events();
        assert_eq!(events.len(), 1,);
        assert_eq!(events[0].0, PortNumber::new(1));
        assert_eq!(
            events[0].1,
            BestForeignSnapshot::Qualified {
                ds: TestClockDS::default_low_grade_slave_only().dataset(),
                source_port_identity: parent_port,
                received_on_port: PortNumber::new(1),
            }
        );
    }
}
