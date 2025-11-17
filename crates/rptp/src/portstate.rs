use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::faulty::FaultyPort;
use crate::log::Log;
use crate::message::{
    AnnounceMessage, DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
    GeneralMessage, SequenceId, SystemMessage, TwoStepSyncMessage,
};
use crate::port::{ParentPortIdentity, Port, PortIdentity, Timeout};
use crate::sync::MasterEstimate;
use crate::time::TimeStamp;

pub enum StateTransition {
    ToMaster,
    ToSlave(ParentPortIdentity),
    ToUncalibrated,
    ToPreMaster,
    ToListening,
    ToFaulty,
}

pub enum PortState<P: Port, B: Bmca, L: Log> {
    Initializing(InitializingPort<P, B, L>),
    Listening(ListeningPort<P, B, L>),
    Slave(SlavePort<P, B, L>),
    Master(MasterPort<P, B, L>),
    PreMaster(PreMasterPort<P, B, L>),
    Uncalibrated(UncalibratedPort<P, B, L>),
    Faulty(FaultyPort<P, B, L>),
}

impl<P: Port, B: Bmca, L: Log> PortState<P, B, L> {
    pub fn initializing(port: P, bmca: B, log: L) -> Self {
        PortState::Initializing(InitializingPort::new(port, bmca, log))
    }

    pub fn master(port: P, bmca: B, log: L) -> Self {
        let announce_send_timeout =
            port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0));
        let announce_cycle = AnnounceCycle::new(0.into(), announce_send_timeout);
        let sync_timeout = port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout);

        PortState::Master(MasterPort::new(port, bmca, announce_cycle, sync_cycle, log))
    }

    pub fn slave(port: P, bmca: B, parent_port_identity: ParentPortIdentity, log: L) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let delay_cycle = DelayCycle::new(
            0.into(),
            port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0)),
        );

        PortState::Slave(SlavePort::new(
            port,
            bmca,
            parent_port_identity,
            announce_receipt_timeout,
            delay_cycle,
            log,
        ))
    }

    pub fn pre_master(port: P, bmca: B, log: L) -> Self {
        let qualification_timeout =
            port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        PortState::PreMaster(PreMasterPort::new(port, bmca, qualification_timeout, log))
    }

    pub fn listening(port: P, bmca: B, log: L) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        PortState::Listening(ListeningPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            log,
        ))
    }

    pub fn uncalibrated(port: P, bmca: B, log: L) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        PortState::Uncalibrated(UncalibratedPort::new(
            port,
            bmca,
            announce_receipt_timeout,
            log,
        ))
    }

    pub fn transit(self, transition: StateTransition) -> Self {
        match transition {
            StateTransition::ToMaster => match self {
                PortState::Listening(port) => port.to_master(),
                PortState::Slave(port) => port.to_master(),
                PortState::PreMaster(port) => port.to_master(),
                PortState::Uncalibrated(port) => port.to_master(),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateTransition::ToSlave(parent_port_identity) => match self {
                PortState::Uncalibrated(uncalibrated) => {
                    uncalibrated.to_slave(parent_port_identity)
                }
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateTransition::ToUncalibrated => match self {
                PortState::Listening(listening) => listening.to_uncalibrated(),
                PortState::Master(master) => master.to_uncalibrated(),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateTransition::ToPreMaster => match self {
                PortState::Listening(listening) => listening.to_pre_master(),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateTransition::ToListening => match self {
                PortState::Initializing(initializing) => initializing.to_listening(),
                PortState::Uncalibrated(uncalibrated) => uncalibrated.to_listening(),
                _ => PortState::Faulty(FaultyPort::new()),
            },
            StateTransition::ToFaulty => PortState::Faulty(FaultyPort::new()),
        }
    }

    pub fn dispatch_event(
        &mut self,
        msg: EventMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateTransition> {
        use EventMessage::*;
        use PortState::*;

        match (self, msg) {
            (Slave(port), TwoStepSync(msg)) => {
                port.process_two_step_sync(msg, source_port_identity, ingress_timestamp)
            }
            (Master(port), DelayReq(msg)) => port.process_delay_request(msg, ingress_timestamp),
            _ => None,
        }
    }

    pub fn dispatch_general(
        &mut self,
        msg: GeneralMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        use GeneralMessage::*;
        use PortState::*;

        match (self, msg) {
            (Listening(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Slave(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Master(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Uncalibrated(port), Announce(msg)) => port.process_announce(msg, source_port_identity),
            (Slave(port), FollowUp(msg)) => port.process_follow_up(msg, source_port_identity),
            (Slave(port), DelayResp(msg)) => port.process_delay_response(msg, source_port_identity),
            _ => None,
        }
    }

    pub fn dispatch_system(&mut self, msg: SystemMessage) -> Option<StateTransition> {
        use PortState::*;
        use StateTransition::*;
        use SystemMessage::*;

        match (self, msg) {
            (Master(port), AnnounceSendTimeout) => {
                port.send_announce();
                None
            }
            (Slave(port), DelayRequestTimeout) => {
                port.send_delay_request();
                None
            }
            (Master(port), SyncTimeout) => {
                port.send_sync();
                None
            }
            (Master(port), Timestamp(msg)) => match msg.event_msg {
                EventMessage::TwoStepSync(sync_msg) => {
                    port.send_follow_up(sync_msg, msg.egress_timestamp)
                }
                _ => None,
            },
            (Slave(port), Timestamp(msg)) => match msg.event_msg {
                EventMessage::DelayReq(req_msg) => {
                    port.process_delay_request(req_msg, msg.egress_timestamp)
                }
                _ => None,
            },
            (Initializing(_), Initialized) => Some(ToListening),
            (Listening(_) | Slave(_) | Master(_) | Uncalibrated(_), AnnounceReceiptTimeout) => {
                Some(StateTransition::ToMaster)
            }
            (PreMaster(_), QualificationTimeout) => Some(ToMaster),
            _ => None,
        }
    }
}

pub struct InitializingPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> InitializingPort<P, B, L> {
    pub fn new(port: P, bmca: B, log: L) -> Self {
        Self { port, bmca, log }
    }

    pub fn to_listening(self) -> PortState<P, B, L> {
        PortState::listening(self.port, self.bmca, self.log)
    }
}

pub struct ListeningPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> ListeningPort<P, B, L> {
    pub fn new(port: P, bmca: B, announce_receipt_timeout: P::Timeout, log: L) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
            log,
        }
    }

    pub fn to_uncalibrated(self) -> PortState<P, B, L> {
        PortState::Uncalibrated(UncalibratedPort::new(
            self.port,
            self.bmca,
            self.announce_receipt_timeout,
            self.log,
        ))
    }

    pub fn to_pre_master(self) -> PortState<P, B, L> {
        PortState::pre_master(self.port, self.bmca, self.log)
    }

    pub fn to_master(self) -> PortState<P, B, L> {
        PortState::master(self.port, self.bmca, self.log)
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToPreMaster),
            BmcaRecommendation::Slave(_) => Some(StateTransition::ToUncalibrated),
            BmcaRecommendation::Undecided => None,
        }
    }
}

pub struct SlavePort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    delay_cycle: DelayCycle<P::Timeout>,
    master_estimate: MasterEstimate,
    parent_port_identity: ParentPortIdentity,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> SlavePort<P, B, L> {
    pub fn new(
        port: P,
        bmca: B,
        parent_port_identity: ParentPortIdentity,
        announce_receipt_timeout: P::Timeout,
        delay_cycle: DelayCycle<P::Timeout>,
        log: L,
    ) -> Self {
        Self {
            port,
            bmca,
            parent_port_identity,
            announce_receipt_timeout,
            delay_cycle,
            master_estimate: MasterEstimate::new(),
            log,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToUncalibrated),
            BmcaRecommendation::Slave(parent) => {
                self.parent_port_identity = parent;
                None
            }
            BmcaRecommendation::Undecided => None,
        }
    }

    pub fn process_two_step_sync(
        &mut self,
        sync: TwoStepSyncMessage,
        source_port_identity: PortIdentity,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateTransition> {
        self.log.message_received("Sync");
        if !self.parent_port_identity.matches(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self
            .master_estimate
            .record_two_step_sync(sync, ingress_timestamp)
        {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn process_follow_up(
        &mut self,
        follow_up: FollowUpMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("FollowUp");
        if !self.parent_port_identity.matches(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self.master_estimate.record_follow_up(follow_up) {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        egress_timestamp: TimeStamp,
    ) -> Option<StateTransition> {
        self.log.message_received("DelayReq");
        if let Some(estimate) = self
            .master_estimate
            .record_delay_request(req, egress_timestamp)
        {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn process_delay_response(
        &mut self,
        resp: DelayResponseMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("DelayResp");
        if !self.parent_port_identity.matches(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self.master_estimate.record_delay_response(resp) {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn send_delay_request(&mut self) {
        let delay_request = self.delay_cycle.delay_request();
        self.port.send_event(EventMessage::DelayReq(delay_request));
        self.delay_cycle.next();
    }

    pub fn to_master(self) -> PortState<P, B, L> {
        PortState::master(self.port, self.bmca, self.log)
    }
}

pub struct MasterPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    announce_cycle: AnnounceCycle<P::Timeout>,
    sync_cycle: SyncCycle<P::Timeout>,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> MasterPort<P, B, L> {
    pub fn new(
        port: P,
        bmca: B,
        announce_cycle: AnnounceCycle<P::Timeout>,
        sync_cycle: SyncCycle<P::Timeout>,
        log: L,
    ) -> Self {
        Self {
            port,
            bmca,
            announce_cycle,
            sync_cycle,
            log,
        }
    }

    pub fn send_announce(&mut self) {
        let announce_message = self.announce_cycle.announce(&self.port.local_clock());
        self.port
            .send_general(GeneralMessage::Announce(announce_message));
        self.announce_cycle.next();
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("Announce");
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Undecided => None,
            BmcaRecommendation::Slave(_) => Some(StateTransition::ToUncalibrated),
            BmcaRecommendation::Master => None,
        }
    }

    pub fn process_delay_request(
        &mut self,
        req: DelayRequestMessage,
        ingress_timestamp: TimeStamp,
    ) -> Option<StateTransition> {
        self.log.message_received("DelayReq");
        self.port
            .send_general(GeneralMessage::DelayResp(req.response(ingress_timestamp)));

        None
    }

    pub fn send_sync(&mut self) {
        let sync_message = self.sync_cycle.two_step_sync();
        self.port
            .send_event(EventMessage::TwoStepSync(sync_message));
        self.sync_cycle.next();
    }

    pub fn send_follow_up(
        &mut self,
        sync: TwoStepSyncMessage,
        egress_timestamp: TimeStamp,
    ) -> Option<StateTransition> {
        self.port
            .send_general(GeneralMessage::FollowUp(sync.follow_up(egress_timestamp)));
        None
    }

    pub fn to_uncalibrated(self) -> PortState<P, B, L> {
        PortState::uncalibrated(self.port, self.bmca, self.log)
    }
}

pub struct PreMasterPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    _qualification_timeout: P::Timeout,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> PreMasterPort<P, B, L> {
    pub fn new(port: P, bmca: B, _qualification_timeout: P::Timeout, log: L) -> Self {
        Self {
            port,
            bmca,
            _qualification_timeout,
            log,
        }
    }

    pub fn to_master(self) -> PortState<P, B, L> {
        PortState::master(self.port, self.bmca, self.log)
    }
}

pub struct UncalibratedPort<P: Port, B: Bmca, L: Log> {
    port: P,
    bmca: B,
    announce_receipt_timeout: P::Timeout,
    log: L,
}

impl<P: Port, B: Bmca, L: Log> UncalibratedPort<P, B, L> {
    pub fn new(port: P, bmca: B, announce_receipt_timeout: P::Timeout, log: L) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
            log,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.log.message_received("Announce");
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.bmca.consider(source_port_identity, msg);

        match self.bmca.recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToPreMaster),
            BmcaRecommendation::Slave(parent) => Some(StateTransition::ToSlave(parent)),
            BmcaRecommendation::Undecided => Some(StateTransition::ToListening),
        }
    }

    pub fn to_slave(self, parent_port_identity: ParentPortIdentity) -> PortState<P, B, L> {
        let delay_cycle = DelayCycle::new(
            0.into(),
            self.port
                .timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0)),
        );
        PortState::Slave(SlavePort::new(
            self.port,
            self.bmca,
            parent_port_identity,
            self.announce_receipt_timeout,
            delay_cycle,
            self.log,
        ))
    }

    pub fn to_listening(self) -> PortState<P, B, L> {
        PortState::Listening(ListeningPort::new(
            self.port,
            self.bmca,
            self.announce_receipt_timeout,
            self.log,
        ))
    }

    pub fn to_master(self) -> PortState<P, B, L> {
        PortState::master(self.port, self.bmca, self.log)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AnnounceCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
}

impl<T: Timeout> AnnounceCycle<T> {
    pub fn new(start: SequenceId, timeout: T) -> Self {
        Self {
            sequence_id: start,
            timeout,
        }
    }

    pub fn next(&mut self) {
        self.timeout.restart(Duration::from_secs(1));
        self.sequence_id = self.sequence_id.next();
    }

    pub fn announce<C: SynchronizableClock>(&self, local_clock: &LocalClock<C>) -> AnnounceMessage {
        local_clock.announce(self.sequence_id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SyncCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
}

impl<T: Timeout> SyncCycle<T> {
    pub fn new(start: SequenceId, timeout: T) -> Self {
        Self {
            sequence_id: start,
            timeout,
        }
    }

    pub fn next(&mut self) {
        self.timeout.restart(Duration::from_secs(1));
        self.sequence_id = self.sequence_id.next();
    }

    pub fn two_step_sync(&self) -> TwoStepSyncMessage {
        TwoStepSyncMessage::new(self.sequence_id)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct DelayCycle<T: Timeout> {
    sequence_id: SequenceId,
    timeout: T,
}

impl<T: Timeout> DelayCycle<T> {
    pub fn new(start: SequenceId, delay_request_timeout: T) -> Self {
        Self {
            sequence_id: start,
            timeout: delay_request_timeout,
        }
    }

    pub fn next(&mut self) {
        self.timeout.restart(Duration::from_secs(1));
        self.sequence_id = self.sequence_id.next();
    }

    pub fn delay_request(&self) -> DelayRequestMessage {
        DelayRequestMessage::new(self.sequence_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{ForeignClockDS, ForeignClockRecord, FullBmca, LocalClockDS};
    use crate::clock::{ClockIdentity, FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::log::NoopLog;
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage, SystemMessage,
        TwoStepSyncMessage,
    };
    use crate::port::test_support::{FakePort, FakeTimeout, FakeTimerHost};
    use crate::port::{DomainNumber, DomainPort, PortNumber};

    #[test]
    fn slave_port_synchronizes_clock() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = SlavePort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            FakeTimeout::new(SystemMessage::AnnounceReceiptTimeout),
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
            ),
            NoopLog,
        );

        slave.process_two_step_sync(
            TwoStepSyncMessage::new(0.into()),
            PortIdentity::fake(),
            TimeStamp::new(1, 0),
        );
        slave.process_follow_up(
            FollowUpMessage::new(0.into(), TimeStamp::new(1, 0)),
            PortIdentity::fake(),
        );
        slave.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));
        slave.process_delay_response(
            DelayResponseMessage::new(0.into(), TimeStamp::new(2, 0)),
            PortIdentity::fake(),
        );

        assert_eq!(local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn master_port_answers_delay_request_with_delay_response() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::high_grade_test_clock(),
        );
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_cycle,
            sync_cycle,
            NoopLog,
        );

        timer_host.take_system_messages();

        master.process_delay_request(DelayRequestMessage::new(0.into()), TimeStamp::new(0, 0));

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                0.into(),
                TimeStamp::new(0, 0)
            )))
        );

        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_answers_sync_timeout_with_sync() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortState::master(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        master.dispatch_system(SystemMessage::SyncTimeout);

        let messages = port.take_event_messages();
        assert!(
            messages.contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(
                0.into()
            )))
        );
    }

    #[test]
    fn master_port_schedules_next_sync() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortState::master(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        // Drain messages that could have been sent during initialization.
        timer_host.take_system_messages();

        master.dispatch_system(SystemMessage::SyncTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncTimeout));
    }

    #[test]
    fn master_port_answers_timestamped_sync_with_follow_up() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_cycle,
            sync_cycle,
            NoopLog,
        );

        timer_host.take_system_messages();

        master.send_follow_up(TwoStepSyncMessage::new(0.into()), TimeStamp::new(0, 0));

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::FollowUp(FollowUpMessage::new(
                0.into(),
                TimeStamp::new(0, 0)
            )))
        );

        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_schedules_next_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortState::master(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        timer_host.take_system_messages();

        master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_port_sends_announce_on_send_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut master = PortState::master(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        master.dispatch_system(SystemMessage::AnnounceSendTimeout);

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::Announce(AnnounceMessage::new(
                0.into(),
                ForeignClockDS::high_grade_test_clock()
            )))
        );
    }

    #[test]
    fn master_port_to_uncalibrated_transition_on_following_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(
            PortIdentity::fake(),
            AnnounceMessage::new(41.into(), foreign_clock_ds),
        )
        .with_resolved_clock(foreign_clock_ds)];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            announce_cycle,
            sync_cycle,
            NoopLog,
        );

        let transition = master.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, Some(StateTransition::ToUncalibrated)));
    }

    #[test]
    fn master_port_stays_master_on_subsequent_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(
            PortIdentity::fake(),
            AnnounceMessage::new(41.into(), foreign_clock_ds),
        )
        .with_resolved_clock(foreign_clock_ds)];
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            announce_cycle,
            sync_cycle,
            NoopLog,
        );

        // Drain any setup timers
        timer_host.take_system_messages();

        let transition = master.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, None));
        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn master_port_stays_master_on_undecided_bmca() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_cycle = AnnounceCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0)),
        );
        let sync_cycle = SyncCycle::new(
            0.into(),
            domain_port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0)),
        );

        let mut master = MasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_cycle,
            sync_cycle,
            NoopLog,
        );

        // Drain any setup timers
        timer_host.take_system_messages();

        let transition = master.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, None));
        assert!(timer_host.take_system_messages().is_empty());
    }

    #[test]
    fn slave_port_schedules_next_delay_request_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = PortState::slave(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        timer_host.take_system_messages();

        slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayRequestTimeout));
    }

    #[test]
    fn slave_port_answers_delay_request_timeout_with_delay_request() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            &port,
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = PortState::slave(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        slave.dispatch_system(SystemMessage::DelayRequestTimeout);

        let events = port.take_event_messages();
        assert!(events.contains(&EventMessage::DelayReq(DelayRequestMessage::new(0.into()))));
    }

    #[test]
    fn slave_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );

        let mut slave = PortState::slave(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        let transition = slave.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(transition, Some(StateTransition::ToMaster)));
    }

    #[test]
    fn initializing_port_to_listening_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let mut initializing = PortState::Initializing(InitializingPort::new(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        ));

        let transition = initializing.dispatch_system(SystemMessage::Initialized);

        assert!(matches!(transition, Some(StateTransition::ToListening)));
    }

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = PortState::Listening(ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopLog,
        ));

        let transition = listening.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(transition, Some(StateTransition::ToMaster)));
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopLog,
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        // Drain any initial schedules
        timer_host.take_system_messages();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), foreign_clock),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, None));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_to_pre_master_transition_on_two_announces() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let mut listening = ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopLog,
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), foreign_clock.clone()),
            PortIdentity::fake(),
        );
        assert!(matches!(transition, None));

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), foreign_clock.clone()),
            PortIdentity::fake(),
        );
        assert!(matches!(transition, Some(StateTransition::ToPreMaster)));
    }

    #[test]
    fn listening_port_to_uncalibrated_transition_() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopLog,
        );

        let foreign_clock = ForeignClockDS::high_grade_test_clock();

        // Drain any setup timers
        timer_host.take_system_messages();

        let transition = listening.process_announce(
            AnnounceMessage::new(0.into(), foreign_clock),
            PortIdentity::fake(),
        );
        assert!(matches!(transition, None));

        let transition = listening.process_announce(
            AnnounceMessage::new(1.into(), foreign_clock),
            PortIdentity::fake(),
        );
        assert!(matches!(transition, Some(StateTransition::ToUncalibrated)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn pre_master_port_schedules_qualification_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            &timer_host,
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let qualification_timeout =
            domain_port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        let _ = PreMasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            qualification_timeout,
            NoopLog,
        );

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_port_to_master_transition_on_qualification_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let qualification_timeout =
            domain_port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        let mut pre_master = PortState::PreMaster(PreMasterPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            qualification_timeout,
            NoopLog,
        ));

        let transition = pre_master.dispatch_system(SystemMessage::QualificationTimeout);

        assert!(matches!(transition, Some(StateTransition::ToMaster)));
    }

    #[test]
    fn uncalibrated_port_to_slave_transition_on_following_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [ForeignClockRecord::new(
            PortIdentity::fake(),
            AnnounceMessage::new(41.into(), foreign_clock_ds),
        )
        .with_resolved_clock(foreign_clock_ds)];
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut uncalibrated = UncalibratedPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            announce_receipt_timeout,
            NoopLog,
        );

        let transition = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, Some(StateTransition::ToSlave(_))));
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut uncalibrated = PortState::Uncalibrated(UncalibratedPort::new(
            domain_port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            announce_receipt_timeout,
            NoopLog,
        ));

        let transition = uncalibrated.dispatch_system(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(transition, Some(StateTransition::ToMaster)));
    }

    #[test]
    fn announce_cycle_produces_announce_messages_with_monotonic_sequence_ids() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let mut cycle = AnnounceCycle::new(
            0.into(),
            FakeTimeout::new(SystemMessage::AnnounceSendTimeout),
        );
        let msg1 = cycle.announce(&local_clock);
        cycle.next();
        let msg2 = cycle.announce(&local_clock);

        assert_eq!(
            msg1,
            AnnounceMessage::new(0.into(), ForeignClockDS::high_grade_test_clock())
        );
        assert_eq!(
            msg2,
            AnnounceMessage::new(1.into(), ForeignClockDS::high_grade_test_clock())
        );
    }

    #[test]
    fn slave_port_ignores_general_messages_from_non_parent() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

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
        let mut slave = SlavePort::new(
            domain_port,
            bmca,
            ParentPortIdentity::new(parent),
            announce_receipt_timeout,
            delay_cycle,
            NoopLog,
        );

        // Record a TwoStepSync from the parent so a matching FollowUp could produce ms_offset
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into()),
            parent,
            TimeStamp::new(2, 0),
        );
        assert!(matches!(transition, None));

        // Record a delay request timestamp to allow sm_offset calculation
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(matches!(transition, None));

        // Send FollowUp and DelayResp from a non-parent; these should be ignored
        let transition = slave.process_follow_up(
            FollowUpMessage::new(1.into(), TimeStamp::new(1, 0)),
            non_parent,
        );
        assert!(matches!(transition, None));

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(2.into(), TimeStamp::new(2, 0)),
            non_parent,
        );
        assert!(matches!(transition, None));

        // With correct filtering, the local clock should remain unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_ignores_event_messages_from_non_parent() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

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
        let mut slave = SlavePort::new(
            domain_port,
            bmca,
            ParentPortIdentity::new(parent),
            announce_receipt_timeout,
            delay_cycle,
            NoopLog,
        );

        // Send a FollowUp from the parent first (ms offset incomplete without sync)
        let transition =
            slave.process_follow_up(FollowUpMessage::new(1.into(), TimeStamp::new(1, 0)), parent);
        assert!(matches!(transition, None));

        // Now send TwoStepSync from a non-parent; should be ignored
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into()),
            non_parent,
            TimeStamp::new(2, 0),
        );
        assert!(matches!(transition, None));

        // Even if delay path completes, estimate should not trigger without accepted sync
        let transition =
            slave.process_delay_request(DelayRequestMessage::new(2.into()), TimeStamp::new(0, 0));
        assert!(matches!(transition, None));

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(2.into(), TimeStamp::new(2, 0)),
            parent,
        );
        assert!(matches!(transition, None));

        // Local clock remains unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_disciplines_on_matching_conversation() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let domain_port = DomainPort::new(
            &local_clock,
            FakePort::new(),
            FakeTimerHost::new(),
            DomainNumber::new(0),
            PortNumber::new(1),
        );
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        // Parent identity
        let parent = PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0xCC, 0xCC, 0xCC]),
            PortNumber::new(1),
        );

        // Create slave with parent
        let mut slave = SlavePort::new(
            domain_port,
            bmca,
            ParentPortIdentity::new(parent),
            announce_receipt_timeout,
            delay_cycle,
            NoopLog,
        );

        // Matching conversation from the parent (numbers chosen to yield estimate 2s)
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(42.into()),
            parent,
            TimeStamp::new(1, 0),
        );
        assert!(matches!(transition, None));

        let transition = slave.process_follow_up(
            FollowUpMessage::new(42.into(), TimeStamp::new(1, 0)),
            parent,
        );
        assert!(matches!(transition, None));

        let transition =
            slave.process_delay_request(DelayRequestMessage::new(43.into()), TimeStamp::new(0, 0));
        assert!(matches!(transition, None));

        let transition = slave.process_delay_response(
            DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)),
            parent,
        );
        assert!(matches!(transition, None));

        // Local clock disciplined to the estimate
        assert_eq!(local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn sync_cycle_message_produces_two_step_sync_message() {
        let sync_cycle = SyncCycle::new(0.into(), FakeTimeout::new(SystemMessage::SyncTimeout));
        let two_step_sync = sync_cycle.two_step_sync();

        assert_eq!(two_step_sync, TwoStepSyncMessage::new(0.into()));
    }

    #[test]
    fn sync_cycle_next() {
        let mut sync_cycle = SyncCycle::new(0.into(), FakeTimeout::new(SystemMessage::SyncTimeout));
        sync_cycle.next();

        assert_eq!(
            sync_cycle,
            SyncCycle::new(1.into(), FakeTimeout::new(SystemMessage::SyncTimeout))
        );
    }

    #[test]
    fn sync_cycle_next_wraps() {
        let mut sync_cycle = SyncCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
        );
        sync_cycle.next();

        assert_eq!(
            sync_cycle,
            SyncCycle::new(0.into(), FakeTimeout::new(SystemMessage::SyncTimeout))
        );
    }

    #[test]
    fn delay_cycle_produces_delay_request_message() {
        let delay_cycle = DelayCycle::new(
            42.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        let delay_request = delay_cycle.delay_request();

        assert_eq!(delay_request, DelayRequestMessage::new(42.into()));
    }

    #[test]
    fn delay_cycle_next() {
        let mut delay_cycle = DelayCycle::new(
            42.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        delay_cycle.next();

        assert_eq!(
            delay_cycle,
            DelayCycle::new(
                43.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout)
            )
        );
    }

    #[test]
    fn delay_cycle_next_wraps() {
        let mut delay_cycle = DelayCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        delay_cycle.next();

        assert_eq!(
            delay_cycle,
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout)
            )
        );
    }

    #[test]
    fn portstate_listening_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let master = listening.transit(StateTransition::ToMaster);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_slave_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        let master = slave.transit(StateTransition::ToMaster);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_pre_master_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let master = pre_master.transit(StateTransition::ToMaster);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let master = uncalibrated.transit(StateTransition::ToMaster);

        assert!(matches!(master, PortState::Master(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_slave_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let slave = uncalibrated.transit(StateTransition::ToSlave(parent));

        assert!(matches!(slave, PortState::Slave(_)));
    }

    #[test]
    fn portstate_listening_to_uncalibrated_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let uncalibrated = listening.transit(StateTransition::ToUncalibrated);

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_master_to_uncalibrated_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let uncalibrated = master.transit(StateTransition::ToUncalibrated);

        assert!(matches!(uncalibrated, PortState::Uncalibrated(_)));
    }

    #[test]
    fn portstate_listening_to_pre_master_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let pre_master = listening.transit(StateTransition::ToPreMaster);

        assert!(matches!(pre_master, PortState::PreMaster(_)));
    }

    #[test]
    fn portstate_initializing_to_listening_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let listening = initializing.transit(StateTransition::ToListening);

        assert!(matches!(listening, PortState::Listening(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_listening_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let listening = uncalibrated.transit(StateTransition::ToListening);

        assert!(matches!(listening, PortState::Listening(_)));
    }

    // Tests for unimplemented/illegal ToMaster transitions

    #[test]
    fn portstate_initializing_to_master_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateTransition::ToMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_master_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateTransition::ToMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToSlave transitions

    #[test]
    fn portstate_initializing_to_slave_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = initializing.transit(StateTransition::ToSlave(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_listening_to_slave_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = listening.transit(StateTransition::ToSlave(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_slave_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = slave.transit(StateTransition::ToSlave(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_slave_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = master.transit(StateTransition::ToSlave(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_slave_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let parent = ParentPortIdentity::new(PortIdentity::fake());
        let result = pre_master.transit(StateTransition::ToSlave(parent));

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToUncalibrated transitions

    #[test]
    fn portstate_initializing_to_uncalibrated_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateTransition::ToUncalibrated);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_uncalibrated_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        let result = slave.transit(StateTransition::ToUncalibrated);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_uncalibrated_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateTransition::ToUncalibrated);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_uncalibrated_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = uncalibrated.transit(StateTransition::ToUncalibrated);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToPreMaster transitions

    #[test]
    fn portstate_initializing_to_pre_master_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateTransition::ToPreMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_pre_master_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        let result = slave.transit(StateTransition::ToPreMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_pre_master_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateTransition::ToPreMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_pre_master_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateTransition::ToPreMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_pre_master_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = uncalibrated.transit(StateTransition::ToPreMaster);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for unimplemented/illegal ToListening transitions

    #[test]
    fn portstate_listening_to_listening_illegal_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = listening.transit(StateTransition::ToListening);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_listening_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        let result = slave.transit(StateTransition::ToListening);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_listening_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateTransition::ToListening);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_listening_unimplemented_transition_goes_to_faulty() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateTransition::ToListening);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    // Tests for explicit ToFaulty transitions from every state

    #[test]
    fn portstate_initializing_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let initializing = PortState::initializing(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = initializing.transit(StateTransition::ToFaulty);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_listening_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let listening = PortState::listening(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = listening.transit(StateTransition::ToFaulty);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_slave_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let slave = PortState::slave(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            ParentPortIdentity::new(PortIdentity::fake()),
            NoopLog,
        );

        let result = slave.transit(StateTransition::ToFaulty);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_master_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = PortState::master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = master.transit(StateTransition::ToFaulty);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_pre_master_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let pre_master = PortState::pre_master(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = pre_master.transit(StateTransition::ToFaulty);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_uncalibrated_to_faulty_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let uncalibrated = PortState::uncalibrated(
            DomainPort::new(
                &local_clock,
                FakePort::new(),
                FakeTimerHost::new(),
                DomainNumber::new(0),
                PortNumber::new(1),
            ),
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            NoopLog,
        );

        let result = uncalibrated.transit(StateTransition::ToFaulty);

        assert!(matches!(result, PortState::Faulty(_)));
    }

    #[test]
    fn portstate_faulty_to_faulty_transition() {
        let faulty: PortState<
            DomainPort<FakeClock, FakePort, FakeTimerHost>,
            FullBmca<SortedForeignClockRecordsVec>,
            NoopLog,
        > = PortState::Faulty(FaultyPort::new());

        let result = faulty.transit(StateTransition::ToFaulty);

        assert!(matches!(result, PortState::Faulty(_)));
    }
}
