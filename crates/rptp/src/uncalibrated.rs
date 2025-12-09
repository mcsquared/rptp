use crate::bmca::{
    Bmca, BmcaDecision, BmcaMasterDecision, BmcaSlaveDecision, LocalMasterTrackingBmca,
    ParentTrackingBmca,
};
use crate::log::{PortEvent, PortLog};
use crate::message::{
    AnnounceMessage, DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
    OneStepSyncMessage, TwoStepSyncMessage,
};
use crate::port::{AnnounceReceiptTimeout, Port, PortIdentity, SendResult};
use crate::portstate::{PortProfile, PortState, StateDecision};
use crate::servo::ServoState;
use crate::sync::EndToEndDelayMechanism;
use crate::time::{Instant, TimeStamp};

pub struct UncalibratedPort<P: Port, B: Bmca, L: PortLog> {
    port: P,
    bmca: ParentTrackingBmca<B>,
    announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
    delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
    log: L,
    profile: PortProfile,
}

impl<P: Port, B: Bmca, L: PortLog> UncalibratedPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: ParentTrackingBmca<B>,
        announce_receipt_timeout: AnnounceReceiptTimeout<P::Timeout>,
        delay_mechanism: EndToEndDelayMechanism<P::Timeout>,
        log: L,
        profile: PortProfile,
    ) -> Self {
        log.port_event(PortEvent::Static("Become UncalibratedPort"));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            delay_mechanism,
            log,
            profile,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
        now: Instant,
    ) -> Option<StateDecision> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout.restart();

        msg.feed_bmca(&mut self.bmca, source_port_identity, now);

        match self.bmca.decision(self.port.local_clock()) {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(decision) => Some(StateDecision::RecommendedSlave(decision)),
            BmcaDecision::Passive => None, // TODO: Handle Passive transition --- IGNORE ---
            BmcaDecision::Undecided => None,
        }
    }

    pub fn process_one_step_sync(
        &mut self,
        sync: OneStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("One-Step Sync");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism
            .record_one_step_sync(sync, ingress_timestamp);
        if let Some(sample) = self.delay_mechanism.sample() {
            let state = self.port.local_clock().discipline(sample);

            match state {
                ServoState::Locked => Some(StateDecision::MasterClockSelected),
                _ => None,
            }
        } else {
            None
        }
    }

    pub fn process_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("Two-Step Sync");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism
            .record_two_step_sync(sync, ingress_timestamp);

        None
    }

    pub fn process_follow_up(
        &mut self,
        follow_up: FollowUpMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("FollowUp");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism.record_follow_up(follow_up);
        if let Some(sample) = self.delay_mechanism.sample() {
            let servo_state = self.port.local_clock().discipline(sample);
            match servo_state {
                ServoState::Locked => Some(StateDecision::MasterClockSelected),
                _ => None,
            }
        } else {
            None
        }
    }

    pub fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        egress_timestamp: TimeStamp,
    ) -> Option<StateDecision> {
        self.log.message_received("DelayReq");
        self.delay_mechanism
            .record_delay_request(req, egress_timestamp);

        None
    }

    pub fn process_delay_response(
        &mut self,
        resp: DelayResponseMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateDecision> {
        self.log.message_received("DelayResp");
        if !self.bmca.matches_parent(&source_port_identity) {
            return None;
        }

        self.delay_mechanism.record_delay_response(resp);

        None
    }

    pub fn send_delay_request(&mut self) -> SendResult {
        let delay_request = self.delay_mechanism.delay_request();
        self.port
            .send_event(EventMessage::DelayReq(delay_request))?;
        self.log.message_sent("DelayReq");
        Ok(())
    }

    pub fn master_clock_selected(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::MasterClockSelected {
            parent: self.bmca.parent(),
        });
        self.profile
            .slave(self.port, self.bmca, self.delay_mechanism, self.log)
    }

    pub fn announce_receipt_timeout_expired(self) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::AnnounceReceiptTimeout);
        let local_tracking_bmca = LocalMasterTrackingBmca::new(self.bmca.into_inner());
        self.profile
            .master(self.port, local_tracking_bmca, self.log)
    }

    pub fn recommended_master(self, decision: BmcaMasterDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedMaster);
        decision.apply(self.port, self.bmca.into_inner(), self.log, self.profile)
    }

    pub fn recommended_slave(self, decision: BmcaSlaveDecision) -> PortState<P, B, L> {
        self.log.port_event(PortEvent::RecommendedSlave {
            parent: *decision.parent_port_identity(),
        });
        decision.apply(self.port, self.bmca.into_inner(), self.log, self.profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{
        BmcaMasterDecision, BmcaMasterDecisionPoint, DefaultDS, ForeignClockDS, ForeignClockRecord,
        IncrementalBmca,
    };
    use crate::clock::{ClockIdentity, LocalClock, StepsRemoved};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::{NOOP_CLOCK_METRICS, NoopPortLog};
    use crate::message::{DelayRequestMessage, DelayResponseMessage, SystemMessage};
    use crate::port::{DomainNumber, DomainPort, ParentPortIdentity, PortNumber};
    use crate::portstate::PortState;
    use crate::servo::{Servo, SteppingServo};
    use crate::slave::DelayCycle;
    use crate::test_support::{FakeClock, FakePort, FakeTimeout, FakeTimerHost, FakeTimestamping};
    use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

    #[test]
    fn uncalibrated_port_produces_slave_recommendation_with_new_parent() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::low_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let parent_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            PortNumber::new(1),
        );
        let foreign_clock_ds = ForeignClockDS::mid_grade_test_clock();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            domain_port.timeout(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            ),
            Duration::from_secs(5),
        );

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                LogInterval::new(0),
            )),
            NoopPortLog,
            PortProfile::default(),
        );

        // Receive two better announces from another parent port
        let new_parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1A, 0xC5, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            PortNumber::new(1),
        );
        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                42.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock(),
            ),
            new_parent,
            Instant::from_secs(0),
        );
        assert!(decision.is_none()); // first announce from new parent is ignored

        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(
                43.into(),
                LogMessageInterval::new(0),
                ForeignClockDS::high_grade_test_clock(),
            ),
            new_parent,
            Instant::from_secs(0),
        );

        // expect a slave recommendation
        assert_eq!(
            decision,
            Some(StateDecision::RecommendedSlave(BmcaSlaveDecision::new(
                ParentPortIdentity::new(new_parent),
                StepsRemoved::new(1),
            )))
        );
    }

    #[test]
    fn uncalibrated_port_becomes_slave_on_next_sync_from_parent() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let parent_port = PortIdentity::fake();
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::qualified(
            parent_port,
            foreign_clock_ds,
            LogInterval::new(0),
            Instant::from_secs(0),
        )];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            domain_port.timeout(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            ),
            Duration::from_secs(5),
        );

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                LogInterval::new(0),
            )),
            NoopPortLog,
            PortProfile::default(),
        );

        // pre-feed the delay mechanism with delay req/resp messages so it can calibrate
        let decision = uncalibrated
            .process_delay_request(DelayRequestMessage::new(42.into()), TimeStamp::new(1, 0));
        assert!(decision.is_none());
        let decision = uncalibrated.process_delay_response(
            DelayResponseMessage::new(42.into(), TimeStamp::new(2, 0)),
            parent_port,
        );
        assert!(decision.is_none());

        let decision = uncalibrated.process_one_step_sync(
            OneStepSyncMessage::new(0.into(), LogMessageInterval::new(0), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );

        assert!(matches!(decision, Some(StateDecision::MasterClockSelected)));
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::high_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            domain_port.timeout(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            ),
            Duration::from_secs(5),
        );

        let mut uncalibrated = PortState::Uncalibrated(UncalibratedPort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(PortIdentity::fake()),
            ),
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                LogInterval::new(0),
            )),
            NoopPortLog,
            PortProfile::default(),
        ));

        let transition = uncalibrated.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(
            transition,
            Some(StateDecision::AnnounceReceiptTimeoutExpired)
        ));
    }

    #[test]
    fn uncalibrated_port_produces_m1_master_recommendation_when_local_better_than_foreign() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::gm_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let parent_port = PortIdentity::fake();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            domain_port.timeout(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            ),
            Duration::from_secs(5),
        );

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                LogInterval::new(0),
            )),
            NoopPortLog,
            PortProfile::default(),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xAA, 0xAA, 0xAA]),
            PortNumber::new(1),
        );

        // First announce qualifies the foreign record but yields no decision yet.
        let _ = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), LogMessageInterval::new(0), foreign_clock),
            foreign_port,
            Instant::from_secs(0),
        );

        // Second announce from the same foreign clock drives BMCA to a Master(M1) decision.
        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(43.into(), LogMessageInterval::new(0), foreign_clock),
            foreign_port,
            Instant::from_secs(0),
        );

        assert_eq!(
            decision,
            Some(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0)
            )))
        );
    }

    #[test]
    fn uncalibrated_port_produces_m2_master_recommendation_when_non_gm_local_better_than_foreign() {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::mid_grade_test_clock(),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let parent_port = PortIdentity::fake();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            FakeTimestamping::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            domain_port.timeout(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            ),
            Duration::from_secs(5),
        );

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            ParentTrackingBmca::new(
                IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
                ParentPortIdentity::new(parent_port),
            ),
            announce_receipt_timeout,
            EndToEndDelayMechanism::new(DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
                LogInterval::new(0),
            )),
            NoopPortLog,
            PortProfile::default(),
        );

        let foreign_clock = ForeignClockDS::low_grade_test_clock();
        let foreign_port = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xBB, 0xBB, 0xBB]),
            PortNumber::new(1),
        );

        let _ = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), LogMessageInterval::new(0), foreign_clock),
            foreign_port,
            Instant::from_secs(0),
        );

        let decision = uncalibrated.process_announce(
            AnnounceMessage::new(43.into(), LogMessageInterval::new(0), foreign_clock),
            foreign_port,
            Instant::from_secs(0),
        );

        assert_eq!(
            decision,
            Some(StateDecision::RecommendedMaster(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0)
            )))
        );
    }
}
