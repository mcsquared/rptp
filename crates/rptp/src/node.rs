use std::time::Duration;

use crate::clock::{SynchronizableClock, SynchronizedClock};
use crate::message::{
    DelayCycleMessage, EventMessage, GeneralMessage, SyncCycleMessage, SystemMessage,
};
use crate::time::TimeStamp;

pub trait EventInterface {
    fn send(&self, msg: EventMessage);
}

pub trait GeneralInterface {
    fn send(&self, msg: GeneralMessage);
}

pub trait SystemInterface {
    fn send(&self, msg: SystemMessage, delay: Duration);
}

pub enum NodeState<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    Initializing(InitializingNode<C, E, G, S>),
    Listening(ListeningNode<C, E, G, S>),
    Slave(SlaveNode<C, E, G, S>),
    Master(MasterNode<C, E, G, S>),
}

impl<C, E, G, S> NodeState<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> Self {
        match self {
            NodeState::Initializing(_) => self,
            NodeState::Listening(_) => self,
            NodeState::Slave(node) => node.event_message(msg, timestamp),
            NodeState::Master(node) => node.event_message(msg, timestamp),
        }
    }

    pub fn general_message(self, msg: GeneralMessage) -> Self {
        match self {
            NodeState::Initializing(_) => self,
            NodeState::Listening(_) => self,
            NodeState::Slave(node) => node.general_message(msg),
            NodeState::Master(node) => node.general_message(msg),
        }
    }

    pub fn system_message(self, msg: SystemMessage) -> Self {
        match self {
            NodeState::Initializing(node) => node.system_message(msg),
            NodeState::Listening(node) => node.system_message(msg),
            NodeState::Slave(node) => node.system_message(msg),
            NodeState::Master(node) => node.system_message(msg),
        }
    }
}

pub struct InitializingNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    clock: SynchronizedClock<C>,
    event_interface: E,
    general_interface: G,
    system_interface: S,
}

impl<C, E, G, S> InitializingNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(
        clock: SynchronizedClock<C>,
        event_interface: E,
        general_interface: G,
        system_interface: S,
    ) -> Self {
        Self {
            clock,
            event_interface,
            general_interface,
            system_interface,
        }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, E, G, S> {
        match msg {
            SystemMessage::Initialized => NodeState::Listening(ListeningNode::new(
                self.clock,
                self.event_interface,
                self.general_interface,
                self.system_interface,
            )),
            _ => NodeState::Initializing(self),
        }
    }
}

pub struct ListeningNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    clock: SynchronizedClock<C>,
    event_interface: E,
    general_interface: G,
    system_interface: S,
}

impl<C, E, G, S> ListeningNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(
        clock: SynchronizedClock<C>,
        event_interface: E,
        general_interface: G,
        system_interface: S,
    ) -> Self {
        system_interface.send(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        Self {
            clock,
            event_interface,
            general_interface,
            system_interface,
        }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, E, G, S> {
        match msg {
            SystemMessage::AnnounceReceiptTimeout => NodeState::Master(MasterNode::new(
                self.clock,
                self.event_interface,
                self.general_interface,
                self.system_interface,
            )),
            _ => NodeState::Listening(self),
        }
    }
}

pub struct SlaveNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    clock: SynchronizedClock<C>,
    event_interface: E,
    _general_interface: G,
    system_interface: S,
}

impl<C, E, G, S> SlaveNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(
        clock: SynchronizedClock<C>,
        event_interface: E,
        general_interface: G,
        system_interface: S,
    ) -> Self {
        system_interface.send(
            SystemMessage::DelayCycle(DelayCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self {
            clock,
            event_interface,
            _general_interface: general_interface,
            system_interface,
        }
    }

    fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> NodeState<C, E, G, S> {
        match msg {
            EventMessage::TwoStepSync(sync) => {
                self.clock.ingest_two_step_sync(sync, timestamp);
            }
            _ => {}
        }

        NodeState::Slave(self)
    }

    fn general_message(self, msg: GeneralMessage) -> NodeState<C, E, G, S> {
        match msg {
            GeneralMessage::FollowUp(follow_up) => {
                self.clock.ingest_follow_up(follow_up);
            }
            GeneralMessage::DelayResp(resp) => {
                self.clock.ingest_delay_response(resp);
            }
        }

        NodeState::Slave(self)
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, E, G, S> {
        match msg {
            SystemMessage::DelayCycle(delay_cycle) => {
                let delay_request = delay_cycle.delay_request();
                let next_cycle = delay_cycle.next();

                self.event_interface
                    .send(EventMessage::DelayReq(delay_request));
                self.system_interface.send(
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

pub struct MasterNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    _clock: SynchronizedClock<C>,
    event_interface: E,
    general_interface: G,
    system_interface: S,
}

impl<C, E, G, S> MasterNode<C, E, G, S>
where
    C: SynchronizableClock,
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(
        _clock: SynchronizedClock<C>,
        event_interface: E,
        general_interface: G,
        system_interface: S,
    ) -> Self {
        system_interface.send(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self {
            _clock,
            event_interface,
            general_interface,
            system_interface,
        }
    }

    fn event_message(self, msg: EventMessage, timestamp: TimeStamp) -> NodeState<C, E, G, S> {
        match msg {
            EventMessage::DelayReq(req) => self
                .general_interface
                .send(GeneralMessage::DelayResp(req.response(timestamp))),
            _ => {}
        }

        NodeState::Master(self)
    }

    fn general_message(self, _msg: GeneralMessage) -> NodeState<C, E, G, S> {
        match _msg {
            _ => {}
        }

        NodeState::Master(self)
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<C, E, G, S> {
        match msg {
            SystemMessage::SyncCycle(sync_cycle) => {
                let sync_message = sync_cycle.two_step_sync();
                let next_cycle = sync_cycle.next();

                self.event_interface
                    .send(EventMessage::TwoStepSync(sync_message));
                self.system_interface
                    .send(SystemMessage::SyncCycle(next_cycle), Duration::from_secs(1));
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::TwoStepSync(twostep) => {
                    self.general_interface
                        .send(GeneralMessage::FollowUp(twostep.follow_up(timestamp)));
                }
                _ => {}
            },
            _ => {}
        }

        NodeState::Master(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::rc::Rc;

    use crate::clock::{Clock, FakeClock};
    use crate::message::{
        DelayRequestMessage, DelayResponseMessage, FollowUpMessage, TwoStepSyncMessage,
    };
    use crate::time::TimeStamp;

    #[test]
    fn slave_node_synchronizes_clock() {
        let local_clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));

        let mut slave = NodeState::Slave(SlaveNode::new(
            SynchronizedClock::new(local_clock.clone()),
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            FakeSystemInterface::new(),
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
        let general_interface = FakeGeneralInterface::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakeEventInterface::new(),
            &general_interface,
            FakeSystemInterface::new(),
        );
        node.event_message(
            EventMessage::DelayReq(DelayRequestMessage::new(0)),
            TimeStamp::new(0, 0),
        );

        assert_eq!(
            *general_interface.sent_messages.lock().unwrap(),
            vec![GeneralMessage::DelayResp(DelayResponseMessage::new(
                0,
                TimeStamp::new(0, 0),
            ))]
        );
    }

    #[test]
    fn master_node_schedules_initial_sync() {
        let system_interface = FakeSystemInterface::new();

        let _ = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        assert_eq!(
            *system_interface.sent_messages.lock().unwrap(),
            vec![SystemMessage::SyncCycle(SyncCycleMessage::new(0))]
        );
    }

    #[test]
    fn master_node_answers_sync_cycle_with_sync() {
        let event_interface = FakeEventInterface::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &event_interface,
            FakeGeneralInterface::new(),
            FakeSystemInterface::new(),
        );
        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        assert_eq!(
            *event_interface.sent_messages.lock().unwrap(),
            vec![EventMessage::TwoStepSync(TwoStepSyncMessage::new(0))],
        );
    }

    #[test]
    fn master_node_schedules_next_sync() {
        let system_interface = FakeSystemInterface::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        assert_eq!(
            *system_interface.sent_messages.lock().unwrap(),
            vec![
                SystemMessage::SyncCycle(SyncCycleMessage::new(0)),
                SystemMessage::SyncCycle(SyncCycleMessage::new(1))
            ],
        );
    }

    #[test]
    fn master_node_answers_timestamped_sync_with_follow_up() {
        let general_interface = FakeGeneralInterface::new();

        let node = MasterNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakeEventInterface::new(),
            &general_interface,
            FakeSystemInterface::new(),
        );
        node.system_message(SystemMessage::Timestamp {
            msg: EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)),
            timestamp: TimeStamp::new(0, 0),
        });

        assert_eq!(
            *general_interface.sent_messages.lock().unwrap(),
            vec![GeneralMessage::FollowUp(FollowUpMessage::new(
                0,
                TimeStamp::new(0, 0)
            ))],
        );
    }

    #[test]
    fn slave_node_schedules_initial_delay_cycle() {
        let system_interface = FakeSystemInterface::new();

        let _ = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        assert_eq!(
            *system_interface.sent_messages.lock().unwrap(),
            vec![SystemMessage::DelayCycle(DelayCycleMessage::new(0))]
        );
    }

    #[test]
    fn slave_node_schedules_next_delay_request() {
        let system_interface = FakeSystemInterface::new();

        let node = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        assert_eq!(
            *system_interface.sent_messages.lock().unwrap(),
            vec![
                SystemMessage::DelayCycle(DelayCycleMessage::new(0)),
                SystemMessage::DelayCycle(DelayCycleMessage::new(1))
            ],
        );
    }

    #[test]
    fn slave_node_answers_delay_request_cycle_with_delay_request() {
        let event_interface = FakeEventInterface::new();

        let node = SlaveNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            &event_interface,
            FakeGeneralInterface::new(),
            FakeSystemInterface::new(),
        );
        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        assert_eq!(
            *event_interface.sent_messages.lock().unwrap(),
            vec![EventMessage::DelayReq(DelayRequestMessage::new(0))]
        );
    }

    #[test]
    fn initializing_node_to_listening_transition() {
        let node = InitializingNode::new(
            SynchronizedClock::new(FakeClock::new(TimeStamp::new(0, 0))),
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            FakeSystemInterface::new(),
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
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            FakeSystemInterface::new(),
        );

        let node = node.system_message(SystemMessage::AnnounceReceiptTimeout);

        match node {
            NodeState::Master(_) => {}
            _ => panic!("Expected Master state"),
        }
    }

    use std::sync::{Arc, Mutex};

    struct FakeEventInterface {
        sent_messages: Arc<Mutex<Vec<EventMessage>>>,
    }

    impl FakeEventInterface {
        fn new() -> Self {
            Self {
                sent_messages: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl EventInterface for FakeEventInterface {
        fn send(&self, msg: EventMessage) {
            self.sent_messages.lock().unwrap().push(msg);
        }
    }

    impl EventInterface for &FakeEventInterface {
        fn send(&self, msg: EventMessage) {
            self.sent_messages.lock().unwrap().push(msg);
        }
    }

    struct FakeGeneralInterface {
        sent_messages: Arc<Mutex<Vec<GeneralMessage>>>,
    }

    impl FakeGeneralInterface {
        fn new() -> Self {
            Self {
                sent_messages: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl GeneralInterface for FakeGeneralInterface {
        fn send(&self, msg: GeneralMessage) {
            self.sent_messages.lock().unwrap().push(msg);
        }
    }

    impl GeneralInterface for &FakeGeneralInterface {
        fn send(&self, msg: GeneralMessage) {
            self.sent_messages.lock().unwrap().push(msg);
        }
    }

    struct FakeSystemInterface {
        sent_messages: Arc<Mutex<Vec<SystemMessage>>>,
    }

    impl FakeSystemInterface {
        fn new() -> Self {
            Self {
                sent_messages: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl SystemInterface for FakeSystemInterface {
        fn send(&self, msg: SystemMessage, _delay: Duration) {
            self.sent_messages.lock().unwrap().push(msg);
        }
    }

    impl SystemInterface for &FakeSystemInterface {
        fn send(&self, msg: SystemMessage, _delay: Duration) {
            self.sent_messages.lock().unwrap().push(msg);
        }
    }
}
