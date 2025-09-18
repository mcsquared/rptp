use std::cell::RefCell;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use rptp::message::{
    EventMessage, FollowUpMessage, GeneralMessage, SystemMessage, TwoStepSyncMessage,
};
use rptp::node::{Node, SlaveNode};
use rptp::time::TimeStamp;
use rptp_daemon::net::MulticastPort;
use rptp_daemon::node::{
    TokioEventInterface, TokioGeneralInterface, TokioNode, TokioSystemInterface,
};

enum TwoStepState {
    None,
    HaveSync(TwoStepSyncMessage),
    HaveFollowUp(FollowUpMessage),
}

impl TwoStepState {
    fn with_sync(&mut self, sync: TwoStepSyncMessage) -> bool {
        if let TwoStepState::HaveFollowUp(follow_up) = self {
            if follow_up
                .master_slave_offset(sync, TimeStamp::new(0, 0))
                .is_some()
            {
                *self = TwoStepState::None;
                return true;
            } else {
                *self = TwoStepState::None;
            }
        } else {
            *self = TwoStepState::HaveSync(sync);
        }
        false
    }

    fn with_follow_up(&mut self, follow_up: FollowUpMessage) -> bool {
        if let TwoStepState::HaveSync(sync) = *self {
            if follow_up
                .master_slave_offset(sync, TimeStamp::new(0, 0))
                .is_some()
            {
                *self = TwoStepState::None;
                return true;
            } else {
                *self = TwoStepState::None;
            }
        } else {
            *self = TwoStepState::HaveFollowUp(follow_up);
        }
        false
    }
}

struct SpyNode {
    node: SlaveNode<TokioEventInterface, TokioGeneralInterface, TokioSystemInterface>,
    two_step_state: RefCell<TwoStepState>,
    received_sync: RefCell<u32>,
    follow_ups_matched: RefCell<u32>,
    received_delay_resp: RefCell<u32>,
    notify: Arc<Notify>,
}

impl SpyNode {
    fn new(
        event: TokioEventInterface,
        general: TokioGeneralInterface,
        system: TokioSystemInterface,
        notify: Arc<Notify>,
    ) -> Self {
        Self {
            node: SlaveNode::new(event, general, system),
            two_step_state: RefCell::new(TwoStepState::None),
            received_sync: RefCell::new(0),
            follow_ups_matched: RefCell::new(0),
            received_delay_resp: RefCell::new(0),
            notify,
        }
    }

    fn test_accept_condition(&self) {
        if *self.received_sync.borrow() > 5
            && *self.received_sync.borrow() == *self.follow_ups_matched.borrow()
            && *self.received_delay_resp.borrow() > 5
        {
            self.notify.notify_waiters();
        }
    }
}

impl Node for SpyNode {
    fn event_message(&self, msg: EventMessage) {
        match msg {
            EventMessage::TwoStepSync(sync) => {
                *self.received_sync.borrow_mut() += 1;

                if self.two_step_state.borrow_mut().with_sync(sync) {
                    *self.follow_ups_matched.borrow_mut() += 1;
                }

                self.test_accept_condition();
            }
            _ => {}
        }
        self.node.event_message(msg)
    }

    fn general_message(&self, msg: GeneralMessage) {
        match msg {
            GeneralMessage::FollowUp(follow_up) => {
                if self.two_step_state.borrow_mut().with_follow_up(follow_up) {
                    *self.follow_ups_matched.borrow_mut() += 1;
                }
                self.test_accept_condition();
            }
            GeneralMessage::DelayResp(_) => {
                *self.received_delay_resp.borrow_mut() += 1;
                self.test_accept_condition();
            }
        }
        self.node.general_message(msg)
    }

    fn system_message(&self, msg: SystemMessage) {
        self.node.system_message(msg)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let notify = Arc::new(Notify::new());

    let event_port = MulticastPort::ptp_event_testing_port().await?;
    let general_port = MulticastPort::ptp_general_testing_port().await?;

    let slave = TokioNode::new(event_port, general_port, |event, general, system| {
        SpyNode::new(event, general, system, notify.clone())
    })
    .await?;

    println!("Slave ready");

    let result = timeout(Duration::from_secs(30), slave.run_until(notify.notified()))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Slave node timed out before receiving expected count of messages",
            )
        })?;

    let socket = UdpSocket::bind("0.0.0.0:12345").await?;
    socket.set_broadcast(true)?;
    socket.send_to(b"accept", "255.255.255.255:12345").await?;

    result
}
