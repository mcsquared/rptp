use std::cell::Cell;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use rptp::message::{EventMessage, GeneralMessage, SystemMessage};
use rptp::node::{MasterNode, Node};
use rptp::time::TimeStamp;
use rptp_daemon::net::MulticastPort;
use rptp_daemon::node::{
    TokioEventInterface, TokioGeneralInterface, TokioNode, TokioSystemInterface,
};

struct SlaveAcceptCondition {
    notify: Arc<Notify>,
}

impl SlaveAcceptCondition {
    fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
        }
    }

    async fn notified(&self) -> std::io::Result<impl std::future::Future<Output = ()> + '_> {
        let socket = UdpSocket::bind("0.0.0.0:12345").await?;
        let notify = self.notify.clone();

        tokio::spawn(async move {
            let mut buf = [0; 1024];
            if let Ok(size) = socket.recv(&mut buf).await {
                if let Ok(message) = str::from_utf8(&buf[..size]) {
                    if message == "accept" {
                        notify.notify_waiters();
                    }
                }
            }
        });

        Ok(self.notify.notified())
    }
}

struct SpyNode {
    node: MasterNode<TokioEventInterface, TokioGeneralInterface, TokioSystemInterface>,
    received_delay_req: Cell<u32>,
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
            node: MasterNode::new(event, general, system),
            received_delay_req: Cell::new(0),
            notify,
        }
    }

    fn test_accept_condition(&self) {
        if self.received_delay_req.get() > 5 {
            self.notify.notify_waiters();
        }
    }
}

impl Node for SpyNode {
    fn event_message(&self, msg: EventMessage, timestamp: TimeStamp) {
        self.node.event_message(msg, timestamp);

        if let EventMessage::DelayReq(_) = msg {
            self.received_delay_req
                .set(self.received_delay_req.get() + 1);
            self.test_accept_condition();
        }
    }

    fn general_message(&self, msg: GeneralMessage) {
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

    let master = TokioNode::new(event_port, general_port, |event, general, system| {
        SpyNode::new(event, general, system, notify.clone())
    })
    .await?;

    println!("Master ready");

    let slave_cond = SlaveAcceptCondition::new();
    let slave_notification = slave_cond.notified().await?;
    let exit_cond = {
        async move {
            let _ = tokio::join!(notify.notified(), slave_notification);
        }
    };

    timeout(Duration::from_secs(30), master.run_until(exit_cond))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Master node timed out before receiving expected count of messages",
            )
        })?
}
