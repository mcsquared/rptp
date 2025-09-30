use std::time::Duration;

use crate::bmca::BestForeignClock;
use crate::message::{
    AnnounceCycleMessage, DelayCycleMessage, EventMessage, GeneralMessage, SyncCycleMessage,
    SystemMessage,
};
use crate::port::Port;
use crate::time::TimeStamp;

pub enum NodeState<P>
where
    P: Port,
{
    Initializing(InitializingNode<P>),
    Listening(ListeningNode<P>),
    Slave(SlaveNode<P>),
    Master(MasterNode<P>),
    PreMaster(PreMasterNode<P>),
}

impl<P> NodeState<P>
where
    P: Port,
{
    pub fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> Self {
        match self {
            NodeState::Initializing(_) => self,
            NodeState::Listening(_) => self,
            NodeState::Slave(node) => node.event_message(msg, timestamp),
            NodeState::Master(node) => node.event_message(msg, timestamp),
            NodeState::PreMaster(_) => self,
        }
    }

    pub fn general_message(self, msg: GeneralMessage) -> Self {
        match self {
            NodeState::Initializing(_) => self,
            NodeState::Listening(node) => node.general_message(msg),
            NodeState::Slave(node) => node.general_message(msg),
            NodeState::Master(node) => node.general_message(msg),
            NodeState::PreMaster(_) => self,
        }
    }

    pub fn system_message(self, msg: SystemMessage) -> Self {
        match self {
            NodeState::Initializing(node) => node.system_message(msg),
            NodeState::Listening(node) => node.system_message(msg),
            NodeState::Slave(node) => node.system_message(msg),
            NodeState::Master(node) => node.system_message(msg),
            NodeState::PreMaster(node) => node.system_message(msg),
        }
    }
}

pub struct InitializingNode<P>
where
    P: Port,
{
    port: P,
}

impl<P> InitializingNode<P>
where
    P: Port,
{
    pub fn new(port: P) -> Self {
        Self { port }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::Initialized => {
                NodeState::Listening(ListeningNode::new(self.port, BestForeignClock::new()))
            }
            _ => NodeState::Initializing(self),
        }
    }
}

pub struct ListeningNode<P>
where
    P: Port,
{
    port: P,
    best_foreign: BestForeignClock,
}

impl<P> ListeningNode<P>
where
    P: Port,
{
    pub fn new(port: P, best_foreign: BestForeignClock) -> Self {
        port.schedule(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        Self { port, best_foreign }
    }

    fn general_message(self, msg: GeneralMessage) -> NodeState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.best_foreign.consider(msg);
                if let Some(best_foreign) = self.best_foreign.best() {
                    if best_foreign.outranks_local(&self.port.clock()) {
                        NodeState::Listening(self) // TODO: implement transition to Uncalibrated as pre-stage to Slave
                    } else {
                        NodeState::PreMaster(PreMasterNode::new(self.port))
                    }
                } else {
                    NodeState::Listening(self)
                }
            }
            _ => NodeState::Listening(self),
        }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => NodeState::Master(MasterNode::new(self.port)),
            _ => NodeState::Listening(self),
        }
    }
}

pub struct SlaveNode<P>
where
    P: Port,
{
    port: P,
}

impl<P> SlaveNode<P>
where
    P: Port,
{
    pub fn new(port: P) -> Self {
        port.schedule(
            SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)),
            Duration::ZERO,
        );
        port.schedule(
            SystemMessage::DelayCycle(DelayCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self { port }
    }

    fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> NodeState<P> {
        match msg {
            EventMessage::TwoStepSync(sync) => {
                self.port.clock().ingest_two_step_sync(sync, timestamp);
            }
            _ => {}
        }

        NodeState::Slave(self)
    }

    fn general_message(self, msg: GeneralMessage) -> NodeState<P> {
        match msg {
            GeneralMessage::Announce(_) => {}
            GeneralMessage::FollowUp(follow_up) => {
                self.port.clock().ingest_follow_up(follow_up);
            }
            GeneralMessage::DelayResp(resp) => {
                self.port.clock().ingest_delay_response(resp);
            }
        }

        NodeState::Slave(self)
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::AnnounceCycle(announce_cycle) => {
                let announce_message = announce_cycle.announce();
                let next_cycle = announce_cycle.next();

                self.port
                    .send_general(GeneralMessage::Announce(announce_message));
                self.port.schedule(
                    SystemMessage::AnnounceCycle(next_cycle),
                    Duration::from_secs(1),
                );
            }
            SystemMessage::DelayCycle(delay_cycle) => {
                let delay_request = delay_cycle.delay_request();
                let next_cycle = delay_cycle.next();

                self.port.send_event(EventMessage::DelayReq(delay_request));
                self.port.schedule(
                    SystemMessage::DelayCycle(next_cycle),
                    Duration::from_secs(1),
                );
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::DelayReq(req) => {
                    self.port.clock().ingest_delay_request(req, timestamp);
                }
                _ => {}
            },
            _ => {}
        }

        NodeState::Slave(self)
    }
}

pub struct MasterNode<P>
where
    P: Port,
{
    port: P,
}

impl<P> MasterNode<P>
where
    P: Port,
{
    pub fn new(port: P) -> Self {
        port.schedule(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self { port }
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

    fn general_message(self, _msg: GeneralMessage) -> NodeState<P> {
        match _msg {
            _ => {}
        }

        NodeState::Master(self)
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::SyncCycle(sync_cycle) => {
                let sync_message = sync_cycle.two_step_sync();
                let next_cycle = sync_cycle.next();

                self.port
                    .send_event(EventMessage::TwoStepSync(sync_message));
                self.port
                    .schedule(SystemMessage::SyncCycle(next_cycle), Duration::from_secs(1));
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

pub struct PreMasterNode<P>
where
    P: Port,
{
    port: P,
}

impl<P> PreMasterNode<P>
where
    P: Port,
{
    pub fn new(port: P) -> Self {
        port.schedule(SystemMessage::QualificationTimeout, Duration::from_secs(2));

        Self { port }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::QualificationTimeout => NodeState::Master(MasterNode::new(self.port)),
            _ => NodeState::PreMaster(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::clock::{Clock, FakeClock, LocalClock, SynchronizableClock};
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::time::TimeStamp;

    #[test]
    fn slave_node_synchronizes_clock() {
        let local_clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));

        let mut slave = NodeState::Slave(SlaveNode::new(FakePortIo::new(local_clock.clone())));

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
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        node.event_message(
            EventMessage::DelayReq(DelayRequestMessage::new(0)),
            TimeStamp::new(0, 0),
        );

        assert!(
            port.general_messages
                .borrow()
                .contains(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                    0,
                    TimeStamp::new(0, 0)
                )))
        );
    }

    #[test]
    fn master_node_schedules_initial_sync() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = MasterNode::new(&port);

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0)))
        );
    }

    #[test]
    fn master_node_answers_sync_cycle_with_sync() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        assert!(
            port.event_messages
                .borrow()
                .contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)))
        );
    }

    #[test]
    fn master_node_schedules_next_sync() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(1)))
        );
    }

    #[test]
    fn master_node_answers_timestamped_sync_with_follow_up() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        node.system_message(SystemMessage::Timestamp {
            msg: EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)),
            timestamp: TimeStamp::new(0, 0),
        });

        assert!(
            port.general_messages
                .borrow()
                .contains(&GeneralMessage::FollowUp(FollowUpMessage::new(
                    0,
                    TimeStamp::new(0, 0)
                )))
        );
    }

    #[test]
    fn slave_node_schedules_initial_announce_cycle() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = SlaveNode::new(&port);

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)))
        );
    }

    #[test]
    fn slave_node_schedules_next_announce() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = SlaveNode::new(&port);

        node.system_message(SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)));

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(1)))
        );
    }

    #[test]
    fn slave_node_answers_announce_cycle_with_announce() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = SlaveNode::new(&port);

        node.system_message(SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)));

        assert!(
            port.general_messages
                .borrow()
                .contains(&GeneralMessage::Announce(AnnounceMessage::new(0)))
        );
    }

    #[test]
    fn slave_node_schedules_initial_delay_cycle() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = SlaveNode::new(&port);

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(0)))
        );
    }

    #[test]
    fn slave_node_schedules_next_delay_request() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = SlaveNode::new(&port);

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(1)))
        );
    }

    #[test]
    fn slave_node_answers_delay_request_cycle_with_delay_request() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = SlaveNode::new(&port);

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        assert!(
            port.event_messages
                .borrow()
                .contains(&EventMessage::DelayReq(DelayRequestMessage::new(0)))
        );
    }

    #[test]
    fn initializing_node_to_listening_transition() {
        let node = InitializingNode::new(FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0))));

        let node = node.system_message(SystemMessage::Initialized);

        match node {
            NodeState::Listening(_) => {}
            _ => panic!("Expected Listening state"),
        }
    }

    #[test]
    fn listening_node_to_master_transition_on_announce_receipt_timeout() {
        let node = ListeningNode::new(
            FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0))),
            BestForeignClock::new(),
        );

        let node = node.system_message(SystemMessage::AnnounceReceiptTimeout);

        match node {
            NodeState::Master(_) => {}
            _ => panic!("Expected Master state"),
        }
    }

    #[test]
    fn listening_node_stays_in_listening_on_single_announce() {
        let node = ListeningNode::new(
            FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0))),
            BestForeignClock::new(),
        );

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(0)));

        match node {
            NodeState::Listening(_) => {}
            _ => panic!("Expected Listening state"),
        }
    }

    #[test]
    fn listening_node_to_pre_master_transition_on_two_announces() {
        let node = ListeningNode::new(
            FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0))),
            BestForeignClock::new(),
        );

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(0)));
        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(1)));

        match node {
            NodeState::PreMaster(_) => {}
            _ => panic!("Expected PreMaster state"),
        }
    }

    #[test]
    fn listening_node_schedules_announce_receipt_timeout() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = ListeningNode::new(&port, BestForeignClock::new());

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::AnnounceReceiptTimeout)
        );
    }

    #[test]
    fn pre_master_node_schedules_qualification_timeout() {
        let port = FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = PreMasterNode::new(&port);

        assert!(
            port.system_messages
                .borrow()
                .contains(&SystemMessage::QualificationTimeout)
        );
    }

    #[test]
    fn pre_master_node_to_master_transition_on_qualification_timeout() {
        let node = PreMasterNode::new(FakePortIo::new(FakeClock::new(TimeStamp::new(0, 0))));

        let node = node.system_message(SystemMessage::QualificationTimeout);

        match node {
            NodeState::Master(_) => {}
            _ => panic!("Expected Master state"),
        }
    }

    struct FakePortIo<C: SynchronizableClock> {
        clock: LocalClock<C>,
        event_messages: RefCell<Vec<EventMessage>>,
        general_messages: RefCell<Vec<GeneralMessage>>,
        system_messages: RefCell<Vec<SystemMessage>>,
    }

    impl<C: SynchronizableClock> FakePortIo<C> {
        pub fn new(clock: C) -> Self {
            Self {
                clock: LocalClock::new(clock),
                event_messages: RefCell::new(Vec::new()),
                general_messages: RefCell::new(Vec::new()),
                system_messages: RefCell::new(Vec::new()),
            }
        }
    }

    impl<C: SynchronizableClock> Port for FakePortIo<C> {
        type Clock = C;

        fn clock(&self) -> &LocalClock<Self::Clock> {
            &self.clock
        }

        fn send_event(&self, msg: EventMessage) {
            self.event_messages.borrow_mut().push(msg);
        }

        fn send_general(&self, msg: GeneralMessage) {
            self.general_messages.borrow_mut().push(msg);
        }

        fn schedule(&self, msg: SystemMessage, _delay: Duration) {
            self.system_messages.borrow_mut().push(msg);
        }
    }

    impl Port for &FakePortIo<FakeClock> {
        type Clock = FakeClock;

        fn clock(&self) -> &LocalClock<Self::Clock> {
            &self.clock
        }

        fn send_event(&self, msg: EventMessage) {
            self.event_messages.borrow_mut().push(msg);
        }

        fn send_general(&self, msg: GeneralMessage) {
            self.general_messages.borrow_mut().push(msg);
        }

        fn schedule(&self, msg: SystemMessage, _delay: Duration) {
            self.system_messages.borrow_mut().push(msg);
        }
    }
}
