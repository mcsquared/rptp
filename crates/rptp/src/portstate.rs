use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{
    AnnounceMessage, DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
    GeneralMessage, SequenceId, SystemMessage, TwoStepSyncMessage,
};
use crate::port::{Port, PortIdentity, Timeout};
use crate::sync::MasterEstimate;
use crate::time::TimeStamp;

pub enum StateTransition {
    ToMaster,
    ToSlave,
    ToUncalibrated,
    ToPreMaster,
    ToListening,
}

pub enum PortState<P: Port> {
    Initializing(InitializingPort<P>),
    Listening(ListeningPort<P>),
    Slave(SlavePort<P>),
    Master(MasterPort<P>),
    PreMaster(PreMasterPort<P>),
    Uncalibrated(UncalibratedPort<P>),
}

impl<P: Port> PortState<P> {
    pub fn initializing(port: P) -> Self {
        PortState::Initializing(InitializingPort::new(port))
    }

    pub fn master(port: P) -> Self {
        let announce_send_timeout =
            port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(0));
        let announce_cycle = AnnounceCycle::new(0.into(), announce_send_timeout);
        let sync_timeout = port.timeout(SystemMessage::SyncTimeout, Duration::from_secs(0));
        let sync_cycle = SyncCycle::new(0.into(), sync_timeout);

        PortState::Master(MasterPort::new(port, announce_cycle, sync_cycle))
    }

    pub fn slave(port: P) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let delay_cycle = DelayCycle::new(
            0.into(),
            port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0)),
        );

        PortState::Slave(SlavePort::new(port, announce_receipt_timeout, delay_cycle))
    }

    pub fn pre_master(port: P) -> Self {
        let qualification_timeout =
            port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        PortState::PreMaster(PreMasterPort::new(port, qualification_timeout))
    }

    pub fn listening(port: P) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        PortState::Listening(ListeningPort::new(port, announce_receipt_timeout))
    }

    pub fn uncalibrated(port: P) -> Self {
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        PortState::Uncalibrated(UncalibratedPort::new(port, announce_receipt_timeout))
    }

    pub fn transit(self, transition: StateTransition) -> Self {
        match transition {
            StateTransition::ToMaster => match self {
                PortState::Listening(port) => PortState::master(port.port),
                PortState::Slave(port) => PortState::master(port.port),
                PortState::PreMaster(port) => PortState::master(port.port),
                PortState::Uncalibrated(port) => PortState::master(port.port),
                _ => self,
            },
            StateTransition::ToSlave => match self {
                PortState::Listening(listening) => {
                    let delay_cycle = DelayCycle::new(
                        0.into(),
                        listening
                            .port
                            .timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0)),
                    );
                    PortState::Slave(SlavePort::new(
                        listening.port,
                        listening.announce_receipt_timeout,
                        delay_cycle,
                    ))
                }
                PortState::Uncalibrated(uncalibrated) => {
                    let delay_cycle = DelayCycle::new(
                        0.into(),
                        uncalibrated
                            .port
                            .timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0)),
                    );
                    PortState::Slave(SlavePort::new(
                        uncalibrated.port,
                        uncalibrated.announce_receipt_timeout,
                        delay_cycle,
                    ))
                }
                _ => self,
            },
            StateTransition::ToUncalibrated => match self {
                PortState::Listening(listening) => PortState::Uncalibrated(UncalibratedPort::new(
                    listening.port,
                    listening.announce_receipt_timeout,
                )),
                PortState::Master(master) => PortState::uncalibrated(master.port),
                _ => self,
            },
            StateTransition::ToPreMaster => match self {
                PortState::Listening(listening) => PortState::pre_master(listening.port),
                _ => self,
            },
            StateTransition::ToListening => match self {
                PortState::Uncalibrated(uncalibrated) => PortState::Listening(ListeningPort::new(
                    uncalibrated.port,
                    uncalibrated.announce_receipt_timeout,
                )),
                _ => self,
            },
        }
    }

    pub fn process_system_message(self, msg: SystemMessage) -> Self {
        match self {
            PortState::Initializing(port) => port.process_system_message(msg),
            PortState::Listening(port) => port.process_system_message(msg),
            PortState::Slave(port) => port.process_system_message(msg),
            PortState::Master(port) => port.process_system_message(msg),
            PortState::PreMaster(port) => port.process_system_message(msg),
            PortState::Uncalibrated(port) => port.process_system_message(msg),
        }
    }
}

pub struct InitializingPort<P: Port> {
    port: P,
}

impl<P: Port> InitializingPort<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }

    fn process_system_message(self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::Initialized => PortState::listening(self.port),
            _ => PortState::Initializing(self),
        }
    }
}

pub struct ListeningPort<P: Port> {
    port: P,
    announce_receipt_timeout: P::Timeout,
}

impl<P: Port> ListeningPort<P> {
    pub fn new(port: P, announce_receipt_timeout: P::Timeout) -> Self {
        Self {
            port,
            announce_receipt_timeout,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.port.bmca().consider(source_port_identity, msg);

        match self.port.bmca().recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToPreMaster),
            BmcaRecommendation::Slave(_) => Some(StateTransition::ToUncalibrated),
            BmcaRecommendation::Undecided => None,
        }
    }

    fn process_system_message(self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => PortState::master(self.port),
            _ => PortState::Listening(self),
        }
    }
}

pub struct SlavePort<P: Port> {
    port: P,
    announce_receipt_timeout: P::Timeout,
    delay_cycle: DelayCycle<P::Timeout>,
    master_estimate: MasterEstimate,
    parent_port_identity: Option<PortIdentity>,
}

impl<P: Port> SlavePort<P> {
    pub fn new(
        port: P,
        announce_receipt_timeout: P::Timeout,
        delay_cycle: DelayCycle<P::Timeout>,
    ) -> Self {
        Self {
            port,
            announce_receipt_timeout,
            delay_cycle,
            master_estimate: MasterEstimate::new(),
            parent_port_identity: None,
        }
    }

    fn accepts_from(&self, source_port_identity: &PortIdentity) -> bool {
        match &self.parent_port_identity {
            Some(parent) => parent == source_port_identity,
            None => true,
        }
    }

    #[cfg(test)]
    fn with_parent(self, parent_port_identity: PortIdentity) -> Self {
        Self {
            parent_port_identity: Some(parent_port_identity),
            ..self
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.port.bmca().consider(source_port_identity, msg);

        match self.port.bmca().recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToUncalibrated),
            BmcaRecommendation::Slave(parent) => {
                self.parent_port_identity = Some(parent);
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
        if !self.accepts_from(&source_port_identity) {
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
        if !self.accepts_from(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self.master_estimate.record_follow_up(follow_up) {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    pub fn process_delay_response(
        &mut self,
        resp: DelayResponseMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        if !self.accepts_from(&source_port_identity) {
            return None;
        }

        if let Some(estimate) = self.master_estimate.record_delay_response(resp) {
            self.port.local_clock().discipline(estimate);
        }

        None
    }

    fn process_system_message(mut self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => PortState::master(self.port),
            SystemMessage::DelayRequestTimeout => {
                let delay_request = self.delay_cycle.delay_request();

                self.port.send_event(EventMessage::DelayReq(delay_request));

                PortState::Slave(SlavePort {
                    delay_cycle: self.delay_cycle.next(),
                    ..self
                })
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::DelayReq(req) => {
                    if let Some(estimate) =
                        self.master_estimate.record_delay_request(req, timestamp)
                    {
                        self.port.local_clock().discipline(estimate);
                    }

                    PortState::Slave(self)
                }
                _ => PortState::Slave(self),
            },
            _ => PortState::Slave(self),
        }
    }
}

pub struct MasterPort<P: Port> {
    port: P,
    announce_cycle: AnnounceCycle<P::Timeout>,
    sync_cycle: SyncCycle<P::Timeout>,
}

impl<P: Port> MasterPort<P> {
    pub fn new(
        port: P,
        announce_cycle: AnnounceCycle<P::Timeout>,
        sync_cycle: SyncCycle<P::Timeout>,
    ) -> Self {
        Self {
            port,
            announce_cycle,
            sync_cycle,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.port.bmca().consider(source_port_identity, msg);

        match self.port.bmca().recommendation(self.port.local_clock()) {
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
        self.port
            .send_general(GeneralMessage::DelayResp(req.response(ingress_timestamp)));

        None
    }

    fn process_system_message(self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::AnnounceSendTimeout => {
                let announce_message = self.announce_cycle.announce(&self.port.local_clock());
                self.port
                    .send_general(GeneralMessage::Announce(announce_message));

                return PortState::Master(MasterPort {
                    announce_cycle: self.announce_cycle.next(),
                    ..self
                });
            }
            SystemMessage::SyncTimeout => {
                let sync_message = self.sync_cycle.two_step_sync();

                self.port
                    .send_event(EventMessage::TwoStepSync(sync_message));

                return PortState::Master(MasterPort {
                    sync_cycle: self.sync_cycle.next(),
                    ..self
                });
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::TwoStepSync(twostep) => {
                    self.port
                        .send_general(GeneralMessage::FollowUp(twostep.follow_up(timestamp)));
                }
                _ => {}
            },
            _ => {}
        }

        PortState::Master(self)
    }
}

pub struct PreMasterPort<P: Port> {
    port: P,
    _qualification_timeout: P::Timeout,
}

impl<P: Port> PreMasterPort<P> {
    pub fn new(port: P, _qualification_timeout: P::Timeout) -> Self {
        Self {
            port,
            _qualification_timeout,
        }
    }

    fn process_system_message(self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::QualificationTimeout => PortState::master(self.port),
            _ => PortState::PreMaster(self),
        }
    }
}

pub struct UncalibratedPort<P: Port> {
    port: P,
    announce_receipt_timeout: P::Timeout,
}

impl<P: Port> UncalibratedPort<P> {
    pub fn new(port: P, announce_receipt_timeout: P::Timeout) -> Self {
        Self {
            port,
            announce_receipt_timeout,
        }
    }

    pub fn process_announce(
        &mut self,
        msg: AnnounceMessage,
        source_port_identity: PortIdentity,
    ) -> Option<StateTransition> {
        self.announce_receipt_timeout
            .restart(Duration::from_secs(5));
        self.port.bmca().consider(source_port_identity, msg);

        match self.port.bmca().recommendation(self.port.local_clock()) {
            BmcaRecommendation::Master => Some(StateTransition::ToPreMaster),
            BmcaRecommendation::Slave(_parent) => Some(StateTransition::ToSlave),
            BmcaRecommendation::Undecided => Some(StateTransition::ToListening),
        }
    }

    fn process_system_message(self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => PortState::master(self.port),
            _ => PortState::Uncalibrated(self),
        }
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

    pub fn next(self) -> Self {
        self.timeout.restart(Duration::from_secs(1));

        Self {
            sequence_id: self.sequence_id.next(),
            timeout: self.timeout,
        }
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

    pub fn next(self) -> Self {
        self.timeout.restart(Duration::from_secs(1));

        Self {
            sequence_id: self.sequence_id.next(),
            timeout: self.timeout,
        }
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

    pub fn next(self) -> Self {
        self.timeout.restart(Duration::from_secs(1));

        Self {
            sequence_id: self.sequence_id.next(),
            timeout: self.timeout,
        }
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
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::port::test_support::{FakePort, FakeTimeout, FakeTimerHost};
    use crate::port::{DomainPort, PortNumber};

    #[test]
    fn slave_port_synchronizes_clock() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port =
            DomainPort::new(&local_clock, bmca, &port, timer_host, 0, PortNumber::new(1));

        let mut slave = SlavePort::new(
            domain_port,
            FakeTimeout::new(SystemMessage::AnnounceReceiptTimeout),
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout),
            ),
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
        let slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(0.into())),
            timestamp: TimeStamp::new(0, 0),
        });

        if let PortState::Slave(mut slave) = slave {
            slave.process_delay_response(
                DelayResponseMessage::new(0.into(), TimeStamp::new(2, 0)),
                PortIdentity::fake(),
            );
        }

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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
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

        let mut master = MasterPort::new(domain_port, announce_cycle, sync_cycle);

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
    fn master_port_answers_sync_cycle_with_sync() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let master = PortState::master(domain_port);

        master.process_system_message(SystemMessage::SyncTimeout);

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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let master = PortState::master(domain_port);

        // Drain messages that could have been sent during initialization.
        timer_host.take_system_messages();

        master.process_system_message(SystemMessage::SyncTimeout);

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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let master = PortState::master(domain_port);

        timer_host.take_system_messages();

        master.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::TwoStepSync(TwoStepSyncMessage::new(0.into())),
            timestamp: TimeStamp::new(0, 0),
        });

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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let master = PortState::master(domain_port);

        timer_host.take_system_messages();

        master.process_system_message(SystemMessage::AnnounceSendTimeout);

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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let master = PortState::master(domain_port);

        master.process_system_message(SystemMessage::AnnounceSendTimeout);

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
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            FakePort::new(),
            FakeTimerHost::new(),
            0,
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

        let mut master = MasterPort::new(domain_port, announce_cycle, sync_cycle);

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
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            &port,
            &timer_host,
            0,
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

        let mut master = MasterPort::new(domain_port, announce_cycle, sync_cycle);

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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
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

        let mut master = MasterPort::new(domain_port, announce_cycle, sync_cycle);

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
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            bmca,
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let slave = PortState::slave(domain_port);

        timer_host.take_system_messages();

        slave.process_system_message(SystemMessage::DelayRequestTimeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayRequestTimeout));
    }

    #[test]
    fn slave_port_answers_delay_request_timeout_with_delay_request() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            bmca,
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let slave = PortState::slave(domain_port);

        slave.process_system_message(SystemMessage::DelayRequestTimeout);

        let events = port.take_event_messages();
        assert!(events.contains(&EventMessage::DelayReq(DelayRequestMessage::new(0.into()))));
    }

    #[test]
    fn slave_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            bmca,
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let slave = PortState::slave(domain_port);

        let state = slave.process_system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceSendTimeout));
        assert!(system_messages.contains(&SystemMessage::SyncTimeout));
    }

    #[test]
    fn initializing_port_to_listening_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let initializing = InitializingPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        ));

        let state = initializing.process_system_message(SystemMessage::Initialized);

        assert!(matches!(state, PortState::Listening(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let listening = ListeningPort::new(domain_port, announce_receipt_timeout);

        let state = listening.process_system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceSendTimeout));
        assert!(system_messages.contains(&SystemMessage::SyncTimeout));
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(domain_port, announce_receipt_timeout);

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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            FakePort::new(),
            FakeTimerHost::new(),
            0,
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let mut listening = ListeningPort::new(domain_port, announce_receipt_timeout);

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
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut listening = ListeningPort::new(domain_port, announce_receipt_timeout);

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
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
        let qualification_timeout =
            domain_port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        let _ = PreMasterPort::new(domain_port, qualification_timeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_port_to_master_transition_on_qualification_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
        let qualification_timeout =
            domain_port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5));

        let pre_master = PreMasterPort::new(domain_port, qualification_timeout);

        let state = pre_master.process_system_message(SystemMessage::QualificationTimeout);

        assert!(matches!(state, PortState::Master(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceSendTimeout));
        assert!(system_messages.contains(&SystemMessage::SyncTimeout));
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
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let mut uncalibrated = UncalibratedPort::new(domain_port, announce_receipt_timeout);

        let transition = uncalibrated.process_announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
            PortIdentity::fake(),
        );

        assert!(matches!(transition, Some(StateTransition::ToSlave)));
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let uncalibrated = UncalibratedPort::new(domain_port, announce_receipt_timeout);

        let state = uncalibrated.process_system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceSendTimeout));
        assert!(system_messages.contains(&SystemMessage::SyncTimeout));
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
        cycle = cycle.next();
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
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            bmca,
            &port,
            &timer_host,
            0,
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
        let mut slave =
            SlavePort::new(domain_port, announce_receipt_timeout, delay_cycle).with_parent(parent);

        // Record a TwoStepSync from the parent so a matching FollowUp could produce ms_offset
        let transition = slave.process_two_step_sync(
            TwoStepSyncMessage::new(1.into()),
            parent,
            TimeStamp::new(2, 0),
        );
        assert!(matches!(transition, None));

        // Record a delay request timestamp to allow sm_offset calculation
        let slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(2.into())),
            timestamp: TimeStamp::new(0, 0),
        });

        // Send FollowUp and DelayResp from a non-parent; these should be ignored
        if let PortState::Slave(mut slave) = slave {
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
        }

        // With correct filtering, the local clock should remain unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_ignores_event_messages_from_non_parent() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            bmca,
            &port,
            &timer_host,
            0,
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

        // Create slave with chosen parent
        let mut slave =
            SlavePort::new(domain_port, announce_receipt_timeout, delay_cycle).with_parent(parent);

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
        let slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(2.into())),
            timestamp: TimeStamp::new(0, 0),
        });

        if let PortState::Slave(mut slave) = slave {
            let transition = slave.process_delay_response(
                DelayResponseMessage::new(2.into(), TimeStamp::new(2, 0)),
                parent,
            );
            assert!(matches!(transition, None));
        }

        // Local clock remains unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_disciplines_on_matching_conversation() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(
            &local_clock,
            bmca,
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );
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
        let mut slave =
            SlavePort::new(domain_port, announce_receipt_timeout, delay_cycle).with_parent(parent);

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

        let slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(43.into())),
            timestamp: TimeStamp::new(0, 0),
        });

        if let PortState::Slave(mut slave) = slave {
            let transition = slave.process_delay_response(
                DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0)),
                parent,
            );
            assert!(matches!(transition, None));
        }

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
        let sync_cycle = SyncCycle::new(0.into(), FakeTimeout::new(SystemMessage::SyncTimeout));
        let next = sync_cycle.next();

        assert_eq!(
            next,
            SyncCycle::new(1.into(), FakeTimeout::new(SystemMessage::SyncTimeout))
        );
    }

    #[test]
    fn sync_cycle_next_wraps() {
        let sync_cycle = SyncCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::SyncTimeout),
        );
        let next = sync_cycle.next();

        assert_eq!(
            next,
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
        let delay_cycle = DelayCycle::new(
            42.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        let next = delay_cycle.next();

        assert_eq!(
            next,
            DelayCycle::new(
                43.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout)
            )
        );
    }

    #[test]
    fn delay_cycle_next_wraps() {
        let delay_cycle = DelayCycle::new(
            u16::MAX.into(),
            FakeTimeout::new(SystemMessage::DelayRequestTimeout),
        );
        let next = delay_cycle.next();

        assert_eq!(
            next,
            DelayCycle::new(
                0.into(),
                FakeTimeout::new(SystemMessage::DelayRequestTimeout)
            )
        );
    }
}
