use std::time::Duration;

use crate::bmca::{Bmca, BmcaRecommendation};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::message::{
    AnnounceMessage, DelayCycleMessage, EventMessage, GeneralMessage, MasterEstimate,
    SyncCycleMessage, SystemMessage,
};
use crate::port::{Port, Timeout};
use crate::time::TimeStamp;

pub enum NodeState<P: Port> {
    Initializing(InitializingNode<P>),
    Listening(ListeningNode<P>),
    Slave(SlaveNode<P>),
    Master(MasterNode<P>),
    PreMaster(PreMasterNode<P>),
    Uncalibrated(UncalibratedNode<P>),
}

impl<P: Port> NodeState<P> {
    pub fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> Self {
        match self {
            NodeState::Initializing(_) => self,
            NodeState::Listening(_) => self,
            NodeState::Slave(node) => node.event_message(msg, timestamp),
            NodeState::Master(node) => node.event_message(msg, timestamp),
            NodeState::PreMaster(_) => self,
            NodeState::Uncalibrated(_) => self,
        }
    }

    pub fn general_message(self, msg: GeneralMessage) -> Self {
        match self {
            NodeState::Initializing(_) => self,
            NodeState::Listening(node) => node.general_message(msg),
            NodeState::Slave(node) => node.general_message(msg),
            NodeState::Master(node) => node.general_message(msg),
            NodeState::PreMaster(_) => self,
            NodeState::Uncalibrated(node) => node.general_message(msg),
        }
    }

    pub fn system_message(self, msg: SystemMessage) -> Self {
        match self {
            NodeState::Initializing(node) => node.system_message(msg),
            NodeState::Listening(node) => node.system_message(msg),
            NodeState::Slave(node) => node.system_message(msg),
            NodeState::Master(node) => node.system_message(msg),
            NodeState::PreMaster(node) => node.system_message(msg),
            NodeState::Uncalibrated(_) => self,
        }
    }
}

pub struct InitializingNode<P: Port> {
    port: P,
}

impl<P: Port> InitializingNode<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::Initialized => {
                let bmca = Bmca::new(self.port.foreign_clock_records(&[]));
                let announce_receipt_timeout = DropTimeout::new(self.port.schedule(
                    SystemMessage::AnnounceReceiptTimeout,
                    Duration::from_secs(5),
                ));

                NodeState::Listening(ListeningNode::new(
                    self.port,
                    bmca,
                    announce_receipt_timeout,
                ))
            }
            _ => NodeState::Initializing(self),
        }
    }
}

pub struct ListeningNode<P: Port> {
    port: P,
    bmca: Bmca<P::ClockRecords>,
    announce_receipt_timeout: DropTimeout<P::Timeout>,
}

impl<P: Port> ListeningNode<P> {
    pub fn new(
        port: P,
        bmca: Bmca<P::ClockRecords>,
        announce_receipt_timeout: DropTimeout<P::Timeout>,
    ) -> Self {
        Self {
            port,
            bmca,
            announce_receipt_timeout,
        }
    }

    fn general_message(mut self, msg: GeneralMessage) -> NodeState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));
                self.bmca.consider(msg);

                match self.bmca.recommendation(self.port.clock()) {
                    BmcaRecommendation::Undecided => NodeState::Listening(self),
                    BmcaRecommendation::Master => {
                        NodeState::PreMaster(PreMasterNode::new(self.port, self.bmca))
                    }
                    BmcaRecommendation::Slave => {
                        NodeState::Uncalibrated(UncalibratedNode::new(self.port, self.bmca))
                    }
                }
            }
            _ => NodeState::Listening(self),
        }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => {
                NodeState::Master(MasterNode::new(self.port, self.bmca))
            }
            _ => NodeState::Listening(self),
        }
    }
}

pub struct SlaveNode<P: Port> {
    port: P,
    delay_cycle_timeout: P::Timeout,
    master_estimate: MasterEstimate,
}

impl<P: Port> SlaveNode<P> {
    pub fn new(port: P) -> Self {
        let delay_cycle_timeout = port.schedule(
            SystemMessage::DelayCycle(DelayCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self {
            port,
            delay_cycle_timeout,
            master_estimate: MasterEstimate::new(),
        }
    }

    fn event_message(mut self, msg: EventMessage, timestamp: TimeStamp) -> NodeState<P> {
        match msg {
            EventMessage::TwoStepSync(sync) => {
                if let Some(estimate) = self.master_estimate.ingest_two_step_sync(sync, timestamp) {
                    self.port.clock().discipline(estimate);
                }
            }
            _ => {}
        }

        NodeState::Slave(self)
    }

    fn general_message(mut self, msg: GeneralMessage) -> NodeState<P> {
        match msg {
            GeneralMessage::Announce(_) => {}
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

        NodeState::Slave(self)
    }

    fn system_message(mut self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::DelayCycle(delay_cycle) => {
                let delay_request = delay_cycle.delay_request();
                let next_cycle = delay_cycle.next();

                self.port.send_event(EventMessage::DelayReq(delay_request));
                self.delay_cycle_timeout.restart_with(
                    SystemMessage::DelayCycle(next_cycle),
                    Duration::from_secs(1),
                );
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::DelayReq(req) => {
                    if let Some(estimate) =
                        self.master_estimate.ingest_delay_request(req, timestamp)
                    {
                        self.port.clock().discipline(estimate);
                    }
                }
                _ => {}
            },
            _ => {}
        }

        NodeState::Slave(self)
    }
}

pub struct MasterNode<P: Port> {
    port: P,
    announce_send_timeout: P::Timeout,
    sync_cycle_timeout: P::Timeout,
    announce_cycle: AnnounceCycle,
    bmca: Bmca<P::ClockRecords>,
}

impl<P: Port> MasterNode<P> {
    pub fn new(port: P, bmca: Bmca<P::ClockRecords>) -> Self {
        let announce_send_timeout =
            port.schedule(SystemMessage::AnnounceSendTimeout, Duration::ZERO);
        let sync_cycle_timeout = port.schedule(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self {
            port,
            announce_send_timeout,
            sync_cycle_timeout,
            announce_cycle: AnnounceCycle::new(0),
            bmca,
        }
    }

    fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> NodeState<P> {
        match msg {
            EventMessage::DelayReq(req) => self
                .port
                .send_general(GeneralMessage::DelayResp(req.response(timestamp))),
            _ => {}
        }

        NodeState::Master(self)
    }

    fn general_message(mut self, _msg: GeneralMessage) -> NodeState<P> {
        match _msg {
            GeneralMessage::Announce(msg) => {
                self.bmca.consider(msg);

                match self.bmca.recommendation(self.port.clock()) {
                    BmcaRecommendation::Undecided => NodeState::Master(self),
                    BmcaRecommendation::Slave => {
                        NodeState::Uncalibrated(UncalibratedNode::new(self.port, self.bmca))
                    }
                    BmcaRecommendation::Master => NodeState::Master(self),
                }
            }
            _ => NodeState::Master(self),
        }
    }

    fn system_message(mut self, msg: SystemMessage) -> NodeState<P> {
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

        NodeState::Master(self)
    }
}

pub struct PreMasterNode<P: Port> {
    port: P,
    bmca: Bmca<P::ClockRecords>,
    _qualification_timeout: P::Timeout,
}

impl<P: Port> PreMasterNode<P> {
    pub fn new(port: P, bmca: Bmca<P::ClockRecords>) -> Self {
        let _qualification_timeout =
            port.schedule(SystemMessage::QualificationTimeout, Duration::from_secs(5));
        Self {
            port,
            bmca,
            _qualification_timeout,
        }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::QualificationTimeout => {
                NodeState::Master(MasterNode::new(self.port, self.bmca))
            }
            _ => NodeState::PreMaster(self),
        }
    }
}

pub struct UncalibratedNode<P: Port> {
    port: P,
    bmca: Bmca<P::ClockRecords>,
}

impl<P: Port> UncalibratedNode<P> {
    pub fn new(port: P, bmca: Bmca<P::ClockRecords>) -> Self {
        Self { port, bmca }
    }

    fn general_message(mut self, msg: GeneralMessage) -> NodeState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.bmca.consider(msg);

                match self.bmca.recommendation(self.port.clock()) {
                    BmcaRecommendation::Undecided => {
                        let announce_receipt_timeout = DropTimeout::new(self.port.schedule(
                            SystemMessage::AnnounceReceiptTimeout,
                            Duration::from_secs(5),
                        ));

                        return NodeState::Listening(ListeningNode::new(
                            self.port,
                            self.bmca,
                            announce_receipt_timeout,
                        ));
                    }
                    BmcaRecommendation::Slave => {
                        return NodeState::Slave(SlaveNode::new(self.port));
                    }
                    BmcaRecommendation::Master => {
                        return NodeState::PreMaster(PreMasterNode::new(self.port, self.bmca));
                    }
                }
            }
            _ => NodeState::Uncalibrated(self),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnounceCycle {
    sequence_id: u16,
}

impl AnnounceCycle {
    pub fn new(start: u16) -> Self {
        Self { sequence_id: start }
    }

    pub fn announce<C: SynchronizableClock>(
        &mut self,
        local_clock: &LocalClock<C>,
    ) -> AnnounceMessage {
        let msg = local_clock.announce(self.sequence_id);
        self.sequence_id = self.sequence_id.wrapping_add(1);
        msg
    }
}

pub struct DropTimeout<T: Timeout> {
    inner: T,
}

impl<T: Timeout> DropTimeout<T> {
    fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T: Timeout> Timeout for DropTimeout<T> {
    fn restart(&self, timeout: Duration) {
        self.inner.restart(timeout);
    }

    fn restart_with(&self, msg: SystemMessage, timeout: Duration) {
        self.inner.restart_with(msg, timeout);
    }

    fn cancel(&self) {
        self.inner.cancel();
    }
}

impl<T: Timeout> Drop for DropTimeout<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::rc::Rc;

    use crate::bmca::{ForeignClockDS, ForeignClockRecord, LocalClockDS};
    use crate::clock::{Clock, FakeClock};
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::port::test_support::FakePort;

    #[test]
    fn slave_node_synchronizes_clock() {
        let local_clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));

        let mut slave = NodeState::Slave(SlaveNode::new(FakePort::new(
            local_clock.clone(),
            LocalClockDS::mid_grade_test_clock(),
        )));

        slave = slave.event_message(
            EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)),
            TimeStamp::new(1, 0),
        );
        slave = slave.general_message(GeneralMessage::FollowUp(FollowUpMessage::new(
            0,
            TimeStamp::new(1, 0),
        )));
        slave = slave.system_message(SystemMessage::Timestamp {
            msg: EventMessage::DelayReq(DelayRequestMessage::new(0)),
            timestamp: TimeStamp::new(0, 0),
        });
        let _ = slave.general_message(GeneralMessage::DelayResp(DelayResponseMessage::new(
            0,
            TimeStamp::new(2, 0),
        )));

        assert_eq!(local_clock.now(), TimeStamp::new(2, 0));
    }

    #[test]
    fn master_node_answers_delay_request_with_delay_response() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        node.event_message(
            EventMessage::DelayReq(DelayRequestMessage::new(0)),
            TimeStamp::new(0, 0),
        );

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                0,
                TimeStamp::new(0, 0)
            )))
        );
    }

    #[test]
    fn master_node_schedules_initial_sync() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let _ = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0))));
    }

    #[test]
    fn master_node_answers_sync_cycle_with_sync() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        let messages = port.take_event_messages();
        assert!(messages.contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(0))));
    }

    #[test]
    fn master_node_schedules_next_sync() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        // Drain messages that could have been sent during initialization.
        port.take_system_messages();

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(1))));
    }

    #[test]
    fn master_node_answers_timestamped_sync_with_follow_up() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        node.system_message(SystemMessage::Timestamp {
            msg: EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)),
            timestamp: TimeStamp::new(0, 0),
        });

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::FollowUp(FollowUpMessage::new(
                0,
                TimeStamp::new(0, 0)
            )))
        );
    }

    #[test]
    fn master_node_schedules_initial_announce() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let _ = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_node_schedules_next_announce() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        port.take_system_messages();

        node.system_message(SystemMessage::AnnounceSendTimeout);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceSendTimeout));
    }

    #[test]
    fn master_node_sends_announce_on_send_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        port.take_system_messages();

        node.system_message(SystemMessage::AnnounceSendTimeout);

        let messages = port.take_general_messages();
        assert!(
            messages.contains(&GeneralMessage::Announce(AnnounceMessage::new(
                0,
                ForeignClockDS::high_grade_test_clock()
            )))
        );
    }

    #[test]
    fn master_node_to_uncalibrated_transition_on_following_announce() {
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();
        let prior_records = [
            ForeignClockRecord::new(AnnounceMessage::new(41, foreign_clock_ds))
                .with_resolved_clock(foreign_clock_ds),
        ];
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&prior_records)));

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42,
            foreign_clock_ds,
        )));

        assert!(matches!(node, NodeState::Uncalibrated(_)));
    }

    #[test]
    fn master_node_stays_master_on_subsequent_announce() {
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let prior_records = [
            ForeignClockRecord::new(AnnounceMessage::new(41, foreign_clock_ds))
                .with_resolved_clock(foreign_clock_ds),
        ];
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&prior_records)));

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42,
            foreign_clock_ds,
        )));

        assert!(matches!(node, NodeState::Master(_)));
    }

    #[test]
    fn master_node_stays_master_on_undecided_bmca() {
        let foreign_clock_ds = ForeignClockDS::low_grade_test_clock();
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        // start with an empty BMCA so that a single first announce makes it undecided
        let node = MasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42,
            foreign_clock_ds,
        )));

        assert!(matches!(node, NodeState::Master(_)));
    }

    #[test]
    fn slave_node_schedules_initial_delay_cycle() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let _ = SlaveNode::new(&port);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(0))));
    }

    #[test]
    fn slave_node_schedules_next_delay_request() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let node = SlaveNode::new(&port);

        port.take_system_messages();

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(1))));
    }

    #[test]
    fn slave_node_answers_delay_cycle_with_delay_request() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let node = SlaveNode::new(&port);

        port.take_system_messages();

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        let events = port.take_event_messages();
        assert!(events.contains(&EventMessage::DelayReq(DelayRequestMessage::new(0))));
    }

    #[test]
    fn initializing_node_to_listening_transition() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let node = InitializingNode::new(&port);

        let node = node.system_message(SystemMessage::Initialized);

        assert!(matches!(node, NodeState::Listening(_)));
    }

    #[test]
    fn listening_node_to_master_transition_on_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let announce_receipt_timeout = port.schedule(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let node = ListeningNode::new(
            &port,
            Bmca::new(port.foreign_clock_records(&[])),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let node = node.system_message(SystemMessage::AnnounceReceiptTimeout);

        assert!(matches!(node, NodeState::Master(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_node_stays_in_listening_on_single_announce() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let announce_receipt_timeout = port.schedule(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let node = ListeningNode::new(
            &port,
            Bmca::new(port.foreign_clock_records(&[])),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            0,
            foreign_clock,
        )));

        assert!(matches!(node, NodeState::Listening(_)));
        assert!(announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_node_to_pre_master_transition_on_two_announces() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let announce_receipt_timeout = port.schedule(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );
        let node = ListeningNode::new(
            &port,
            Bmca::new(port.foreign_clock_records(&[])),
            DropTimeout::new(announce_receipt_timeout.clone()),
        );

        let foreign_clock = ForeignClockDS::mid_grade_test_clock();

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            0,
            foreign_clock.clone(),
        )));
        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            1,
            foreign_clock.clone(),
        )));

        assert!(matches!(node, NodeState::PreMaster(_)));
        assert!(!announce_receipt_timeout.is_active());
    }

    #[test]
    fn listening_node_schedules_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());

        let _ = ListeningNode::new(
            &port,
            Bmca::new(port.foreign_clock_records(&[])),
            DropTimeout::new(port.schedule(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            )),
        );

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_node_to_uncalibrated_transition_() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let node = ListeningNode::new(
            &port,
            Bmca::new(port.foreign_clock_records(&[])),
            DropTimeout::new(port.schedule(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(5),
            )),
        );

        let foreign_clock = ForeignClockDS::high_grade_test_clock();

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            0,
            foreign_clock,
        )));
        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            1,
            foreign_clock,
        )));

        assert!(matches!(node, NodeState::Uncalibrated(_)));
    }

    #[test]
    fn pre_master_node_schedules_qualification_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let _ = PreMasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_node_to_master_transition_on_qualification_timeout() {
        let port = FakePort::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());
        let node = PreMasterNode::new(&port, Bmca::new(port.foreign_clock_records(&[])));

        let node = node.system_message(SystemMessage::QualificationTimeout);

        assert!(matches!(node, NodeState::Master(_)));
    }

    #[test]
    fn uncalibrated_node_to_slave_transition_on_following_announce() {
        let foreign_clock_ds = ForeignClockDS::high_grade_test_clock();

        let prior_records = [
            ForeignClockRecord::new(AnnounceMessage::new(41, foreign_clock_ds))
                .with_resolved_clock(foreign_clock_ds),
        ];
        let port = FakePort::new(FakeClock::default(), LocalClockDS::mid_grade_test_clock());
        let node =
            UncalibratedNode::new(&port, Bmca::new(port.foreign_clock_records(&prior_records)));

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(
            42,
            foreign_clock_ds,
        )));

        assert!(matches!(node, NodeState::Slave(_)));
    }

    #[test]
    fn announce_cycle_produces_announce_messages_with_monotonic_sequence_ids() {
        let local_clock =
            LocalClock::new(FakeClock::default(), LocalClockDS::high_grade_test_clock());

        let mut cycle = AnnounceCycle::new(0);
        let msg1 = cycle.announce(&local_clock);
        let msg2 = cycle.announce(&local_clock);

        assert_eq!(
            msg1,
            AnnounceMessage::new(0, ForeignClockDS::high_grade_test_clock())
        );
        assert_eq!(
            msg2,
            AnnounceMessage::new(1, ForeignClockDS::high_grade_test_clock())
        );
    }
}
