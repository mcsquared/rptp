use std::time::Duration;

use crate::bmca::BestForeignClock;
use crate::message::{
    AnnounceCycleMessage, DelayCycleMessage, EventMessage, GeneralMessage, SyncCycleMessage,
    SystemMessage,
};
use crate::port::{Port, Timeout};
use crate::time::TimeStamp;

pub enum NodeState<P: Port> {
    Initializing(InitializingNode<P>),
    Listening(ListeningNode<P>),
    Slave(SlaveNode<P>),
    Master(MasterNode<P>),
    PreMaster(PreMasterNode<P>),
}

impl<P: Port> NodeState<P> {
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

pub struct InitializingNode<P: Port> {
    port: P,
}

impl<P: Port> InitializingNode<P> {
    pub fn new(port: P) -> Self {
        Self { port }
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::Initialized => NodeState::Listening(ListeningNode::new(self.port)),
            _ => NodeState::Initializing(self),
        }
    }
}

pub struct ListeningNode<P: Port> {
    port: P,
    best_foreign: BestForeignClock<P::ClockStore>,
    announce_receipt_timeout: P::Timeout,
}

impl<P: Port> ListeningNode<P> {
    pub fn new(port: P) -> Self {
        let announce_receipt_timeout = port.schedule(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(5),
        );

        let best_foreign = BestForeignClock::new(port.foreign_clock_store());

        Self {
            port,
            best_foreign,
            announce_receipt_timeout,
        }
    }

    fn general_message(self, msg: GeneralMessage) -> NodeState<P> {
        match msg {
            GeneralMessage::Announce(msg) => {
                self.announce_receipt_timeout
                    .restart(Duration::from_secs(5));
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

pub struct SlaveNode<P: Port> {
    port: P,
    delay_cycle_timeout: P::Timeout,
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
        }
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
                    self.port.clock().ingest_delay_request(req, timestamp);
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
    announce_cycle_timeout: P::Timeout,
    sync_cycle_timeout: P::Timeout,
}

impl<P: Port> MasterNode<P> {
    pub fn new(port: P) -> Self {
        let announce_cycle_timeout = port.schedule(
            SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)),
            Duration::ZERO,
        );
        let sync_cycle_timeout = port.schedule(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0)),
            Duration::ZERO,
        );

        Self {
            port,
            announce_cycle_timeout,
            sync_cycle_timeout,
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

    fn general_message(self, _msg: GeneralMessage) -> NodeState<P> {
        match _msg {
            _ => {}
        }

        NodeState::Master(self)
    }

    fn system_message(self, msg: SystemMessage) -> NodeState<P> {
        match msg {
            SystemMessage::AnnounceCycle(announce_cycle) => {
                let announce_message = announce_cycle.announce();
                let next_cycle = announce_cycle.next();

                self.port
                    .send_general(GeneralMessage::Announce(announce_message));
                self.announce_cycle_timeout.restart_with(
                    SystemMessage::AnnounceCycle(next_cycle),
                    Duration::from_secs(1),
                );
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
    _qualification_timeout: P::Timeout,
}

impl<P: Port> PreMasterNode<P> {
    pub fn new(port: P) -> Self {
        let _qualification_timeout =
            port.schedule(SystemMessage::QualificationTimeout, Duration::from_secs(2));
        Self {
            port,
            _qualification_timeout,
        }
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

    use std::rc::Rc;

    use crate::clock::{Clock, FakeClock};
    use crate::message::{
        AnnounceMessage, DelayRequestMessage, DelayResponseMessage, FollowUpMessage,
        TwoStepSyncMessage,
    };
    use crate::port::test_support::FakePort;
    use crate::time::TimeStamp;

    #[test]
    fn slave_node_synchronizes_clock() {
        let local_clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));

        let mut slave = NodeState::Slave(SlaveNode::new(FakePort::new(local_clock.clone())));

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
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

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
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = MasterNode::new(&port);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0))));
    }

    #[test]
    fn master_node_answers_sync_cycle_with_sync() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        let messages = port.take_event_messages();
        assert!(messages.contains(&EventMessage::TwoStepSync(TwoStepSyncMessage::new(0))));
    }

    #[test]
    fn master_node_schedules_next_sync() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        // Drain messages that could have been sent during initialization.
        port.take_system_messages();

        node.system_message(SystemMessage::SyncCycle(SyncCycleMessage::new(0)));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(1))));
    }

    #[test]
    fn master_node_answers_timestamped_sync_with_follow_up() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

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
    fn master_node_schedules_initial_announce_cycle() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = MasterNode::new(&port);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0))));
    }

    #[test]
    fn master_node_schedules_next_announce() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        port.take_system_messages();

        node.system_message(SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(1))));
    }

    #[test]
    fn master_node_answers_announce_cycle_with_announce() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = MasterNode::new(&port);

        port.take_system_messages();

        node.system_message(SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0)));

        let messages = port.take_general_messages();
        assert!(messages.contains(&GeneralMessage::Announce(AnnounceMessage::new(0))));
    }

    #[test]
    fn slave_node_schedules_initial_delay_cycle() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = SlaveNode::new(&port);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(0))));
    }

    #[test]
    fn slave_node_schedules_next_delay_request() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = SlaveNode::new(&port);

        port.take_system_messages();

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::DelayCycle(DelayCycleMessage::new(1))));
    }

    #[test]
    fn slave_node_answers_delay_cycle_with_delay_request() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let node = SlaveNode::new(&port);

        port.take_system_messages();

        node.system_message(SystemMessage::DelayCycle(DelayCycleMessage::new(0)));

        let events = port.take_event_messages();
        assert!(events.contains(&EventMessage::DelayReq(DelayRequestMessage::new(0))));
    }

    #[test]
    fn initializing_node_to_listening_transition() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));
        let node = InitializingNode::new(&port);

        let _ = node.system_message(SystemMessage::Initialized);

        // Listening node is expected to schedule an announce receipt timeout
        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_node_to_master_transition_on_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));
        let node = ListeningNode::new(&port);

        let _ = node.system_message(SystemMessage::AnnounceReceiptTimeout);

        // Master node is expected to schedule a sync cycle and announce cycle
        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0))));
        assert!(messages.contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0))));
    }

    #[test]
    fn listening_node_stays_in_listening_on_single_announce() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));
        let node = ListeningNode::new(&port);

        port.take_system_messages(); // Ignore the initial announce receipt timeout.

        let _ = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(0)));

        // Listening node is expected to refresh the announce receipt timeout
        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn listening_node_to_pre_master_transition_on_two_announces() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));
        let node = ListeningNode::new(&port);

        // Ignore the initial announce receipt timeout.
        port.take_system_messages();

        let node = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(0)));
        let _ = node.general_message(GeneralMessage::Announce(AnnounceMessage::new(1)));

        // PreMaster node is expected to schedule a qualification timeout
        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn listening_node_schedules_announce_receipt_timeout() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = ListeningNode::new(&port);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::AnnounceReceiptTimeout));
    }

    #[test]
    fn pre_master_node_schedules_qualification_timeout() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));

        let _ = PreMasterNode::new(&port);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::QualificationTimeout));
    }

    #[test]
    fn pre_master_node_to_master_transition_on_qualification_timeout() {
        let port = FakePort::new(FakeClock::new(TimeStamp::new(0, 0)));
        let node = PreMasterNode::new(&port);

        let _ = node.system_message(SystemMessage::QualificationTimeout);

        let messages = port.take_system_messages();
        assert!(messages.contains(&SystemMessage::SyncCycle(SyncCycleMessage::new(0))));
        assert!(messages.contains(&SystemMessage::AnnounceCycle(AnnounceCycleMessage::new(0))));
    }
}
