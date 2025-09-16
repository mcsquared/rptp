use std::cell::Cell;
use std::sync::Arc;

use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use rptp::message::{EventMessage, GeneralMessage, SystemMessage};
use rptp::node::Node;
use rptp_daemon::net::MulticastPort;
use rptp_daemon::node::TokioNode;

struct SpyNode {
    received_sync: Cell<u32>,
    received_follow_up: Cell<u32>,
    notify: Arc<Notify>,
}

impl SpyNode {
    fn new(notify: Arc<Notify>) -> Self {
        Self {
            received_sync: Cell::new(0),
            received_follow_up: Cell::new(0),
            notify,
        }
    }
}

impl Node for SpyNode {
    fn event_message(&self, msg: EventMessage) {
        if let EventMessage::Sync = msg {
            self.received_sync.set(self.received_sync.get() + 1);
            if self.received_sync.get() > 5 && self.received_follow_up.get() > 5 {
                self.notify.notify_waiters();
            }
        }
    }

    fn general_message(&self, msg: GeneralMessage) {
        if let GeneralMessage::FollowUp(_) = msg {
            self.received_follow_up
                .set(self.received_follow_up.get() + 1);
            if self.received_sync.get() > 5 && self.received_follow_up.get() > 5 {
                self.notify.notify_waiters();
            }
        }
    }

    fn system_message(&self, _msg: SystemMessage) {}
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let notify = Arc::new(Notify::new());

    let event_port = MulticastPort::ptp_event_testing_port().await?;
    let general_port = MulticastPort::ptp_general_testing_port().await?;

    let slave = TokioNode::new(event_port, general_port, |_event, _general, _system| {
        SpyNode::new(notify.clone())
    })
    .await?;

    println!("Slave ready");

    timeout(Duration::from_secs(30), slave.run_until(notify.notified()))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Timeout for receiving sync and follow-up messages",
            )
        })?
}
