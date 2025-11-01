use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{
    AnnounceMessage, DelayCycleMessage, EventMessage, GeneralMessage, SequenceId, SyncCycleMessage,
    SystemMessage,
};
use crate::port::{DropTimeout, PhysicalPort, Timeout};
use crate::sync::MasterEstimate;
use crate::time::TimeStamp;

pub enum PortState<P: PhysicalPort, B: Bmca> {
    Initializing(InitializingPort<P, B>),
    Listening(ListeningPort<P, B>),
    Slave(SlavePort<P, B>),
    Master(MasterPort<P, B>),
    PreMaster(PreMasterPort<P, B>),
    Uncalibrated(UncalibratedPort<P, B>),
}

impl<P: PhysicalPort, B: Bmca> PortState<P, B> {
    pub fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> Self {
        match self {
            PortState::Initializing(_) => self,
            PortState::Listening(_) => self,
            PortState::Slave(port) => port.event_message(msg, timestamp),
            PortState::Master(port) => port.event_message(msg, timestamp),
            PortState::PreMaster(_) => self,
            PortState::Uncalibrated(_) => self,
        }
    }

    pub fn general_message(self, msg: GeneralMessage) -> Self {
        match self {
            PortState::Initializing(_) => self,
            PortState::Listening(port) => port.general_message(msg),
            PortState::Slave(port) => port.general_message(msg),
            PortState::Master(port) => port.general_message(msg),
            PortState::PreMaster(_) => self,
            PortState::Uncalibrated(port) => port.general_message(msg),
        }
    }

    pub fn system_message(self, msg: SystemMessage) -> Self {
        match self {
            PortState::Initializing(port) => port.system_message(msg),
            PortState::Listening(port) => port.system_message(msg),
            PortState::Slave(port) => port.system_message(msg),
            PortState::Master(port) => port.system_message(msg),
            PortState::PreMaster(port) => port.system_message(msg),
            PortState::Uncalibrated(port) => port.system_message(msg),
        }
    }
}

pub struct InitializingPort<P: PhysicalPort, B: Bmca> {
    port: P,
    bmca: B,
}

impl<P: PhysicalPort, B: Bmca> InitializingPort<P, B> {
    pub fn new(port: P, bmca: B) -> Self {
        Self { port, bmca }
    }

    fn system_message(self, msg: SystemMessage) -> PortState<P, B> {
        match msg {
            SystemMessage::Initialized => {
                let announce_receipt_timeout = DropTimeout::new(self.port.timeout(
                    SystemMessage::AnnounceReceiptTimeout,
                    Duration::from_secs(5),
                ));

                PortState::Listening(ListeningPort::new(
                    self.port,
                    self.bmca,
                    announce_receipt_timeout,
                ))
            }
            _ => PortState::Initializing(self),
        }
    }
}

pub struct ListeningPort<P: PhysicalPort, B: Bmca> {
    port: P,
    bmca: B,
    announce_receipt_timeout: DropTimeout<P::Timeout>,
}

impl<P: PhysicalPort, B: Bmca> ListeningPort<P, B> {
    pub fn new(port: P, bmca: B, announce_receipt_timeout: DropTimeout<P::Timeout>) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
        }
    }

    fn general_message(mut self, msg: GeneralMessage) -> PortState<P, B> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));
                self.bmca.consider(msg);

                match self.bmca.recommendation(self.port.clock()) {
                    BmcaRecommendation::Undecided => PortState::Listening(self),
                    BmcaRecommendation::Master => {
                        PortState::PreMaster(PreMasterPort::new(self.port, self.bmca))
                    }
                    BmcaRecommendation::Slave => PortState::Uncalibrated(UncalibratedPort::new(
                        self.port,
                        self.bmca,
                        self.announce_receipt_timeout,
                    )),
                }
            }
            _ => PortState::Listening(self),
        }
    }

    fn system_message(self, msg: SystemMessage) -> PortState<P, B> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => {
                PortState::Master(MasterPort::new(self.port, self.bmca))
            }
            _ => PortState::Listening(self),
        }
    }
}

pub struct SlavePort<P: PhysicalPort, B: Bmca> {
    port: P,
    bmca: B,
    announce_receipt_timeout: DropTimeout<P::Timeout>,
    delay_cycle_timeout: DropTimeout<P::Timeout>,
    master_estimate: MasterEstimate,
}

impl<P: PhysicalPort, B: Bmca> SlavePort<P, B> {
    pub fn new(port: P, bmca: B, announce_receipt_timeout: DropTimeout<P::Timeout>) -> Self {
        let delay_cycle_timeout = DropTimeout::new(port.timeout(
            SystemMessage::DelayCycle(DelayCycleMessage::new(0.into())),
            Duration::ZERO,
        ));

        Self {
            port,
            bmca,
            announce_receipt_timeout,
            delay_cycle_timeout,
            master_estimate: MasterEstimate::new(),
        }
    }

    fn event_message(mut self, msg: EventMessage, timestamp: TimeStamp) -> PortState<P, B> {
        match msg {
            EventMessage::TwoStepSync(sync) => {
                if let Some(estimate) = self.master_estimate.ingest_two_step_sync(sync, timestamp) {
                    self.port.clock().discipline(estimate);
                }
            }
            _ => {}
        }

        PortState::Slave(self)
    }

    fn general_message(mut self, msg: GeneralMessage) -> PortState<P, B> {
        match msg {
            GeneralMessage::Announce(_) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));
            }
            GeneralMessage::FollowUp(follow_up) => {
                if let Some(estimate) = self.master_estimate.ingest_follow_up(follow_up) {
                    self.port.clock().discipline(estimate);
                }
            }
            GeneralMessage::DelayResp(resp) => {
                if let Some(estimate) = self.master_estimate.ingest_delay_response(resp) {
                    self.port.clock().discipline(estimate);
                }
            }
        }

        PortState::Slave(self)
    }

    fn system_message(mut self, msg: SystemMessage) -> PortState<P, B> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => {
                PortState::Master(MasterPort::new(self.port, self.bmca))
            }
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
                        self.port.clock().discipline(estimate);
                    }

                    PortState::Slave(self)
                }
                _ => PortState::Slave(self),
            },
            _ => PortState::Slave(self),
        }
    }
}

pub struct MasterPort<P: PhysicalPort, B: Bmca> {
    port: P,
    bmca: B,
    announce_send_timeout: DropTimeout<P::Timeout>,
    sync_cycle_timeout: DropTimeout<P::Timeout>,
    announce_cycle: AnnounceCycle,
}

impl<P: PhysicalPort, B: Bmca> MasterPort<P, B> {
    pub fn new(port: P, bmca: B) -> Self {
        let announce_send_timeout =
            DropTimeout::new(port.timeout(SystemMessage::AnnounceSendTimeout, Duration::ZERO));
        let sync_cycle_timeout = DropTimeout::new(port.timeout(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0.into())),
            Duration::ZERO,
        ));

        Self {
            port,
            bmca,
            announce_send_timeout,
            sync_cycle_timeout,
            announce_cycle: AnnounceCycle::new(0.into()),
        }
    }

    fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> PortState<P, B> {
        match msg {
            EventMessage::DelayReq(req) => self
                .port
                .send_general(GeneralMessage::DelayResp(req.response(timestamp))),
            _ => {}
        }

        PortState::Master(self)
    }

    fn general_message(mut self, _msg: GeneralMessage) -> PortState<P, B> {
        match _msg {
            GeneralMessage::Announce(msg) => {
                self.bmca.consider(msg);

                match self.bmca.recommendation(self.port.clock()) {
                    BmcaRecommendation::Undecided => PortState::Master(self),
                    BmcaRecommendation::Slave => {
                        let announce_receipt_timeout = DropTimeout::new(self.port.timeout(
                            SystemMessage::AnnounceReceiptTimeout,
                            Duration::from_secs(5),
                        ));
                        PortState::Uncalibrated(UncalibratedPort::new(
                            self.port,
                            self.bmca,
                            announce_receipt_timeout,
                        ))
                    }
                    BmcaRecommendation::Master => PortState::Master(self),
                }
            }
            _ => PortState::Master(self),
        }
    }

    fn system_message(mut self, msg: SystemMessage) -> PortState<P, B> {
        match msg {
            SystemMessage::AnnounceSendTimeout => {
                let announce_message = self.announce_cycle.announce(&self.port.clock());
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

pub struct PreMasterPort<P: PhysicalPort, B: Bmca> {
    port: P,
    bmca: B,
    _qualification_timeout: DropTimeout<P::Timeout>,
}

impl<P: PhysicalPort, B: Bmca> PreMasterPort<P, B> {
    pub fn new(port: P, bmca: B) -> Self {
        let _qualification_timeout = DropTimeout::new(
            port.timeout(SystemMessage::QualificationTimeout, Duration::from_secs(5)),
        );
        Self {
            port,
            bmca,
            _qualification_timeout,
        }
    }

    fn system_message(self, msg: SystemMessage) -> PortState<P, B> {
        match msg {
            SystemMessage::QualificationTimeout => {
                PortState::Master(MasterPort::new(self.port, self.bmca))
            }
            _ => PortState::PreMaster(self),
        }
    }
}

pub struct UncalibratedPort<P: PhysicalPort, B: Bmca> {
    port: P,
    bmca: B,
    announce_receipt_timeout: DropTimeout<P::Timeout>,
}

impl<P: PhysicalPort, B: Bmca> UncalibratedPort<P, B> {
    pub fn new(port: P, bmca: B, announce_receipt_timeout: DropTimeout<P::Timeout>) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
        }
    }

    fn general_message(mut self, msg: GeneralMessage) -> PortState<P, B> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));

                self.bmca.consider(msg);

                match self.bmca.recommendation(self.port.clock()) {
                    BmcaRecommendation::Undecided => {
                        let announce_receipt_timeout = DropTimeout::new(self.port.timeout(
                            SystemMessage::AnnounceReceiptTimeout,
                            Duration::from_secs(5),
                        ));

                        return PortState::Listening(ListeningPort::new(
                            self.port,
                            self.bmca,
                            announce_receipt_timeout,
                        ));
                    }
                    BmcaRecommendation::Slave => {
                        return PortState::Slave(SlavePort::new(
                            self.port,
                            self.bmca,
                            self.announce_receipt_timeout,
                        ));
                    }
                    BmcaRecommendation::Master => {
                        return PortState::PreMaster(PreMasterPort::new(self.port, self.bmca));
                    }
                }
            }
            _ => PortState::Uncalibrated(self),
        }
    }

    fn system_message(self, msg: SystemMessage) -> PortState<P, B> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => {
                PortState::Master(MasterPort::new(self.port, self.bmca))
            }
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

    use std::rc::Rc;

    use crate::bmca::{ForeignClockDS, ForeignClockRecord, FullBmca, LocalClockDS};
    use crate::clock::{Clock, FakeClock};
    use crate::infra::infra_support::SortedForeignClockRecordsVec;
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::port::test_support::FakePort;

    #[test]
    fn slave_port_synchronizes_clock() {
        let local_clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));
        let port = FakePort::new(local_clock.clone(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let announce_receipt_timeout = DropTimeout::new(port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let mut slave = PortState::Slave(SlavePort::new(port, bmca, announce_receipt_timeout));

        slave = slave.event_message(
            EventMessage::TwoStepSync(TwoStepSyncMessage::new(0.into())),
            TimeStamp::new(1, 0),
        );
        slave = slave.general_message(GeneralMessage::FollowUp(FollowUpMessage::new(
            0.into(),
            TimeStamp::new(1, 0),
        )));
        slave = slave.system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(0.into())),
            timestamp: TimeStamp::new(0, 0),
        });
        let _ = slave.general_message(GeneralMessage::DelayResp(DelayResponseMessage::new(
            0.into(),
            TimeStamp::new(2, 0),
        )));

        assert_eq!(local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn master_port_answers_delay_request_with_delay_response() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        master.event_message(
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
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let _ = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0.into()))));
    }

    #[test]
    fn master_port_answers_sync_cycle_with_sync() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        master.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0.into())));

        let messages = port.take_event_messages();
        assert!(
            messages.contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(
                0.into()
            )))
        );
    }

    #[test]
    fn master_port_schedules_next_sync() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        // Drain messages that could have been sent during initialization.
        port.take_system_messages();

        master.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0.into())));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(1.into()))));
    }

    #[test]
    fn master_port_answers_timestamped_sync_with_follow_up() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        master.system_message(SystemMessage::Timestamp {
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
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let _ = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_port_schedules_next_announce() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        port.take_system_messages();

        master.system_message(SystemMessage::AnnounceSendTimeout);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_port_sends_announce_on_send_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        port.take_system_messages();

        master.system_message(SystemMessage::AnnounceSendTimeout);

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
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records =
            [
                ForeignClockRecord::new(AnnounceMessage::new(41.into(), foreign_clock_ds))
                    .with_resolved_clock(foreign_clock_ds),
            ];
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let master = MasterPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
        );

        let state = master.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42.into(),
            foreign_clock_ds,
        )));

        assert!(matches!(state, PortState::Uncalibrated(_)));
    }

    #[test]
    fn master_port_stays_master_on_subsequent_announce() {
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records =
            [
                ForeignClockRecord::new(AnnounceMessage::new(41.into(), foreign_clock_ds))
                    .with_resolved_clock(foreign_clock_ds),
            ];
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let master = MasterPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
        );

        let state = master.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42.into(),
            foreign_clock_ds,
        )));

        assert!(matches!(state, PortState::Master(_)));
    }

    #[test]
    fn master_port_stays_master_on_undecided_bmca() {
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        // start with an empty BMCA so that a single first announce makes it undecided
        let master = MasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        let state = master.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42.into(),
            foreign_clock_ds,
        )));

        assert!(matches!(state, PortState::Master(_)));
    }

    #[test]
    fn slave_port_schedules_initial_delay_cycle() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let announce_receipt_timeout = DropTimeout::new(port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let _ = SlavePort::new(&port, bmca, announce_receipt_timeout);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(0.into()))));
    }

    #[test]
    fn slave_port_schedules_next_delay_request() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let announce_receipt_timeout = DropTimeout::new(port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let slave = SlavePort::new(&port, bmca, announce_receipt_timeout);

        port.take_system_messages();

        slave.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0.into())));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(1.into()))));
    }

    #[test]
    fn slave_port_answers_delay_cycle_with_delay_request() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let announce_receipt_timeout = DropTimeout::new(port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        ));

        let slave = SlavePort::new(&port, bmca, announce_receipt_timeout);

        port.take_system_messages();

        slave.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0.into())));

        let events = port.take_event_messages();
        assert!(events.contains(&EventMessage::DelayReq(DelayRequestMessage::new(0.into()))));
    }

    #[test]
    fn slave_port_to_master_transition_on_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let slave = SlavePort::new(
            &port,
            bmca,
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = slave.system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn initializing_port_to_listening_transition() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let initializing =
            InitializingPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        let state = initializing.system_message(SystemMessage::Initialized);

        assert!(matches!(state, PortState::Listening(_)));
    }

    #[test]
    fn listening_port_to_master_transition_on_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let listening = ListeningPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = listening.system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(state, PortState::Master(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_port_stays_in_listening_on_single_announce() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let listening = ListeningPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let state = listening.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            0.into(),
            foreign_clock,
        )));

        assert!(matches!(state, PortState::Listening(_)));
        assert!(announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_port_to_pre_master_transition_on_two_announces() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let listening = ListeningPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let state = listening.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            0.into(),
            foreign_clock.clone(),
        )));
        let state = state.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            1.into(),
            foreign_clock.clone(),
        )));

        assert!(matches!(state, PortState::PreMaster(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_port_schedules_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let _ = ListeningPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            DropTimeout::new(port.timeout(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            )),
        );

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_port_to_uncalibrated_transition_() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let listening = ListeningPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::high_grade_test_clock();

        let state = listening.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            0.into(),
            foreign_clock,
        )));
        let state = state.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            1.into(),
            foreign_clock,
        )));

        assert!(matches!(state, PortState::Uncalibrated(_)));
        assert!(announce_receipt_timeout.is_active());
    }

    #[test]
    fn pre_master_port_schedules_qualification_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let _ = PreMasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_port_to_master_transition_on_qualification_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let pre_master =
            PreMasterPort::new(&port, FullBmca::new(SortedForeignClockRecordsVec::new()));

        let state = pre_master.system_message(SystemMessage::QualificationTimeout);

        assert!(matches!(state, PortState::Master(_)));
    }

    #[test]
    fn uncalibrated_port_to_slave_transition_on_following_announce() {
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();

        let prior_records =
            [
                ForeignClockRecord::new(AnnounceMessage::new(41.into(), foreign_clock_ds))
                    .with_resolved_clock(foreign_clock_ds),
            ];
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let uncalibrated = UncalibratedPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::from_records(&prior_records)),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = uncalibrated.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42.into(),
            foreign_clock_ds,
        )));

        assert!(matches!(state, PortState::Slave(_)));
        assert!(announce_receipt_timeout.is_active());
    }

    #[test]
    fn uncalibrated_port_to_master_on_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let announce_receipt_timeout = port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let uncalibrated = UncalibratedPort::new(
            &port,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let state = uncalibrated.system_message(SystemMessage::AnnounceReceiptTimeout);

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
