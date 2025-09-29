use std::time::Duration;

use crate::clock::{SynchronizableClock, SynchronizedClock};
use crate::message::{
    AnnounceCycleMessage, DelayCycleMessage, EventMessage, GeneralMessage, SyncCycleMessage,
    SystemMessage,
};
use crate::port::PortIo;
use crate::time::TimeStamp;

pub enum NodeState<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    Initializing(InitializingNode<C, P>),
    Listening(ListeningNode<C, P>),
    Slave(SlaveNode<C, P>),
    Master(MasterNode<C, P>),
    PreMaster(PreMasterNode<C, P>),
}

impl<C, P> NodeState<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
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

pub struct InitializingNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    clock: SynchronizedClock<C>,
    portio: P,
}

impl<C, P> InitializingNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    pub fn new(clock: SynchronizedClock<C>, portio: P) -> Self {
        Self { clock, portio }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, P> {
        match msg {
            SystemMessage::Initialized => {
                NodeState::Listening(ListeningNode::new(self.clock, self.portio, 0))
            }
            _ => NodeState::Initializing(self),
        }
    }
}

pub struct ListeningNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    clock: SynchronizedClock<C>,
    portio: P,
    announce_count: u8,
}

impl<C, P> ListeningNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    pub fn new(clock: SynchronizedClock<C>, portio: P, announce_count: u8) -> Self {
        portio.schedule(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        Self {
            clock,
            portio,
            announce_count,
        }
    }

    fn general_message(self, msg: GeneralMessage) -> NodeState<C, P> {
        match msg {
            GeneralMessage::Announce(_) => {
                if self.announce_count >= 1 {
                    NodeState::PreMaster(PreMasterNode::new(self.clock, self.portio))
                } else {
                    NodeState::Listening(ListeningNode::new(
                        self.clock,
                        self.portio,
                        self.announce_count + 1,
                    ))
                }
            }
            _ => NodeState::Listening(self),
        }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, P> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => {
                NodeState::Master(MasterNode::new(self.clock, self.portio))
            }
            _ => NodeState::Listening(self),
        }
    }
}

pub struct SlaveNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    clock: SynchronizedClock<C>,
    portio: P,
}

impl<C, P> SlaveNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    pub fn new(clock: SynchronizedClock<C>, portio: P) -> Self {
        portio.schedule(
            SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)),
            Duration::ZERO,
        );
        portio.schedule(
            SystemMessage::DelayCycle(DelayCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self { clock, portio }
    }

    fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> NodeState<C, P> {
        match msg {
            EventMessage::TwoStepSync(sync) => {
                self.clock.ingest_two_step_sync(sync, timestamp);
            }
            _ => {}
        }

        NodeState::Slave(self)
    }

    fn general_message(self, msg: GeneralMessage) -> NodeState<C, P> {
        match msg {
            GeneralMessage::Announce(_) => {}
            GeneralMessage::FollowUp(follow_up) => {
                self.clock.ingest_follow_up(follow_up);
            }
            GeneralMessage::DelayResp(resp) => {
                self.clock.ingest_delay_response(resp);
            }
        }

        NodeState::Slave(self)
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, P> {
        match msg {
            SystemMessage::AnnounceCycle(announce_cycle) => {
                let announce_message = announce_cycle.announce();
                let next_cycle = announce_cycle.next();

                self.portio
                    .send_general(GeneralMessage::Announce(announce_message));
                self.portio.schedule(
                    SystemMessage::AnnounceCycle(next_cycle),
                    Duration::from_secs(1),
                );
            }
            SystemMessage::DelayCycle(delay_cycle) => {
                let delay_request = delay_cycle.delay_request();
                let next_cycle = delay_cycle.next();

                self.portio
                    .send_event(EventMessage::DelayReq(delay_request));
                self.portio.schedule(
                    SystemMessage::DelayCycle(next_cycle),
                    Duration::from_secs(1),
                );
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::DelayReq(req) => {
                    self.clock.ingest_delay_request(req, timestamp);
                }
                _ => {}
            },
            _ => {}
        }

        NodeState::Slave(self)
    }
}

pub struct MasterNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    _clock: SynchronizedClock<C>,
    portio: P,
}

impl<C, P> MasterNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    pub fn new(_clock: SynchronizedClock<C>, portio: P) -> Self {
        portio.schedule(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self { _clock, portio }
    }

    fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> NodeState<C, P> {
        match msg {
            EventMessage::DelayReq(req) => self
                .portio
                .send_general(GeneralMessage::DelayResp(req.response(timestamp))),
            _ => {}
        }

        NodeState::Master(self)
    }

    fn general_message(self, _msg: GeneralMessage) -> NodeState<C, P> {
        match _msg {
            _ => {}
        }

        NodeState::Master(self)
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, P> {
        match msg {
            SystemMessage::SyncCycle(sync_cycle) => {
                let sync_message = sync_cycle.two_step_sync();
                let next_cycle = sync_cycle.next();

                self.portio
                    .send_event(EventMessage::TwoStepSync(sync_message));
                self.portio
                    .schedule(SystemMessage::SyncCycle(next_cycle), Duration::from_secs(1));
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::TwoStepSync(twostep) => {
                    self.portio
                        .send_general(GeneralMessage::FollowUp(twostep.follow_up(timestamp)));
                }
                _ => {}
            },
            _ => {}
        }

        NodeState::Master(self)
    }
}

pub struct PreMasterNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    clock: SynchronizedClock<C>,
    portio: P,
}

impl<C, P> PreMasterNode<C, P>
where
    C: SynchronizableClock,
    P: PortIo,
{
    pub fn new(clock: SynchronizedClock<C>, portio: P) -> Self {
        portio.schedule(SystemMessage::QualificationTimeout, Duration::from_secs(2));

        Self { clock, portio }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, P> {
        match msg {
            SystemMessage::QualificationTimeout => {
                NodeState::Master(MasterNode::new(self.clock, self.portio))
            }
            _ => NodeState::PreMaster(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::clock::{Clock, FakeClock};
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::time::TimeStamp;

    #[test]
    fn slave_node_synchronizes_clock() {
        let local_clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));

        let mut slave = NodeState::Slave(SlaveNode::new(
            SynchronizedClock::new(local_clock.clone()),
            FakePortIo::new(),
        ));

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
        let portio = FakePortIo::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.event_message(
            EventMessage::DelayReq(DelayRequestMessage::new(0)),
            TimeStamp::new(0, 0),
        );

        assert!(
            portio
                .general_messages
                .borrow()
                .contains(&GeneralMessage::DelayResp(DelayResponseMessage::new(
                    0,
                    TimeStamp::new(0, 0)
                )))
        );
    }

    #[test]
    fn master_node_schedules_initial_sync() {
        let portio = FakePortIo::new();

        let _ = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0)))
        );
    }

    #[test]
    fn master_node_answers_sync_cycle_with_sync() {
        let portio = FakePortIo::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        assert!(
            portio
                .event_messages
                .borrow()
                .contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)))
        );
    }

    #[test]
    fn master_node_schedules_next_sync() {
        let portio = FakePortIo::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(1)))
        );
    }

    #[test]
    fn master_node_answers_timestamped_sync_with_follow_up() {
        let portio = FakePortIo::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.system_message(SystemMessage::Timestamp {
            msg: EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)),
            timestamp: TimeStamp::new(0, 0),
        });

        assert!(
            portio
                .general_messages
                .borrow()
                .contains(&GeneralMessage::FollowUp(FollowUpMessage::new(
                    0,
                    TimeStamp::new(0, 0)
                )))
        );
    }

    #[test]
    fn slave_node_schedules_initial_announce_cycle() {
        let portio = FakePortIo::new();

        let _ = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)))
        );
    }

    #[test]
    fn slave_node_schedules_next_announce() {
        let portio = FakePortIo::new();

        let node = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.system_message(SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)));

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(1)))
        );
    }

    #[test]
    fn slave_node_answers_announce_cycle_with_announce() {
        let portio = FakePortIo::new();

        let node = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.system_message(SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)));

        assert!(
            portio
                .general_messages
                .borrow()
                .contains(&GeneralMessage::Announce(AnnounceMessage::new(0)))
        );
    }

    #[test]
    fn slave_node_schedules_initial_delay_cycle() {
        let portio = FakePortIo::new();

        let _ = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(0)))
        );
    }

    #[test]
    fn slave_node_schedules_next_delay_request() {
        let portio = FakePortIo::new();

        let node = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(1)))
        );
    }

    #[test]
    fn slave_node_answers_delay_request_cycle_with_delay_request() {
        let portio = FakePortIo::new();

        let node = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        assert!(
            portio
                .event_messages
                .borrow()
                .contains(&EventMessage::DelayReq(DelayRequestMessage::new(0)))
        );
    }

    #[test]
    fn initializing_node_to_listening_transition() {
        let node = InitializingNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakePortIo::new(),
        );

        let node = node.system_message(SystemMessage::Initialized);

        match node {
            NodeState::Listening(_) => {}
            _ => panic!("Expected Listening state"),
        }
    }

    #[test]
    fn listening_node_to_master_transition_on_announce_receipt_timeout() {
        let node = ListeningNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakePortIo::new(),
            0,
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
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakePortIo::new(),
            0,
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
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakePortIo::new(),
            0,
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
        let portio = FakePortIo::new();

        let _ = ListeningNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
            0,
        );

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::AnnounceReceiptTimeout)
        );
    }

    #[test]
    fn pre_master_node_schedules_qualification_timeout() {
        let portio = FakePortIo::new();

        let _ = PreMasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &portio,
        );

        assert!(
            portio
                .system_messages
                .borrow()
                .contains(&SystemMessage::QualificationTimeout)
        );
    }

    #[test]
    fn pre_master_node_to_master_transition_on_qualification_timeout() {
        let node = PreMasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakePortIo::new(),
        );

        let node = node.system_message(SystemMessage::QualificationTimeout);

        match node {
            NodeState::Master(_) => {}
            _ => panic!("Expected Master state"),
        }
    }

    struct FakePortIo {
        event_messages: RefCell<Vec<EventMessage>>,
        general_messages: RefCell<Vec<GeneralMessage>>,
        system_messages: RefCell<Vec<SystemMessage>>,
    }

    impl FakePortIo {
        pub fn new() -> Self {
            Self {
                event_messages: RefCell::new(Vec::new()),
                general_messages: RefCell::new(Vec::new()),
                system_messages: RefCell::new(Vec::new()),
            }
        }
    }

    impl PortIo for FakePortIo {
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

    impl PortIo for &FakePortIo {
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
