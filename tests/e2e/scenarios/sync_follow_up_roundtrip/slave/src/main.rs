use std::cell::Cell;
use std::rc::Rc;

use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use rptp::message::{EventMessage, GeneralMessage, SystemMessage};
use rptp::node::Node;
use rptp_daemon::node::TokioNode;

struct SpyNode {
    received_sync: Cell<bool>,
    received_follow_up: Cell<bool>,
    notify: Rc<Notify>,
}

impl SpyNode {
    fn new(notify: Rc<Notify>) -> Self {
        Self {
            received_sync: Cell::new(false),
            received_follow_up: Cell::new(false),
            notify,
        }
    }
}

impl Node for SpyNode {
    fn event_message(&self, msg: EventMessage) {
        if let EventMessage::Sync = msg {
            self.received_sync.set(true);
            if self.received_sync.get() && self.received_follow_up.get() {
                self.notify.notify_waiters();
            }
        }
    }

    fn general_message(&self, msg: GeneralMessage) {
        if let GeneralMessage::FollowUp(_) = msg {
            self.received_follow_up.set(true);
            if self.received_sync.get() && self.received_follow_up.get() {
                self.notify.notify_waiters();
            }
        }
    }

    fn system_message(&self, _msg: SystemMessage) {}
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let notify = Rc::new(Notify::new());

    let slave = TokioNode::new(|_event, _general, _system| SpyNode::new(notify.clone())).await?;
    println!("Slave ready");

    timeout(Duration::from_secs(10), slave.run_until(notify.notified()))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Timeout for receiving sync and follow-up messages",
            )
        })?
}
