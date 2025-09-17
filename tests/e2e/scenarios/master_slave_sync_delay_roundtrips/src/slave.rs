use std::cell::Cell;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use rptp::message::{EventMessage, GeneralMessage, SystemMessage};
use rptp::node::{Node, SlaveNode};
use rptp_daemon::net::MulticastPort;
use rptp_daemon::node::{
    TokioEventInterface, TokioGeneralInterface, TokioNode, TokioSystemInterface,
};

struct SpyNode {
    node: SlaveNode<TokioEventInterface, TokioGeneralInterface, TokioSystemInterface>,
    received_sync: Cell<u32>,
    received_follow_up: Cell<u32>,
    received_delay_resp: Cell<u32>,
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
            received_sync: Cell::new(0),
            received_follow_up: Cell::new(0),
            received_delay_resp: Cell::new(0),
            notify,
        }
    }

    fn test_accept_condition(&self) {
        if self.received_sync.get() > 5
            && self.received_follow_up.get() > 5
            && self.received_delay_resp.get() > 5
        {
            self.notify.notify_waiters();
        }
    }
}

impl Node for SpyNode {
    fn event_message(&self, msg: EventMessage) {
        if let EventMessage::TwoStepSync(_) = msg {
            self.received_sync.set(self.received_sync.get() + 1);
            self.test_accept_condition();
        }
        self.node.event_message(msg)
    }

    fn general_message(&self, msg: GeneralMessage) {
        if let GeneralMessage::FollowUp(_) = msg {
            self.received_follow_up
                .set(self.received_follow_up.get() + 1);
            self.test_accept_condition();
        }
        if let GeneralMessage::DelayResp(_) = msg {
            self.received_delay_resp
                .set(self.received_delay_resp.get() + 1);
            self.test_accept_condition();
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
