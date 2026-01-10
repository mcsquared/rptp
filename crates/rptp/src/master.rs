//! Port state: Master.
//!
//! This module implements the `MASTER` and related “master-side” behaviour in the IEEE 1588 port
//! state machine (IEEE 1588-2019 §9.2.5).
//!
//! In `rptp`, a port in `Master` is responsible for:
//! - periodically sending Announce messages,
//! - periodically sending Sync messages (currently: two-step Sync),
//! - answering incoming DelayReq messages with DelayResp, and
//! - continuing to evaluate Announce messages via BMCA to detect when a transition is required
//!   (e.g. a better master appears).
//!
//! Timing and sequencing for periodic traffic is encapsulated in [`AnnounceCycle`] and
//! [`SyncCycle`]. Both schedule their next activation by restarting a `Timeout` handle.

use crate::bmca::{
    Bmca, BmcaMasterDecision, ForeignClockRecords, GrandMaster, GrandMasterTrackingBmca,
};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::PortEvent;
use crate::message::{
    AnnounceMessage, DelayRequestMessage, EventMessage, GeneralMessage, SequenceId,
    TwoStepSyncMessage,
};
use crate::port::{ParentPortIdentity, Port, PortIdentity, SendResult, Timeout};
use crate::portstate::{PortState, StateDecision};
use crate::profile::PortProfile;
use crate::time::{Instant, LogInterval, TimeStamp};

/// Port role for the `MASTER` state.
///
/// This state is typically assembled by [`PortProfile::master`]. It owns:
/// - the `Port` boundary (I/O + timers + logging),
/// - a [`GrandMasterTrackingBmca`] instance to keep BMCA decisions stable while tracking a specific
///   grandmaster,
/// - an [`AnnounceCycle`] and [`SyncCycle`] that drive periodic transmissions, and
/// - the [`PortProfile`] used for follow-on state transitions.
///
/// ## Two-step Sync and follow-up handling
///
/// `send_sync()` transmits a two-step Sync as an event message. The corresponding FollowUp is sent
/// later when infrastructure reports the egress timestamp via `SystemMessage::Timestamp` (see
/// `PortState::dispatch_system`). `send_follow_up()` performs that second step.
pub struct MasterPort<'a, P: Port, S: ForeignClockRecords> {
    port: P,
    bmca: GrandMasterTrackingBmca<'a, S>,
    announce_cycle: AnnounceCycle<P::Timeout>,
    sync_cycle: SyncCycle<P::Timeout>,
    profile: PortProfile,
}

impl<'a, P: Port, S: ForeignClockRecords> MasterPort<'a, P, S> {
    /// Create a new `MasterPort`.
    pub(crate) fn new(
        port: P,
        bmca: GrandMasterTrackingBmca<'a, S>,
        announce_cycle: AnnounceCycle<P::Timeout>,
        sync_cycle: SyncCycle<P::Timeout>,
        profile: PortProfile,
    ) -> Self {
        port.log(PortEvent::Static("Become MasterPort"));

        Self {
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            profile,
        }
    }

    /// Send an Announce message and schedule the next Announce timeout.
    ///
    /// The announce payload is produced from the currently tracked grandmaster dataset.
    pub(crate) fn send_announce(&mut self) -> SendResult {
        let local_clock = self.port.local_clock();
        let announce_cycle = &mut self.announce_cycle;
        let announce_message = self
            .bmca
            .using_grandmaster(|gm| announce_cycle.announce(local_clock, gm));
        self.port
            .send_general(GeneralMessage::Announce(announce_message))?;
        self.announce_cycle.next();
        self.port.log(PortEvent::MessageSent("Announce"));
        Ok(())
    }

    /// Process an incoming Announce message while in `MASTER`.
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

    /// Process an incoming DelayReq and respond with a DelayResp.
    ///
    /// `ingress_timestamp` is the event ingress timestamp of the DelayReq (as observed by the
    /// receiver). `requesting_port_identity` identifies the requester and is copied into the
    /// response.
    pub(crate) fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        ingress_timestamp: TimeStamp,
        requesting_port_identity: PortIdentity,
    ) -> SendResult {
        self.port.log(PortEvent::MessageReceived("DelayReq"));

        let response = req.response(
            self.profile
                .log_min_delay_request_interval()
                .log_message_interval(),
            ingress_timestamp,
            requesting_port_identity,
        );

        let result = self.port.send_general(GeneralMessage::DelayResp(response));
        if result.is_ok() {
            self.port.log(PortEvent::MessageSent("DelayResp"));
        }
        result
    }

    /// Send a two-step Sync message and schedule the next Sync timeout.
    pub(crate) fn send_sync(&mut self) -> SendResult {
        let sync_message = self.sync_cycle.two_step_sync();
        self.port
            .send_event(EventMessage::TwoStepSync(sync_message))?;
        self.sync_cycle.next();
        self.port.log(PortEvent::MessageSent("TwoStepSync"));
        Ok(())
    }

    /// Send the FollowUp corresponding to a previously sent two-step Sync.
    ///
    /// `egress_timestamp` is provided by infrastructure and represents the event egress time of the
    /// Sync message. It is embedded into the FollowUp payload.
    pub(crate) fn send_follow_up(
        &mut self,
        sync: TwoStepSyncMessage,
        egress_timestamp: TimeStamp,
    ) -> SendResult {
        self.port
            .send_general(GeneralMessage::FollowUp(sync.follow_up(egress_timestamp)))?;
        self.port.log(PortEvent::MessageSent("FollowUp"));
        Ok(())
    }

    /// Apply a BMCA recommendation to become (pre-)master.
    ///
    /// This transitions into `PRE_MASTER` and updates the tracked grandmaster identity according to
    /// `decision`.
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
}

/// Announce message sequencing and scheduling.
///
/// This object:
/// - holds the Announce sequence counter,
/// - provides [`announce`](Self::announce) to build an Announce message, and
/// - schedules the next send time via [`next`](Self::next).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AnnounceCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
    log_interval: LogInterval,
}

impl<T: Timeout> AnnounceCycle<T> {
    /// Create a new announce cycle starting at `start`.
    pub fn new(start: SequenceId, timeout: T, log_interval: LogInterval) -> Self {
        Self {
            sequence_id: start,
            timeout,
            log_interval,
        }
    }

    /// Restart the timeout for the next Announce and advance the sequence id.
    pub fn next(&mut self) {
        self.timeout.restart(self.log_interval.duration());
        self.sequence_id = self.sequence_id.next();
    }

    /// Build an Announce message for the current sequence id.
    ///
    /// The message reflects:
    /// - the grandmaster dataset (local or foreign) provided by `grandmaster`, and
    /// - the current time scale of the local clock.
    pub fn announce<C: SynchronizableClock>(
        &self,
        local_clock: &LocalClock<C>,
        grandmaster: GrandMaster,
    ) -> AnnounceMessage {
        grandmaster.announce(
            self.sequence_id,
            self.log_interval.log_message_interval(),
            local_clock.time_scale(),
        )
    }
}

/// Sync message sequencing and scheduling.
///
/// `rptp` currently models master-side synchronization using two-step Sync.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SyncCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
    log_interval: LogInterval,
}

impl<T: Timeout> SyncCycle<T> {
    /// Create a new sync cycle starting at `start`.
    pub fn new(start: SequenceId, timeout: T, log_interval: LogInterval) -> Self {
        Self {
            sequence_id: start,
            timeout,
            log_interval,
        }
    }

    /// Restart the timeout for the next Sync and advance the sequence id.
    pub fn next(&mut self) {
        self.timeout.restart(self.log_interval.duration());
        self.sequence_id = self.sequence_id.next();
    }

    /// Build a two-step Sync message for the current sequence id.
    pub fn two_step_sync(&self) -> TwoStepSyncMessage {
        TwoStepSyncMessage::new(self.sequence_id, self.log_interval.log_message_interval())
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
    use crate::clock::{LocalClock, StepsRemoved, TimeScale};
    use crate::infra::infra_support::ForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{
        DelayResponseMessage, EventMessage, FollowUpMessage, GeneralMessage, SystemMessage,
        TwoStepSyncMessage,
    };
    use crate::port::{DomainNumber, DomainPort, PortNumber};
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{
        FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping, TestClockCatalog,
    };
    use crate::time::{Instant, LogInterval, LogMessageInterval};

    type MasterTestDomainPort<'a> =
        DomainPort<'a, FakeClock, &'a FakeTimerHost, FakeTimestamping, NoopPortLog>;

    type MasterTestPort<'a> =
        MasterPort<'a, MasterTestDomainPort<'a>, ForeignClockRecordsVec>;

    struct MasterPortTestSetup {
        local_clock: LocalClock<FakeClock>,
        default_ds: ClockDS,
        physical_port: FakePort,
        timer_host: FakeTimerHost,
        foreign_candidates: Cell<BestForeignSnapshot>,
    }

    impl MasterPortTestSetup {
        fn new(ds: ClockDS) -> Self {
            Self::new_with_time(ds, TimeStamp::new(0, 0))
        }

        fn new_with_time(ds: ClockDS, now: TimeStamp) -> Self {
            Self {
                local_clock: LocalClock::new(
                    FakeClock::new(now, TimeScale::Ptp),
                    *ds.identity(),
                    Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
                ),
                default_ds: ds,
                physical_port: FakePort::new(),
                timer_host: FakeTimerHost::new(),
                foreign_candidates: Cell::new(BestForeignSnapshot::Empty),
            }
        }

        fn port_under_test(&self, records: &[ForeignClockRecord]) -> MasterTestPort<'_> {
            let domain_port = DomainPort::new(
                &self.local_clock,
                &self.physical_port,
                &self.timer_host,
                FakeTimestamping::new(),
                NoopPortLog,
                DomainNumber::new(0),
                PortNumber::new(1),
            );
            let announce_cycle = AnnounceCycle::new(
                0.into(),
                domain_port.timeout(SystemMessage::AnnounceSendTimeout),
                LogInterval::new(0),
            );
            let sync_cycle = SyncCycle::new(
                0.into(),
                domain_port.timeout(SystemMessage::SyncTimeout),
                LogInterval::new(0),
            );
            let grandmaster_id = *self.local_clock.identity();

            MasterPort::new(
                domain_port,
                GrandMasterTrackingBmca::new(
                    BestMasterClockAlgorithm::new(
                        &self.default_ds,
                        &self.foreign_candidates,
                        PortNumber::new(1),
                    ),
                    BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::from_records(records)),
                    grandmaster_id,
                ),
                announce_cycle,
                sync_cycle,
                PortProfile::default(),
            )
        }
    }

    #[test]
    fn master_port_test_setup_is_side_effect_free() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let _slave = setup.port_under_test(&[]);

        assert!(setup.timer_host.take_system_messages().is_empty());
        assert!(setup.physical_port.is_empty());
    }

    #[test]
    fn master_port_answers_delay_request_with_delay_response() {
        let setup = MasterPortTestSetup::new_with_time(
            TestClockCatalog::default_high_grade().default_ds(),
            TimeStamp::new(0, 0),
        );

        let mut master = setup.port_under_test(&[]);

        master
            .process_delay_request(
                DelayRequestMessage::new(0.into()),
                TimeStamp::new(0, 0),
                PortIdentity::fake(),
            )
            .unwrap();

        assert!(
            setup
                .physical_port
                .contains_general_message(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                    0.into(),
                    LogMessageInterval::new(0),
                    TimeStamp::new(0, 0),
                    PortIdentity::fake()
                )))
        );
    }

    #[test]
    fn master_port_sends_sync() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_sync().unwrap();

        assert!(
            setup
                .physical_port
                .contains_event_message(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(
                    0.into(),
                    LogMessageInterval::new(0)
                )))
        );
    }

    #[test]
    fn master_port_schedules_next_sync_timeout() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_sync().unwrap();

        let messages = setup.timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncTimeout));
    }

    #[test]
    fn master_port_sends_follow_up() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master
            .send_follow_up(
                TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0)),
                TimeStamp::new(0, 0),
            )
            .unwrap();

        assert!(
            setup
                .physical_port
                .contains_general_message(&GeneralMessage::FollowUp(FollowUpMessage::new(
                    0.into(),
                    LogMessageInterval::new(0),
                    TimeStamp::new(0, 0)
                )))
        );
    }

    #[test]
    fn master_port_schedules_next_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_announce().unwrap();

        let messages = setup.timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_port_sends_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let mut master = setup.port_under_test(&[]);

        master.send_announce().unwrap();

        assert!(
            setup
                .physical_port
                .contains_general_message(&GeneralMessage::Announce(AnnounceMessage::new(
                    0.into(),
                    LogMessageInterval::new(0),
                    TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0)),
                    TimeScale::Ptp,
                )))
        );
    }

    #[test]
    fn master_port_recommends_slave_on_two_better_announces() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_mid_grade().default_ds());

        let foreign_clock_ds =
            TestClockCatalog::default_high_grade().foreign_ds(StepsRemoved::new(0));

        let mut master = setup.port_under_test(&[]);

        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );
        assert!(decision.is_none());

        let decision = master.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(matches!(decision, Some(StateDecision::RecommendedSlave(_))));
    }

    #[test]
    fn master_port_stays_master_on_subsequent_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let foreign_clock_ds =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));
        let prior_records = [ForeignClockRecord::qualified(
            PortIdentity::fake(),
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut master = setup.port_under_test(&prior_records);

        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(decision.is_none());
        assert!(setup.timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_stays_master_single_new_announce() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let foreign_clock_ds =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));

        let mut master = setup.port_under_test(&[]);

        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                foreign_clock_ds,
                TimeScale::Ptp,
            ),
            PortIdentity::fake(),
            Instant::from_secs(0),
        );

        assert!(decision.is_none());
    }

    #[test]
    fn master_port_does_not_recommend_master_when_local_clock_unchanged_but_still_best() {
        let setup = MasterPortTestSetup::new(TestClockCatalog::default_high_grade().default_ds());

        let parent_port = PortIdentity::fake();
        let foreign_clock_ds =
            TestClockCatalog::default_low_grade_slave_only().foreign_ds(StepsRemoved::new(0));
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];

        let mut master = setup.port_under_test(&prior_records);

        // Receive a better announce (but still lower quality than local high-grade clock)
        let decision = master.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                TestClockCatalog::default_mid_grade().foreign_ds(StepsRemoved::new(0)),
                TimeScale::Ptp,
            ),
            parent_port,
            Instant::from_secs(0),
        );

        // expect no state change - master stays master when receiving worse announces
        assert!(decision.is_none());
    }

    #[test]
    fn announce_cycle_produces_announce_messages_with_monotonic_sequence_ids() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let clock_ds = TestClockCatalog::default_high_grade().default_ds();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *clock_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let bmca =
            BestMasterClockAlgorithm::new(&clock_ds, &foreign_candidates, PortNumber::new(1));

        let mut cycle = AnnounceCycle::new(
            0.into(),
            FakeTimeout::new(SystemMessage::AnnounceSendTimeout),
            LogInterval::new(0),
        );
        let msg1 = bmca.using_grandmaster(|gm| cycle.announce(&local_clock, gm));
        cycle.next();
        let msg2 = bmca.using_grandmaster(|gm| cycle.announce(&local_clock, gm));

        assert_eq!(
            msg1,
            AnnounceMessage::new(
                0.into(),
                LogInterval::new(0).log_message_interval(),
                clock_ds,
                TimeScale::Ptp,
            )
        );
        assert_eq!(
            msg2,
            AnnounceMessage::new(
                1.into(),
                LogInterval::new(0).log_message_interval(),
                clock_ds,
                TimeScale::Ptp,
            )
        );
    }

    #[test]
    fn sync_cycle_message_produces_two_step_sync_message() {
        let sync_cycle = SyncCycle::new(
            0.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
            LogInterval::new(0),
        );
        let two_step_sync = sync_cycle.two_step_sync();

        assert_eq!(
            two_step_sync,
            TwoStepSyncMessage::new(0.into(), LogMessageInterval::new(0))
        );
    }

    #[test]
    fn sync_cycle_next() {
        let mut sync_cycle = SyncCycle::new(
            0.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
            LogInterval::new(0),
        );
        sync_cycle.next();

        assert_eq!(
            sync_cycle,
            SyncCycle::new(
                1.into(),
                FakeTimeout::new(SystemMessage::SyncTimeout),
                LogInterval::new(0)
            )
        );
    }

    #[test]
    fn sync_cycle_next_wraps() {
        let mut sync_cycle = SyncCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
            LogInterval::new(0),
        );
        sync_cycle.next();

        assert_eq!(
            sync_cycle,
            SyncCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::SyncTimeout),
                LogInterval::new(0)
            )
        );
    }
}
