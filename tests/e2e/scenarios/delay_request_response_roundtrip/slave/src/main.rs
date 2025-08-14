use std::rc::Rc;

use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use rptp::message::{EventMessage, GeneralMessage, SystemMessage};
use rptp::node::{Node, SlaveNode};
use rptp_daemon::node::{
    TokioEventInterface, TokioGeneralInterface, TokioNode, TokioSystemInterface,
};

struct SpyNode {
    node: SlaveNode<TokioEventInterface, TokioGeneralInterface, TokioSystemInterface>,
    notify: Rc<Notify>,
}

impl SpyNode {
    fn new(
        event: TokioEventInterface,
        general: TokioGeneralInterface,
        system: TokioSystemInterface,
        notify: Rc<Notify>,
    ) -> Self {
        Self {
            node: SlaveNode::new(event, general, system),
            notify,
        }
    }
}

impl Node for SpyNode {
    fn event_message(&self, msg: EventMessage) {
        self.node.event_message(msg)
    }

    fn general_message(&self, msg: GeneralMessage) {
        if let GeneralMessage::DelayResp = msg {
            self.notify.notify_waiters();
        }
        self.node.general_message(msg)
    }

    fn system_message(&self, msg: SystemMessage) {
        self.node.system_message(msg)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let notify = Rc::new(Notify::new());

    let slave = TokioNode::new(|event, general, system| {
        SpyNode::new(event, general, system, notify.clone())
    })
    .await?;

    timeout(Duration::from_secs(10), slave.run_until(notify.notified()))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Slave node timed out before receiving delay response",
            )
        })?
}
