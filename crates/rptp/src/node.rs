use std::time::Duration;

use crate::message::{EventMessage, GeneralMessage, SystemMessage, TwoStepSyncMessage};

pub trait EventInterface: Send {
    fn send(&self, msg: EventMessage);
}

pub trait GeneralInterface: Send {
    fn send(&self, msg: GeneralMessage);
}

pub trait SystemInterface: Send {
    fn send(&self, msg: SystemMessage, delay: Duration);
}

pub trait Node: Send {
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
    system_interface: S,
}

impl<E, G, S> SlaveNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(event_interface: E, general_interface: G, system_interface: S) -> Self {
        system_interface.send(SystemMessage::DelayCycle, Duration::ZERO);

        Self {
            event_interface,
            _general_interface: general_interface,
            system_interface,
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
            SystemMessage::DelayCycle => {
                self.event_interface.send(EventMessage::DelayReq);
                self.system_interface
                    .send(SystemMessage::DelayCycle, Duration::from_secs(1));
            }
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
    system_interface: S,
}

impl<E, G, S> MasterNode<E, G, S>
where
    E: EventInterface,
    G: GeneralInterface,
    S: SystemInterface,
{
    pub fn new(event_interface: E, general_interface: G, system_interface: S) -> Self {
        system_interface.send(SystemMessage::SyncCycle, Duration::ZERO);

        Self {
            event_interface,
            general_interface,
            system_interface,
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
                self.event_interface
                    .send(EventMessage::TwoStepSync(TwoStepSyncMessage::new(0)));
                self.system_interface
                    .send(SystemMessage::SyncCycle, Duration::from_secs(1));
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::message::FollowUpMessage;
    use crate::time::TimeStamp;

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
            *general_interface.sent_messages.lock().unwrap(),
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
            *system_interface.sent_messages.lock().unwrap(),
            vec![SystemMessage::SyncCycle]
        );
    }

    #[test]
    fn master_node_answers_sync_cycle_with_sync() {
        let event_interface = FakeEventInterface::new();

        let node = MasterNode::new(
            &event_interface,
            FakeGeneralInterface::new(),
            FakeSystemInterface::new(),
        );
        node.system_message(SystemMessage::SyncCycle);

        assert_eq!(
            *event_interface.sent_messages.lock().unwrap(),
            vec![EventMessage::TwoStepSync(TwoStepSyncMessage::new(0))],
        );
    }

    #[test]
    fn master_node_schedules_next_sync() {
        let system_interface = FakeSystemInterface::new();

        let node = MasterNode::new(
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        node.system_message(SystemMessage::SyncCycle);

        assert_eq!(
            *system_interface.sent_messages.lock().unwrap(),
            vec![SystemMessage::SyncCycle, SystemMessage::SyncCycle],
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
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        assert_eq!(
            *system_interface.sent_messages.lock().unwrap(),
            vec![SystemMessage::DelayCycle]
        );
    }

    #[test]
    fn slave_node_schedules_next_delay_request() {
        let system_interface = FakeSystemInterface::new();

        let node = SlaveNode::new(
            FakeEventInterface::new(),
            FakeGeneralInterface::new(),
            &system_interface,
        );

        node.system_message(SystemMessage::DelayCycle);

        assert_eq!(
            *system_interface.sent_messages.lock().unwrap(),
            vec![SystemMessage::DelayCycle, SystemMessage::DelayCycle],
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
            *event_interface.sent_messages.lock().unwrap(),
            vec![EventMessage::DelayReq]
        );
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
