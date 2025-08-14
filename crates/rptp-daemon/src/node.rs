use std::net::Ipv4Addr;

use tokio::sync::mpsc;

use rptp::{
    message::{EventMessage, GeneralMessage, SystemMessage},
    node::{EventInterface, GeneralInterface, MasterNode, Node, SlaveNode, SystemInterface},
};

use crate::net::MulticastPort;

const PTP_MCAST: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 129);
const EVENT_PORT: u16 = 5319;
const GENERAL_PORT: u16 = 5320;

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

impl EventInterface for &TokioEventInterface {
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

impl GeneralInterface for &TokioGeneralInterface {
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
    fn send(&self, msg: SystemMessage) {
        let _ = self.tx.send(msg);
    }
}

impl SystemInterface for &TokioSystemInterface {
    fn send(&self, msg: SystemMessage) {
        let _ = self.tx.send(msg);
    }
}

pub struct TokioNode {
    node: Box<dyn Node>,
    event_port: MulticastPort,
    general_port: MulticastPort,
    event_rx: mpsc::UnboundedReceiver<EventMessage>,
    general_rx: mpsc::UnboundedReceiver<GeneralMessage>,
    system_rx: mpsc::UnboundedReceiver<SystemMessage>,
    timestamp_counter: u64,
}

impl TokioNode {
    pub async fn new<N, C>(ctor: C) -> std::io::Result<Self>
    where
        N: Node + 'static,
        C: FnOnce(TokioEventInterface, TokioGeneralInterface, TokioSystemInterface) -> N,
    {
        let event_port = MulticastPort::bind_v4(PTP_MCAST, EVENT_PORT).await?;
        let general_port = MulticastPort::bind_v4(PTP_MCAST, GENERAL_PORT).await?;

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
            timestamp_counter: 0,
        })
    }

    pub async fn master() -> std::io::Result<Self> {
        Self::new(|event, general, system| MasterNode::new(event, general, system)).await
    }

    pub async fn slave() -> std::io::Result<Self> {
        Self::new(|event, general, system| SlaveNode::new(event, general, system)).await
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
                            timestamp: self.timestamp_counter
                        });

                        self.timestamp_counter = self.timestamp_counter.wrapping_add(1);
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
