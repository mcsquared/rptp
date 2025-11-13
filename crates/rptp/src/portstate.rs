use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{
    AnnounceMessage, DelayRequestMessage, EventMessage, GeneralMessage, SequenceId, SystemMessage,
    TwoStepSyncMessage,
};
use crate::port::{Port, PortIdentity, Timeout};
use crate::sync::MasterEstimate;
use crate::time::TimeStamp;

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

    pub fn process_event_message(
        self,
        source_port_identity: PortIdentity,
        msg: EventMessage,
        timestamp: TimeStamp,
    ) -> Self {
        match self {
            PortState::Initializing(_) => self,
            PortState::Listening(_) => self,
            PortState::Slave(port) => {
                port.process_event_message(source_port_identity, msg, timestamp)
            }
            PortState::Master(port) => {
                port.process_event_message(source_port_identity, msg, timestamp)
            }
            PortState::PreMaster(_) => self,
            PortState::Uncalibrated(_) => self,
        }
    }

    pub fn process_general_message(
        self,
        source_port_identity: PortIdentity,
        msg: GeneralMessage,
    ) -> Self {
        match self {
            PortState::Initializing(_) => self,
            PortState::Listening(port) => port.process_general_message(source_port_identity, msg),
            PortState::Slave(port) => port.process_general_message(source_port_identity, msg),
            PortState::Master(port) => port.process_general_message(source_port_identity, msg),
            PortState::PreMaster(_) => self,
            PortState::Uncalibrated(port) => {
                port.process_general_message(source_port_identity, msg)
            }
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

    fn process_general_message(
        self,
        source_port_identity: PortIdentity,
        msg: GeneralMessage,
    ) -> PortState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));
                self.port.bmca().consider(source_port_identity, msg);

                match self.port.bmca().recommendation(self.port.local_clock()) {
                    BmcaRecommendation::Undecided => PortState::Listening(self),
                    BmcaRecommendation::Master => PortState::pre_master(self.port),
                    BmcaRecommendation::Slave(_) => PortState::Uncalibrated(UncalibratedPort::new(
                        self.port,
                        self.announce_receipt_timeout,
                    )),
                }
            }
            _ => PortState::Listening(self),
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

    fn with_parent(self, parent_port_identity: PortIdentity) -> Self {
        Self {
            parent_port_identity: Some(parent_port_identity),
            ..self
        }
    }

    fn process_event_message(
        mut self,
        source_port_identity: PortIdentity,
        msg: EventMessage,
        timestamp: TimeStamp,
    ) -> PortState<P> {
        if !self.accepts_from(&source_port_identity) {
            return PortState::Slave(self);
        }

        match msg {
            EventMessage::TwoStepSync(sync) => {
                if let Some(estimate) = self.master_estimate.record_two_step_sync(sync, timestamp) {
                    self.port.local_clock().discipline(estimate);
                }
            }
            _ => {}
        }

        PortState::Slave(self)
    }

    fn process_general_message(
        mut self,
        source_port_identity: PortIdentity,
        msg: GeneralMessage,
    ) -> PortState<P> {
        match msg {
            GeneralMessage::Announce(announce) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));

                self.port.bmca().consider(source_port_identity, announce);

                match self.port.bmca().recommendation(self.port.local_clock()) {
                    BmcaRecommendation::Slave(parent) => {
                        return PortState::Slave(self.with_parent(parent));
                    }
                    _ => {} // TODO: handle undecided and master recommendations
                }
            }
            GeneralMessage::FollowUp(follow_up) => {
                if !self.accepts_from(&source_port_identity) {
                    return PortState::Slave(self);
                }
                if let Some(estimate) = self.master_estimate.record_follow_up(follow_up) {
                    self.port.local_clock().discipline(estimate);
                }
            }
            GeneralMessage::DelayResp(resp) => {
                if !self.accepts_from(&source_port_identity) {
                    return PortState::Slave(self);
                }
                if let Some(estimate) = self.master_estimate.record_delay_response(resp) {
                    self.port.local_clock().discipline(estimate);
                }
            }
        }

        PortState::Slave(self)
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

    fn process_event_message(
        self,
        _source_port_identity: PortIdentity,
        msg: EventMessage,
        timestamp: TimeStamp,
    ) -> PortState<P> {
        match msg {
            EventMessage::DelayReq(req) => self
                .port
                .send_general(GeneralMessage::DelayResp(req.response(timestamp))),
            _ => {}
        }

        PortState::Master(self)
    }

    fn process_general_message(
        self,
        source_port_identity: PortIdentity,
        msg: GeneralMessage,
    ) -> PortState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.port.bmca().consider(source_port_identity, msg);

                match self.port.bmca().recommendation(self.port.local_clock()) {
                    BmcaRecommendation::Undecided => PortState::Master(self),
                    BmcaRecommendation::Slave(_) => PortState::uncalibrated(self.port),
                    BmcaRecommendation::Master => PortState::Master(self),
                }
            }
            _ => PortState::Master(self),
        }
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

    fn process_general_message(
        self,
        source_port_identity: PortIdentity,
        msg: GeneralMessage,
    ) -> PortState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));

                self.port.bmca().consider(source_port_identity, msg);

                match self.port.bmca().recommendation(self.port.local_clock()) {
                    BmcaRecommendation::Undecided => {
                        return PortState::listening(self.port);
                    }
                    BmcaRecommendation::Slave(parent) => {
                        let delay_cycle = DelayCycle::new(
                            0.into(),
                            self.port.timeout(
                                SystemMessage::DelayRequestTimeout,
                                Duration::from_secs(1),
                            ),
                        );

                        return PortState::Slave(
                            SlavePort::new(self.port, self.announce_receipt_timeout, delay_cycle)
                                .with_parent(parent),
                        );
                    }
                    BmcaRecommendation::Master => {
                        return PortState::pre_master(self.port);
                    }
                }
            }
            _ => PortState::Uncalibrated(self),
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

        let mut slave = PortState::slave(domain_port);

        slave = slave.process_event_message(
            PortIdentity::fake(),
            EventMessage::TwoStepSync(TwoStepSyncMessage::new(0.into())),
            TimeStamp::new(1, 0),
        );
        slave = slave.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::FollowUp(FollowUpMessage::new(0.into(), TimeStamp::new(1, 0))),
        );
        slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(0.into())),
            timestamp: TimeStamp::new(0, 0),
        });
        let _ = slave.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::DelayResp(DelayResponseMessage::new(0.into(), TimeStamp::new(2, 0))),
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
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
            PortNumber::new(1),
        );

        let master = PortState::master(domain_port);

        timer_host.take_system_messages();

        master.process_event_message(
            PortIdentity::fake(),
            EventMessage::DelayReq(DelayRequestMessage::new(0.into())),
            TimeStamp::new(0, 0),
        );

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

        let master = PortState::master(domain_port);

        let state = master.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(42.into(), foreign_clock_ds)),
        );

        assert!(matches!(state, PortState::Uncalibrated(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
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

        let master = PortState::master(domain_port);

        // Drain any setup timers
        timer_host.take_system_messages();

        let state = master.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(42.into(), foreign_clock_ds)),
        );

        assert!(matches!(state, PortState::Master(_)));
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

        let master = PortState::master(domain_port);

        // Drain any setup timers
        timer_host.take_system_messages();

        let state = master.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(42.into(), foreign_clock_ds)),
        );

        assert!(matches!(state, PortState::Master(_)));

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

        let listening = ListeningPort::new(domain_port, announce_receipt_timeout);

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        // Drain any initial schedules
        timer_host.take_system_messages();

        let state = listening.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(0.into(), foreign_clock)),
        );

        assert!(matches!(state, PortState::Listening(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_to_pre_master_transition_on_two_announces() {
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

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let state = listening.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(0.into(), foreign_clock.clone())),
        );
        let state = state.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(1.into(), foreign_clock.clone())),
        );

        assert!(matches!(state, PortState::PreMaster(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::QualificationTimeout));
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

        let listening = ListeningPort::new(domain_port, announce_receipt_timeout);

        let foreign_clock = ForeignClockDS::high_grade_test_clock();

        // Drain any setup timers
        timer_host.take_system_messages();

        let state = listening.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(0.into(), foreign_clock)),
        );
        let state = state.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(1.into(), foreign_clock)),
        );

        assert!(matches!(state, PortState::Uncalibrated(_)));

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

        let uncalibrated = UncalibratedPort::new(domain_port, announce_receipt_timeout);

        let state = uncalibrated.process_general_message(
            PortIdentity::fake(),
            GeneralMessage::Announce(AnnounceMessage::new(42.into(), foreign_clock_ds)),
        );

        assert!(matches!(state, PortState::Slave(_)));

        let system_messages = timer_host.take_system_messages();
        assert!(system_messages.contains(&SystemMessage::DelayRequestTimeout));
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
        let mut slave = PortState::Slave(
            SlavePort::new(domain_port, announce_receipt_timeout, delay_cycle).with_parent(parent),
        );

        // Record a TwoStepSync from the parent so a matching FollowUp could produce ms_offset
        slave = slave.process_event_message(
            parent,
            EventMessage::TwoStepSync(TwoStepSyncMessage::new(1.into())),
            TimeStamp::new(2, 0),
        );

        // Record a delay request timestamp to allow sm_offset calculation
        slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(2.into())),
            timestamp: TimeStamp::new(0, 0),
        });

        // Send FollowUp and DelayResp from a non-parent; these should be ignored
        slave = slave.process_general_message(
            non_parent,
            GeneralMessage::FollowUp(FollowUpMessage::new(1.into(), TimeStamp::new(1, 0))),
        );
        let _ = slave.process_general_message(
            non_parent,
            GeneralMessage::DelayResp(DelayResponseMessage::new(2.into(), TimeStamp::new(2, 0))),
        );

        // With correct filtering, the local clock should remain unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_ignores_event_messages_from_non_parent() {
        use crate::message::{EventMessage, FollowUpMessage, GeneralMessage, TwoStepSyncMessage};

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
        let mut slave = PortState::Slave(
            SlavePort::new(domain_port, announce_receipt_timeout, delay_cycle).with_parent(parent),
        );

        // Send a FollowUp from the parent first (ms offset incomplete without sync)
        slave = slave.process_general_message(
            parent,
            GeneralMessage::FollowUp(FollowUpMessage::new(1.into(), TimeStamp::new(1, 0))),
        );

        // Now send TwoStepSync from a non-parent; should be ignored
        slave = slave.process_event_message(
            non_parent,
            EventMessage::TwoStepSync(TwoStepSyncMessage::new(1.into())),
            TimeStamp::new(2, 0),
        );

        // Even if delay path completes, estimate should not trigger without accepted sync
        slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(2.into())),
            timestamp: TimeStamp::new(0, 0),
        });
        let _ = slave.process_general_message(
            parent,
            GeneralMessage::DelayResp(DelayResponseMessage::new(2.into(), TimeStamp::new(2, 0))),
        );

        // Local clock remains unchanged
        assert_eq!(local_clock.now(), TimeStamp::new(0, 0));
    }

    #[test]
    fn slave_port_disciplines_on_matching_conversation() {
        use crate::message::{
            DelayRequestMessage, DelayResponseMessage, EventMessage, FollowUpMessage,
            GeneralMessage, TwoStepSyncMessage,
        };

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
        let mut slave = PortState::Slave(
            SlavePort::new(domain_port, announce_receipt_timeout, delay_cycle).with_parent(parent),
        );

        // Matching conversation from the parent (numbers chosen to yield estimate 2s)
        slave = slave.process_event_message(
            parent,
            EventMessage::TwoStepSync(TwoStepSyncMessage::new(42.into())),
            TimeStamp::new(1, 0),
        );
        slave = slave.process_general_message(
            parent,
            GeneralMessage::FollowUp(FollowUpMessage::new(42.into(), TimeStamp::new(1, 0))),
        );
        slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(43.into())),
            timestamp: TimeStamp::new(0, 0),
        });
        let _ = slave.process_general_message(
            parent,
            GeneralMessage::DelayResp(DelayResponseMessage::new(43.into(), TimeStamp::new(2, 0))),
        );

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
