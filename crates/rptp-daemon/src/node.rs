use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rptp::bmca::{Bmca, ForeignClockRecord};
use rptp::clock::{ClockIdentity, ClockQuality};
use tokio::sync::mpsc;

use rptp::{
    bmca::LocalClockDS,
    clock::{Clock, LocalClock, SynchronizableClock},
    infra::infra_support::SortedForeignClockRecordsVec,
    message::{EventMessage, GeneralMessage, SystemMessage},
    node::{InitializingNode, MasterNode, NodeState, SlaveNode},
    port::{DropTimeout, Port, Timeout},
};

use crate::net::NetPort;

pub struct TokioTimeout {
    inner: Arc<TokioTimeoutInner>,
}

struct TokioTimeoutInner {
    tx: mpsc::UnboundedSender<SystemMessage>,
    msg: Mutex<SystemMessage>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TokioTimeout {
    fn new(tx: mpsc::UnboundedSender<SystemMessage>, msg: SystemMessage, delay: Duration) -> Self {
        let inner = Arc::new(TokioTimeoutInner {
            tx,
            msg: Mutex::new(msg),
            handle: Mutex::new(None),
        });

        let timeout = Self { inner };
        timeout.reset(delay);
        timeout
    }

    fn reset(&self, delay: Duration) {
        let msg = *self.inner.msg.lock().unwrap();
        let mut guard = self.inner.handle.lock().unwrap();

        if let Some(handle) = guard.take() {
            handle.abort();
        }
        *guard = Some(Self::spawn(Arc::clone(&self.inner), msg, delay));
    }

    fn spawn(
        inner: Arc<TokioTimeoutInner>,
        msg: SystemMessage,
        delay: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = inner.tx.send(msg);
        })
    }

    fn cancel(&self) {
        if let Some(handle) = self.inner.handle.lock().unwrap().take() {
            handle.abort();
        }
    }
}

impl Timeout for TokioTimeout {
    fn restart(&self, delay: Duration) {
        self.reset(delay);
    }

    fn restart_with(&self, msg: SystemMessage, delay: Duration) {
        *self.inner.msg.lock().unwrap() = msg;
        self.reset(delay);
    }

    fn cancel(&self) {
        if let Some(handle) = self.inner.handle.lock().unwrap().take() {
            handle.abort();
        }
    }
}

impl Drop for TokioTimeout {
    fn drop(&mut self) {
        self.cancel();
    }
}

struct TokioPort {
    clock: LocalClock<Rc<dyn SynchronizableClock>>,
    event_tx: mpsc::UnboundedSender<EventMessage>,
    general_tx: mpsc::UnboundedSender<GeneralMessage>,
    system_tx: mpsc::UnboundedSender<SystemMessage>,
}

impl TokioPort {
    fn new(
        clock: LocalClock<Rc<dyn SynchronizableClock>>,
        event_tx: mpsc::UnboundedSender<EventMessage>,
        general_tx: mpsc::UnboundedSender<GeneralMessage>,
        system_tx: mpsc::UnboundedSender<SystemMessage>,
    ) -> Self {
        Self {
            clock,
            event_tx,
            general_tx,
            system_tx,
        }
    }
}

impl Port for TokioPort {
    type Clock = Rc<dyn SynchronizableClock>;
    type ClockRecords = Box<SortedForeignClockRecordsVec>;
    type Timeout = TokioTimeout;

    fn clock(&self) -> &LocalClock<Self::Clock> {
        &self.clock
    }

    fn foreign_clock_records(&self, records: &[ForeignClockRecord]) -> Self::ClockRecords {
        Box::new(SortedForeignClockRecordsVec::from_records(records))
    }

    fn send_event(&self, msg: EventMessage) {
        let _ = self.event_tx.send(msg);
    }

    fn send_general(&self, msg: GeneralMessage) {
        let _ = self.general_tx.send(msg);
    }

    fn schedule(&self, msg: SystemMessage, delay: Duration) -> Self::Timeout {
        let tx = self.system_tx.clone();
        TokioTimeout::new(tx, msg, delay)
    }
}

pub struct TokioNode<P: NetPort> {
    node: NodeState<Box<TokioPort>>,
    clock: Rc<dyn SynchronizableClock>,
    event_port: P,
    general_port: P,
    event_rx: mpsc::UnboundedReceiver<EventMessage>,
    general_rx: mpsc::UnboundedReceiver<GeneralMessage>,
    system_rx: mpsc::UnboundedReceiver<SystemMessage>,
}

impl<P: NetPort> TokioNode<P> {
    pub async fn initializing(
        clock: Rc<dyn SynchronizableClock>,
        event_port: P,
        general_port: P,
        localds: LocalClockDS,
    ) -> std::io::Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (general_tx, general_rx) = mpsc::unbounded_channel();
        let (system_tx, system_rx) = mpsc::unbounded_channel();

        let node = NodeState::Initializing(InitializingNode::new(Box::new(TokioPort::new(
            LocalClock::new(clock.clone(), localds),
            event_tx,
            general_tx,
            system_tx,
        ))));

        Ok(Self {
            node: node.system_message(SystemMessage::Initialized),
            clock,
            event_port,
            general_port,
            event_rx,
            general_rx,
            system_rx,
        })
    }

    pub async fn master(
        clock: Rc<dyn SynchronizableClock>,
        event_port: P,
        general_port: P,
    ) -> std::io::Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (general_tx, general_rx) = mpsc::unbounded_channel();
        let (system_tx, system_rx) = mpsc::unbounded_channel();

        let port = Box::new(TokioPort::new(
            LocalClock::new(
                clock.clone(),
                LocalClockDS::new(
                    ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
                    ClockQuality::new(248, 0xFE, 0xFFFF),
                ),
            ),
            event_tx,
            general_tx,
            system_tx,
        ));
        let bmca = Bmca::new(port.foreign_clock_records(&[]));

        let node = NodeState::Master(MasterNode::new(port, bmca));

        Ok(Self {
            node,
            clock,
            event_port,
            general_port,
            event_rx,
            general_rx,
            system_rx,
        })
    }

    pub async fn slave(
        clock: Rc<dyn SynchronizableClock>,
        event_port: P,
        general_port: P,
    ) -> std::io::Result<Self> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (general_tx, general_rx) = mpsc::unbounded_channel();
        let (system_tx, system_rx) = mpsc::unbounded_channel();

        let port = Box::new(TokioPort::new(
            LocalClock::new(
                clock.clone(),
                LocalClockDS::new(
                    ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
                    ClockQuality::new(255, 0xFF, 0xFFFF),
                ),
            ),
            event_tx,
            general_tx,
            system_tx,
        ));
        let bmca = Bmca::new(port.foreign_clock_records(&[]));
        let announce_receipt_timeout = DropTimeout::new(port.schedule(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(30), // TODO: this is a hack to avoid tests running into timeouts
                                     // -> long term solution would be to have Bmca trait and
                                     // instantiate a slave-only Bmca here.
        ));

        let node = NodeState::Slave(SlaveNode::new(port, bmca, announce_receipt_timeout));

        Ok(Self {
            node,
            clock,
            event_port,
            general_port,
            event_rx,
            general_rx,
            system_rx,
        })
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
                            self.node = self.node.event_message(msg, self.clock.now());
                        }
                    }
                }
                recv = self.general_port.recv(&mut general_buf) => {
                    if let Ok((size, _peer)) = recv {
                        if let Ok(msg) = GeneralMessage::try_from(&general_buf[..size]) {
                            eprintln!("[general] recv {:?}", msg);
                            self.node = self.node.general_message(msg);
                        }
                    }
                }
                msg = self.event_rx.recv() => {
                    if let Some(msg) = msg {
                        eprintln!("[event] send {:?}", msg);
                        let _ = self.event_port.send(msg.to_wire().as_ref()).await;

                        self.node = self.node.system_message(SystemMessage::Timestamp {
                            msg,
                            timestamp: self.clock.now(),
                        });
                    }
                }
                msg = self.general_rx.recv() => {
                    if let Some(msg) = msg {
                        eprintln!("[general] send {:?}", msg);
                        let _ = self.general_port.send(msg.to_wire().as_ref()).await;
                    }
                }
                msg = self.system_rx.recv() => {
                    if let Some(msg) = msg {
                        self.node = self.node.system_message(msg);
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

    use futures::FutureExt;
    use tokio::time;

    use rptp::clock::FakeClock;

    use crate::net::FakeNetPort;

    #[test]
    fn tokio_node_state_size() {
        use std::mem::size_of;
        let s = size_of::<NodeState<Box<TokioPort>>>();
        println!("NodeState<Box<TokioPort>> size: {}", s);
        assert!(s <= 256);
    }

    #[tokio::test(start_paused = true)]
    async fn master_node_sends_periodic_sync_follow_up() -> std::io::Result<()> {
        let (event_port, mut event_rx) = FakeNetPort::new();
        let (general_port, mut general_rx) = FakeNetPort::new();

        let clock = Rc::new(FakeClock::default());
        let node = TokioNode::master(clock, event_port, general_port).await?;

        let mut sync_count = 0;
        let mut follow_up_count = 0;

        let cond = time::timeout(Duration::from_secs(10), async {
            loop {
                time::advance(Duration::from_millis(100)).await;

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
                    return;
                }
            }
        })
        .map(|_| {});

        node.run_until(cond).await?;

        assert_eq!(sync_count, follow_up_count);
        assert_eq!(sync_count, 5);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn slave_node_sends_periodic_delay_requests() -> std::io::Result<()> {
        let (event_port, mut event_rx) = FakeNetPort::new();
        let (general_port, _) = FakeNetPort::new();

        let clock = Rc::new(FakeClock::default());
        let node = TokioNode::slave(clock, event_port, general_port).await?;

        let mut delay_request_count = 0;

        let cond = time::timeout(Duration::from_secs(10), async {
            loop {
                time::advance(Duration::from_millis(100)).await;

                while let Ok(msg) = event_rx.try_recv() {
                    if matches!(
                        EventMessage::try_from(msg.as_ref()),
                        Ok(EventMessage::DelayReq(_))
                    ) {
                        delay_request_count += 1;
                    }
                }

                if delay_request_count >= 5 {
                    return;
                }
            }
        })
        .map(|_result| {});

        node.run_until(cond).await?;

        assert!(delay_request_count >= 5);
        Ok(())
    }
}
