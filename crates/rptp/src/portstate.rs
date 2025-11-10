use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{
    AnnounceMessage, DelayCycleMessage, EventMessage, GeneralMessage, SequenceId, SyncCycleMessage,
    SystemMessage,
};
use crate::port::{DropTimeout, Port, Timeout};
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
        PortState::Master(MasterPort::new(port))
    }

    pub fn slave(port: P, announce_receipt_timeout: P::Timeout) -> Self {
        PortState::Slave(SlavePort::new(
            port,
            DropTimeout::new(announce_receipt_timeout),
        ))
    }

    pub fn process_event_message(self, msg: EventMessage, timestamp: TimeStamp) -> Self {
        match self {
            PortState::Initializing(_) => self,
            PortState::Listening(_) => self,
            PortState::Slave(port) => port.process_event_message(msg, timestamp),
            PortState::Master(port) => port.process_event_message(msg, timestamp),
            PortState::PreMaster(_) => self,
            PortState::Uncalibrated(_) => self,
        }
    }

    pub fn process_general_message(self, msg: GeneralMessage) -> Self {
        match self {
            PortState::Initializing(_) => self,
            PortState::Listening(port) => port.process_general_message(msg),
            PortState::Slave(port) => port.process_general_message(msg),
            PortState::Master(port) => port.process_general_message(msg),
            PortState::PreMaster(_) => self,
            PortState::Uncalibrated(port) => port.process_general_message(msg),
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
            SystemMessage::Initialized => {
                let announce_receipt_timeout = DropTimeout::new(self.port.timeout(
                    SystemMessage::AnnounceReceiptTimeout,
                    Duration::from_secs(5),
                ));

                PortState::Listening(ListeningPort::new(self.port, announce_receipt_timeout))
            }
            _ => PortState::Initializing(self),
        }
    }
}

pub struct ListeningPort<P: Port> {
    port: P,
    announce_receipt_timeout: DropTimeout<P::Timeout>,
}

impl<P: Port> ListeningPort<P> {
    pub fn new(port: P, announce_receipt_timeout: DropTimeout<P::Timeout>) -> Self {
        Self {
            port,
            announce_receipt_timeout,
        }
    }

    fn process_general_message(self, msg: GeneralMessage) -> PortState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));
                self.port.bmca().consider(msg);

                match self.port.bmca().recommendation(self.port.local_clock()) {
                    BmcaRecommendation::Undecided => PortState::Listening(self),
                    BmcaRecommendation::Master => {
                        PortState::PreMaster(PreMasterPort::new(self.port))
                    }
                    BmcaRecommendation::Slave => PortState::Uncalibrated(UncalibratedPort::new(
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
            SystemMessage::AnnounceReceiptTimeout => PortState::Master(MasterPort::new(self.port)),
            _ => PortState::Listening(self),
        }
    }
}

pub struct SlavePort<P: Port> {
    port: P,
    announce_receipt_timeout: DropTimeout<P::Timeout>,
    delay_cycle_timeout: DropTimeout<P::Timeout>,
    master_estimate: MasterEstimate,
}

impl<P: Port> SlavePort<P> {
    pub fn new(port: P, announce_receipt_timeout: DropTimeout<P::Timeout>) -> Self {
        let delay_cycle_timeout = DropTimeout::new(port.timeout(
            SystemMessage::DelayCycle(DelayCycleMessage::new(0.into())),
            Duration::ZERO,
        ));

        Self {
            port,
            announce_receipt_timeout,
            delay_cycle_timeout,
            master_estimate: MasterEstimate::new(),
        }
    }

    fn process_event_message(mut self, msg: EventMessage, timestamp: TimeStamp) -> PortState<P> {
        match msg {
            EventMessage::TwoStepSync(sync) => {
                if let Some(estimate) = self.master_estimate.ingest_two_step_sync(sync, timestamp) {
                    self.port.local_clock().discipline(estimate);
                }
            }
            _ => {}
        }

        PortState::Slave(self)
    }

    fn process_general_message(mut self, msg: GeneralMessage) -> PortState<P> {
        match msg {
            GeneralMessage::Announce(_) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));
            }
            GeneralMessage::FollowUp(follow_up) => {
                if let Some(estimate) = self.master_estimate.ingest_follow_up(follow_up) {
                    self.port.local_clock().discipline(estimate);
                }
            }
            GeneralMessage::DelayResp(resp) => {
                if let Some(estimate) = self.master_estimate.ingest_delay_response(resp) {
                    self.port.local_clock().discipline(estimate);
                }
            }
        }

        PortState::Slave(self)
    }

    fn process_system_message(mut self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => PortState::Master(MasterPort::new(self.port)),
            SystemMessage::DelayCycle(delay_cycle) => {
                let delay_request = delay_cycle.delay_request();
                let next_cycle = delay_cycle.next();

                self.port.send_event(EventMessage::DelayReq(delay_request));
                self.delay_cycle_timeout.restart_with(
                    SystemMessage::DelayCycle(next_cycle),
                    Duration::from_secs(1),
                );

                PortState::Slave(self)
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::DelayReq(req) => {
                    if let Some(estimate) =
                        self.master_estimate.ingest_delay_request(req, timestamp)
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
    announce_send_timeout: DropTimeout<P::Timeout>,
    sync_cycle_timeout: DropTimeout<P::Timeout>,
    announce_cycle: AnnounceCycle,
}

impl<P: Port> MasterPort<P> {
    pub fn new(port: P) -> Self {
        let announce_send_timeout =
            DropTimeout::new(port.timeout(SystemMessage::AnnounceSendTimeout, Duration::ZERO));
        let sync_cycle_timeout = DropTimeout::new(port.timeout(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0.into())),
            Duration::ZERO,
        ));

        Self {
            port,
            announce_send_timeout,
            sync_cycle_timeout,
            announce_cycle: AnnounceCycle::new(0.into()),
        }
    }

    fn process_event_message(self, msg: EventMessage, timestamp: TimeStamp) -> PortState<P> {
        match msg {
            EventMessage::DelayReq(req) => self
                .port
                .send_general(GeneralMessage::DelayResp(req.response(timestamp))),
            _ => {}
        }

        PortState::Master(self)
    }

    fn process_general_message(self, _msg: GeneralMessage) -> PortState<P> {
        match _msg {
            GeneralMessage::Announce(msg) => {
                self.port.bmca().consider(msg);

                match self.port.bmca().recommendation(self.port.local_clock()) {
                    BmcaRecommendation::Undecided => PortState::Master(self),
                    BmcaRecommendation::Slave => {
                        let announce_receipt_timeout = DropTimeout::new(self.port.timeout(
                            SystemMessage::AnnounceReceiptTimeout,
                            Duration::from_secs(5),
                        ));
                        PortState::Uncalibrated(UncalibratedPort::new(
                            self.port,
                            announce_receipt_timeout,
                        ))
                    }
                    BmcaRecommendation::Master => PortState::Master(self),
                }
            }
            _ => PortState::Master(self),
        }
    }

    fn process_system_message(mut self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::AnnounceSendTimeout => {
                let announce_message = self.announce_cycle.announce(&self.port.local_clock());
                self.port
                    .send_general(GeneralMessage::Announce(announce_message));
                self.announce_send_timeout.restart(Duration::from_secs(1));
            }
            SystemMessage::SyncCycle(sync_cycle) => {
                let sync_message = sync_cycle.two_step_sync();
                let next_cycle = sync_cycle.next();

                self.port
                    .send_event(EventMessage::TwoStepSync(sync_message));
                self.sync_cycle_timeout
                    .restart_with(SystemMessage::SyncCycle(next_cycle), Duration::from_secs(1));
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
    _qualification_timeout: DropTimeout<P::Timeout>,
}

impl<P: Port> PreMasterPort<P> {
    pub fn new(port: P) -> Self {
        let _qualification_timeout = DropTimeout::new(
            port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5)),
        );
        Self {
            port,
            _qualification_timeout,
        }
    }

    fn process_system_message(self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::QualificationTimeout => PortState::Master(MasterPort::new(self.port)),
            _ => PortState::PreMaster(self),
        }
    }
}

pub struct UncalibratedPort<P: Port> {
    port: P,
    announce_receipt_timeout: DropTimeout<P::Timeout>,
}

impl<P: Port> UncalibratedPort<P> {
    pub fn new(port: P, announce_receipt_timeout: DropTimeout<P::Timeout>) -> Self {
        Self {
            port,
            announce_receipt_timeout,
        }
    }

    fn process_general_message(self, msg: GeneralMessage) -> PortState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));

                self.port.bmca().consider(msg);

                match self.port.bmca().recommendation(self.port.local_clock()) {
                    BmcaRecommendation::Undecided => {
                        let announce_receipt_timeout = DropTimeout::new(self.port.timeout(
                            SystemMessage::AnnounceReceiptTimeout,
                            Duration::from_secs(5),
                        ));

                        return PortState::Listening(ListeningPort::new(
                            self.port,
                            announce_receipt_timeout,
                        ));
                    }
                    BmcaRecommendation::Slave => {
                        return PortState::Slave(SlavePort::new(
                            self.port,
                            self.announce_receipt_timeout,
                        ));
                    }
                    BmcaRecommendation::Master => {
                        return PortState::PreMaster(PreMasterPort::new(self.port));
                    }
                }
            }
            _ => PortState::Uncalibrated(self),
        }
    }

    fn process_system_message(self, msg: SystemMessage) -> PortState<P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => PortState::Master(MasterPort::new(self.port)),
            _ => PortState::Uncalibrated(self),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnounceCycle {
    sequence_id: SequenceId,
}

impl AnnounceCycle {
    pub fn new(start: SequenceId) -> Self {
        Self { sequence_id: start }
    }

    pub fn announce<C: SynchronizableClock>(
        &mut self,
        local_clock: &LocalClock<C>,
    ) -> AnnounceMessage {
        let msg = local_clock.announce(self.sequence_id);
        self.sequence_id = self.sequence_id.next();
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bmca::{ForeignClockDS, ForeignClockRecord, FullBmca, LocalClockDS};
    use crate::clock::{FakeClock, LocalClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::port::DomainPort;
    use crate::port::test_support::{FakePort, FakeTimerHost};

    #[test]
    fn slave_port_synchronizes_clock() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::mid_grade_test_clock(),
        );
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(&local_clock, bmca, &port, timer_host, 0);
        let announce_receipt_timeout = DropTimeout::new(domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let mut slave = PortState::Slave(SlavePort::new(domain_port, announce_receipt_timeout));

        slave = slave.process_event_message(
            EventMessage::TwoStepSync(TwoStepSyncMessage::new(0.into())),
            TimeStamp::new(1, 0),
        );
        slave = slave.process_general_message(GeneralMessage::FollowUp(FollowUpMessage::new(
            0.into(),
            TimeStamp::new(1, 0),
        )));
        slave = slave.process_system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(0.into())),
            timestamp: TimeStamp::new(0, 0),
        });
        let _ = slave.process_general_message(GeneralMessage::DelayResp(
            DelayResponseMessage::new(0.into(), TimeStamp::new(2, 0)),
        ));

        assert_eq!(local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn master_port_answers_delay_request_with_delay_response() {
        let local_clock = LocalClock::new(
            FakeClock::new(TimeStamp::new(0, 0)),
            LocalClockDS::high_grade_test_clock(),
        );
        let port = FakePort::new();

        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        ));

        master.process_event_message(
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
    }

    #[test]
    fn master_port_schedules_initial_sync() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();

        let _ = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
        ));

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0.into()))));
    }

    #[test]
    fn master_port_answers_sync_cycle_with_sync() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();

        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
        ));

        master.process_system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0.into())));

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
        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
        ));

        // Drain messages that could have been sent during initialization.
        timer_host.take_system_messages();

        master.process_system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0.into())));

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(1.into()))));
    }

    #[test]
    fn master_port_answers_timestamped_sync_with_follow_up() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();

        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        ));

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
    }

    #[test]
    fn master_port_schedules_initial_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();

        let _ = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
        ));

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_port_schedules_next_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();

        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
        ));

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

        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
        ));

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
        let prior_records =
            [
                ForeignClockRecord::new(AnnounceMessage::new(41.into(), foreign_clock_ds))
                    .with_resolved_clock(foreign_clock_ds),
            ];
        let port = FakePort::new();

        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            &port,
            FakeTimerHost::new(),
            0,
        ));

        let state = master.process_general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42.into(),
            foreign_clock_ds,
        )));

        assert!(matches!(state, PortState::Uncalibrated(_)));
    }

    #[test]
    fn master_port_stays_master_on_subsequent_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records =
            [
                ForeignClockRecord::new(AnnounceMessage::new(41.into(), foreign_clock_ds))
                    .with_resolved_clock(foreign_clock_ds),
            ];
        let port = FakePort::new();

        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            &port,
            FakeTimerHost::new(),
            0,
        ));

        let state = master.process_general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42.into(),
            foreign_clock_ds,
        )));

        assert!(matches!(state, PortState::Master(_)));
    }

    #[test]
    fn master_port_stays_master_on_undecided_bmca() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let port = FakePort::new();

        // start with an empty BMCA so that a single first announce makes it undecided
        let master = MasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        ));

        let state = master.process_general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42.into(),
            foreign_clock_ds,
        )));

        assert!(matches!(state, PortState::Master(_)));
    }

    #[test]
    fn slave_port_schedules_initial_delay_cycle() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(&local_clock, bmca, &port, &timer_host, 0);
        let announce_receipt_timeout = DropTimeout::new(domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let _ = SlavePort::new(domain_port, announce_receipt_timeout);

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(0.into()))));
    }

    #[test]
    fn slave_port_schedules_next_delay_request() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(&local_clock, bmca, &port, &timer_host, 0);
        let announce_receipt_timeout = DropTimeout::new(domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let slave = SlavePort::new(domain_port, announce_receipt_timeout);

        timer_host.take_system_messages();

        slave.process_system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0.into())));

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(1.into()))));
    }

    #[test]
    fn slave_port_answers_delay_cycle_with_delay_request() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let timer_host = FakeTimerHost::new();
        let domain_port = DomainPort::new(&local_clock, bmca, &port, &timer_host, 0);
        let announce_receipt_timeout = DropTimeout::new(domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let slave = SlavePort::new(domain_port, announce_receipt_timeout);

        slave.process_system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0.into())));

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
        let domain_port = DomainPort::new(&local_clock, bmca, &port, timer_host, 0);
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let slave = SlavePort::new(
            domain_port,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = slave.process_system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn initializing_port_to_listening_transition() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let initializing = InitializingPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        ));

        let state = initializing.process_system_message(SystemMessage::Initialized);

        assert!(matches!(state, PortState::Listening(_)));
    }

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let listening = ListeningPort::new(
            domain_port,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = listening.process_system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let listening = ListeningPort::new(
            domain_port,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let state = listening.process_general_message(GeneralMessage::Announce(
            AnnounceMessage::new(0.into(), foreign_clock),
        ));

        assert!(matches!(state, PortState::Listening(_)));
        assert!(announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_port_to_pre_master_transition_on_two_announces() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let listening = ListeningPort::new(
            domain_port,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let state = listening.process_general_message(GeneralMessage::Announce(
            AnnounceMessage::new(0.into(), foreign_clock.clone()),
        ));
        let state = state.process_general_message(GeneralMessage::Announce(AnnounceMessage::new(
            1.into(),
            foreign_clock.clone(),
        )));

        assert!(matches!(state, PortState::PreMaster(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_port_schedules_announce_receipt_timeout() {
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
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let _ = ListeningPort::new(
            domain_port,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_to_uncalibrated_transition_() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let listening = ListeningPort::new(
            DomainPort::new(
                &local_clock,
                FullBmca::new(SortedForeignClockRecordsVec::new()),
                &port,
                FakeTimerHost::new(),
                0,
            ),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::high_grade_test_clock();

        let state = listening.process_general_message(GeneralMessage::Announce(
            AnnounceMessage::new(0.into(), foreign_clock),
        ));
        let state = state.process_general_message(GeneralMessage::Announce(AnnounceMessage::new(
            1.into(),
            foreign_clock,
        )));

        assert!(matches!(state, PortState::Uncalibrated(_)));
        assert!(announce_receipt_timeout.is_active());
    }

    #[test]
    fn pre_master_port_schedules_qualification_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let timer_host = FakeTimerHost::new();

        let _ = PreMasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            &timer_host,
            0,
        ));

        let messages = timer_host.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_port_to_master_transition_on_qualification_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let pre_master = PreMasterPort::new(DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        ));

        let state = pre_master.process_system_message(SystemMessage::QualificationTimeout);

        assert!(matches!(state, PortState::Master(_)));
    }

    #[test]
    fn uncalibrated_port_to_slave_transition_on_following_announce() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records =
            [
                ForeignClockRecord::new(AnnounceMessage::new(41.into(), foreign_clock_ds))
                    .with_resolved_clock(foreign_clock_ds),
            ];
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            &port,
            FakeTimerHost::new(),
            0,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let uncalibrated = UncalibratedPort::new(
            domain_port,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = uncalibrated.process_general_message(GeneralMessage::Announce(
            AnnounceMessage::new(42.into(), foreign_clock_ds),
        ));

        assert!(matches!(state, PortState::Slave(_)));
        assert!(announce_receipt_timeout.is_active());
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let port = FakePort::new();
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            &port,
            FakeTimerHost::new(),
            0,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let uncalibrated = UncalibratedPort::new(
            domain_port,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = uncalibrated.process_system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn announce_cycle_produces_announce_messages_with_monotonic_sequence_ids() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let mut cycle = AnnounceCycle::new(0.into());
        let msg1 = cycle.announce(&local_clock);
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
}
