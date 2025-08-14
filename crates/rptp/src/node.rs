use crate::message::{EventMessage, GeneralMessage, SystemMessage};

pub trait EventInterface {
    fn send(&self, msg: EventMessage);
}

pub trait GeneralInterface {
    fn send(&self, msg: GeneralMessage);
}

pub trait SystemInterface {
    fn send(&self, msg: SystemMessage);
}

pub trait Node {
    fn event_message(&self, msg: EventMessage);
    fn general_message(&self, msg: GeneralMessage);
    fn system_message(&self, msg: SystemMessage);
}

pub struct SlaveNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    event_interface: E,
    _general_interface: G,
    _system_interface: S,
}

impl<E, G, S> SlaveNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(event_interface: E, general_interface: G, system_interface: S) -> Self {
        system_interface.send(SystemMessage::DelayCycle);

        Self {
            event_interface,
            _general_interface: general_interface,
            _system_interface: system_interface,
        }
    }
}

impl<E, G, S> Node for SlaveNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    fn event_message(&self, msg: EventMessage) {
        match msg {
            _ => {}
        }
    }

    fn general_message(&self, msg: GeneralMessage) {
        match msg {
            _ => {}
        }
    }

    fn system_message(&self, msg: SystemMessage) {
        match msg {
            SystemMessage::DelayCycle => self.event_interface.send(EventMessage::DelayReq),
            _ => {}
        }
    }
}

pub struct MasterNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    event_interface: E,
    general_interface: G,
    _system_interface: S,
}

impl<E, G, S> MasterNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(event_interface: E, general_interface: G, system_interface: S) -> Self {
        system_interface.send(SystemMessage::SyncCycle);

        Self {
            event_interface,
            general_interface,
            _system_interface: system_interface,
        }
    }
}

impl<E, G, S> Node for MasterNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    fn event_message(&self, msg: EventMessage) {
        match msg {
            EventMessage::DelayReq => self.general_interface.send(GeneralMessage::DelayResp),
            _ => {}
        }
    }

    fn general_message(&self, _msg: GeneralMessage) {
        match _msg {
            _ => {}
        }
    }

    fn system_message(&self, msg: SystemMessage) {
        match msg {
            SystemMessage::SyncCycle => {
                self.event_interface.send(EventMessage::Sync);
            }
            SystemMessage::Timestamp { msg, timestamp } => match msg {
                EventMessage::Sync => {
                    self.general_interface
                        .send(GeneralMessage::FollowUp(timestamp));
                }
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_node_answers_delay_request_with_delay_response() {
        let general_interface = FakeGeneralInterface::new();

        let node = MasterNode::new(
            FakeEventInterface::new(),
            &general_interface,
            FakeSystemInterface::new(),
        );
        node.event_message(EventMessage::DelayReq);

        assert_eq!(
            *general_interface.sent_messages.borrow(),
            vec![GeneralMessage::DelayResp]
        );
    }

    #[test]
    fn master_node_schedules_initial_sync() {
        let system_interface = FakeSystemInterface::new();

        let _ = MasterNode::new(
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        assert_eq!(
            *system_interface.sent_messages.borrow(),
            vec![SystemMessage::SyncCycle]
        );
    }

    #[test]
    fn master_node_answers_sync_cycle_with_sync() {
        let event_interface = FakeEventInterface::new();
        let general_interface = FakeGeneralInterface::new();

        let node = MasterNode::new(
            &event_interface,
            &general_interface,
            FakeSystemInterface::new(),
        );
        node.system_message(SystemMessage::SyncCycle);

        assert_eq!(
            *event_interface.sent_messages.borrow(),
            vec![EventMessage::Sync],
        );
    }

    #[test]
    fn master_node_answers_timestamped_sync_with_follow_up() {
        let general_interface = FakeGeneralInterface::new();

        let node = MasterNode::new(
            FakeEventInterface::new(),
            &general_interface,
            FakeSystemInterface::new(),
        );
        node.system_message(SystemMessage::Timestamp {
            msg: EventMessage::Sync,
            timestamp: 42,
        });

        assert_eq!(
            *general_interface.sent_messages.borrow(),
            vec![GeneralMessage::FollowUp(42)],
        );
    }

    #[test]
    fn slave_node_schedules_initial_delay_cycle() {
        let system_interface = FakeSystemInterface::new();

        let _ = SlaveNode::new(
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        assert_eq!(
            *system_interface.sent_messages.borrow(),
            vec![SystemMessage::DelayCycle]
        );
    }

    #[test]
    fn slave_node_answers_delay_request_cycle_with_delay_request() {
        let event_interface = FakeEventInterface::new();

        let node = SlaveNode::new(
            &event_interface,
            FakeGeneralInterface::new(),
            FakeSystemInterface::new(),
        );
        node.system_message(SystemMessage::DelayCycle);

        assert_eq!(
            *event_interface.sent_messages.borrow(),
            vec![EventMessage::DelayReq]
        );
    }

    use std::cell::RefCell;

    struct FakeEventInterface {
        sent_messages: RefCell<Vec<EventMessage>>,
    }

    impl FakeEventInterface {
        fn new() -> Self {
            Self {
                sent_messages: RefCell::new(Vec::new()),
            }
        }
    }

    impl EventInterface for FakeEventInterface {
        fn send(&self, msg: EventMessage) {
            self.sent_messages.borrow_mut().push(msg);
        }
    }

    impl EventInterface for &FakeEventInterface {
        fn send(&self, msg: EventMessage) {
            self.sent_messages.borrow_mut().push(msg);
        }
    }

    struct FakeGeneralInterface {
        sent_messages: RefCell<Vec<GeneralMessage>>,
    }

    impl FakeGeneralInterface {
        fn new() -> Self {
            Self {
                sent_messages: RefCell::new(Vec::new()),
            }
        }
    }

    impl GeneralInterface for FakeGeneralInterface {
        fn send(&self, msg: GeneralMessage) {
            self.sent_messages.borrow_mut().push(msg);
        }
    }

    impl GeneralInterface for &FakeGeneralInterface {
        fn send(&self, msg: GeneralMessage) {
            self.sent_messages.borrow_mut().push(msg);
        }
    }

    struct FakeSystemInterface {
        sent_messages: RefCell<Vec<SystemMessage>>,
    }

    impl FakeSystemInterface {
        fn new() -> Self {
            Self {
                sent_messages: RefCell::new(Vec::new()),
            }
        }
    }

    impl SystemInterface for FakeSystemInterface {
        fn send(&self, msg: SystemMessage) {
            self.sent_messages.borrow_mut().push(msg);
        }
    }

    impl SystemInterface for &FakeSystemInterface {
        fn send(&self, msg: SystemMessage) {
            self.sent_messages.borrow_mut().push(msg);
        }
    }
}
