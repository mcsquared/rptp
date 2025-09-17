use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;

use rptp::{
    message::{EventMessage, GeneralMessage, SystemMessage},
    node::{EventInterface, GeneralInterface, MasterNode, Node, SlaveNode, SystemInterface},
    time::TimeStamp,
};

use crate::net::NetPort;

pub struct TokioEventInterface {
    tx: mpsc::UnboundedSender<EventMessage>,
}

impl TokioEventInterface {
    pub fn new(tx: mpsc::UnboundedSender<EventMessage>) -> Self {
        Self { tx }
    }
}

impl EventInterface for TokioEventInterface {
    fn send(&self, msg: EventMessage) {
        let _ = self.tx.send(msg);
    }
}

pub struct TokioGeneralInterface {
    tx: mpsc::UnboundedSender<GeneralMessage>,
}

impl TokioGeneralInterface {
    pub fn new(tx: mpsc::UnboundedSender<GeneralMessage>) -> Self {
        Self { tx }
    }
}

impl GeneralInterface for TokioGeneralInterface {
    fn send(&self, msg: GeneralMessage) {
        let _ = self.tx.send(msg);
    }
}

pub struct TokioSystemInterface {
    tx: mpsc::UnboundedSender<SystemMessage>,
}

impl TokioSystemInterface {
    pub fn new(tx: mpsc::UnboundedSender<SystemMessage>) -> Self {
        Self { tx }
    }
}

impl SystemInterface for TokioSystemInterface {
    fn send(&self, msg: SystemMessage, delay: Duration) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            sleep(delay).await;
            let _ = tx.send(msg);
        });
    }
}

pub struct TokioNode<P: NetPort> {
    node: Box<dyn Node>,
    event_port: P,
    general_port: P,
    event_rx: mpsc::UnboundedReceiver<EventMessage>,
    general_rx: mpsc::UnboundedReceiver<GeneralMessage>,
    system_rx: mpsc::UnboundedReceiver<SystemMessage>,
    timestamp: TimeStamp,
}

impl<P: NetPort> TokioNode<P> {
    pub async fn new<N, C>(event_port: P, general_port: P, ctor: C) -> std::io::Result<Self>
    where
        N: Node + 'static,
        C: FnOnce(TokioEventInterface, TokioGeneralInterface, TokioSystemInterface) -> N,
    {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (general_tx, general_rx) = mpsc::unbounded_channel();
        let (system_tx, system_rx) = mpsc::unbounded_channel();

        let node = Box::new(ctor(
            TokioEventInterface::new(event_tx),
            TokioGeneralInterface::new(general_tx),
            TokioSystemInterface::new(system_tx),
        ));

        Ok(Self {
            node,
            event_port,
            general_port,
            event_rx,
            general_rx,
            system_rx,
            timestamp: TimeStamp::new(0, 0),
        })
    }

    pub async fn master(event_port: P, general_port: P) -> std::io::Result<Self> {
        Self::new(event_port, general_port, |event, general, system| {
            MasterNode::new(event, general, system)
        })
        .await
    }

    pub async fn slave(event_port: P, general_port: P) -> std::io::Result<Self> {
        Self::new(event_port, general_port, |event, general, system| {
            SlaveNode::new(event, general, system)
        })
        .await
    }

    pub async fn run(self) -> std::io::Result<()> {
        self.run_until(std::future::pending::<()>()).await
    }

    pub async fn run_until<F>(mut self, shutdown: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()>,
    {
        let mut event_buf = [0u8; 2048];
        let mut general_buf = [0u8; 2048];

        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                recv = self.event_port.recv(&mut event_buf) => {
                    if let Ok((size, _peer)) = recv {
                        if let Ok(msg) = EventMessage::try_from(&event_buf[..size]) {
                            eprintln!("[event] recv {:?}", msg);
                            self.node.event_message(msg);
                        }
                    }
                }
                recv = self.general_port.recv(&mut general_buf) => {
                    if let Ok((size, _peer)) = recv {
                        if let Ok(msg) = GeneralMessage::try_from(&general_buf[..size]) {
                            eprintln!("[general] recv {:?}", msg);
                            self.node.general_message(msg);
                        }
                    }
                }
                msg = self.event_rx.recv() => {
                    if let Some(msg) = msg {
                        eprintln!("[event] send {:?}", msg);
                        let _ = self.event_port.send(msg.as_ref()).await;

                        self.node.system_message(SystemMessage::Timestamp {
                            msg,
                            timestamp: self.timestamp
                        });
                    }
                }
                msg = self.general_rx.recv() => {
                    if let Some(msg) = msg {
                        eprintln!("[general] send {:?}", msg);
                        let _ = self.general_port.send(msg.as_ref()).await;
                    }
                }
                msg = self.system_rx.recv() => {
                    if let Some(msg) = msg {
                        self.node.system_message(msg);
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    return Ok(());
                }
                _ = terminate() => {
                    return Ok(());
                }
                _ = &mut shutdown => {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(unix)]
async fn terminate() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sig = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    sig.recv().await;
}

#[cfg(not(unix))]
async fn terminate() {
    std::future::pending::<()>().await
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::net::FakeNetPort;
    use tokio::time;

    #[tokio::test(start_paused = true)]
    async fn master_node_sends_periodic_sync_follow_up() -> std::io::Result<()> {
        let (event_port, mut event_rx) = FakeNetPort::new();
        let (general_port, mut general_rx) = FakeNetPort::new();

        let node = TokioNode::master(event_port, general_port).await?;

        tokio::task::spawn(async move { node.run().await });

        let result = time::timeout(Duration::from_secs(10), async {
            let mut sync_count = 0;
            let mut follow_up_count = 0;

            loop {
                time::advance(Duration::from_secs(1)).await;

                while let Ok(msg) = event_rx.try_recv() {
                    if matches!(
                        EventMessage::try_from(msg.as_ref()),
                        Ok(EventMessage::TwoStepSync(_))
                    ) {
                        sync_count += 1;
                    }
                }
                while let Ok(msg) = general_rx.try_recv() {
                    if matches!(
                        GeneralMessage::try_from(msg.as_ref()),
                        Ok(GeneralMessage::FollowUp(_))
                    ) {
                        follow_up_count += 1;
                    }
                }

                if sync_count >= 5 && follow_up_count >= 5 {
                    return (sync_count, follow_up_count);
                }
            }
        })
        .await;

        let (sync_count, follow_up_count) = result.expect("timeout waiting for messages");
        assert_eq!(sync_count, follow_up_count);
        assert_eq!(sync_count, 5);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn slave_node_sends_periodic_delay_requests() -> std::io::Result<()> {
        let (event_port, mut event_rx) = FakeNetPort::new();
        let (general_port, _) = FakeNetPort::new();

        let node = TokioNode::slave(event_port, general_port).await?;

        tokio::task::spawn(async move { node.run().await });

        let result = time::timeout(Duration::from_secs(10), async {
            let mut delay_request_count = 0;

            loop {
                time::advance(Duration::from_secs(1)).await;

                while let Ok(msg) = event_rx.try_recv() {
                    if matches!(
                        EventMessage::try_from(msg.as_ref()),
                        Ok(EventMessage::DelayReq)
                    ) {
                        delay_request_count += 1;
                    }
                }

                if delay_request_count >= 5 {
                    return delay_request_count;
                }
            }
        })
        .await;

        let delay_request_count = result.expect("timeout waiting for delay requests");
        assert_eq!(delay_request_count, 5);
        Ok(())
    }
}
