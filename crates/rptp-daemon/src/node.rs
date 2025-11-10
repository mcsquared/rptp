use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rptp::bmca::FullBmca;
use rptp::port::TimerHost;
use tokio::sync::mpsc;

use rptp::{
    clock::{FakeClock, LocalClock},
    infra::infra_support::SortedForeignClockRecordsVec,
    message::{DomainMessage, EventMessage, GeneralMessage, SystemMessage},
    port::{DomainPort, PhysicalPort, PortMap, SingleDomainPortMap, Timeout},
};

use crate::net::NetworkSocket;

pub struct TokioTimeout {
    inner: Arc<TokioTimeoutInner>,
}

struct TokioTimeoutInner {
    domain_number: u8,
    tx: mpsc::UnboundedSender<(u8, SystemMessage)>,
    msg: Mutex<SystemMessage>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TokioTimeout {
    fn new(
        domain_number: u8,
        tx: mpsc::UnboundedSender<(u8, SystemMessage)>,
        msg: SystemMessage,
        delay: Duration,
    ) -> Self {
        let inner = Arc::new(TokioTimeoutInner {
            domain_number,
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
            let _ = inner.tx.send((inner.domain_number, msg));
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
}

impl Drop for TokioTimeout {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub struct TokioTimerHost {
    domain_number: u8,
    tx: mpsc::UnboundedSender<(u8, SystemMessage)>,
}

impl TokioTimerHost {
    pub fn new(domain_number: u8, tx: mpsc::UnboundedSender<(u8, SystemMessage)>) -> Self {
        Self { domain_number, tx }
    }
}

impl TimerHost for TokioTimerHost {
    type Timeout = TokioTimeout;

    fn timeout(&self, msg: SystemMessage, delay: Duration) -> Self::Timeout {
        TokioTimeout::new(self.domain_number, self.tx.clone(), msg, delay)
    }
}

pub struct TokioPhysicalPort {
    domain_number: u8,
    event_tx: mpsc::UnboundedSender<(u8, EventMessage)>,
    general_tx: mpsc::UnboundedSender<GeneralMessage>,
}

impl TokioPhysicalPort {
    pub fn new(
        domain_number: u8,
        event_tx: mpsc::UnboundedSender<(u8, EventMessage)>,
        general_tx: mpsc::UnboundedSender<GeneralMessage>,
    ) -> Self {
        Self {
            domain_number,
            event_tx,
            general_tx,
        }
    }
}

impl PhysicalPort for TokioPhysicalPort {
    fn send_event(&self, msg: EventMessage) {
        eprintln!("[event] send {:?}", msg);
        let _ = self.event_tx.send((self.domain_number, msg));
    }

    fn send_general(&self, msg: GeneralMessage) {
        eprintln!("[general] send {:?}", msg);
        let _ = self.general_tx.send(msg);
    }
}

pub struct TokioNetwork<'a, N: NetworkSocket> {
    local_clock: &'a LocalClock<Rc<FakeClock>>,
    portmap: SingleDomainPortMap<
        DomainPort<
            'a,
            Rc<FakeClock>,
            FullBmca<SortedForeignClockRecordsVec>,
            TokioPhysicalPort,
            TokioTimerHost,
        >,
    >,
    event_socket: N,
    general_socket: N,
    event_rx: mpsc::UnboundedReceiver<(u8, EventMessage)>,
    general_rx: mpsc::UnboundedReceiver<GeneralMessage>,
    system_rx: mpsc::UnboundedReceiver<(u8, SystemMessage)>,
}

impl<'a, N: NetworkSocket> TokioNetwork<'a, N> {
    pub async fn new(
        local_clock: &'a LocalClock<Rc<FakeClock>>,
        event_socket: N,
        general_socket: N,
        portmap: SingleDomainPortMap<
            DomainPort<
                'a,
                Rc<FakeClock>,
                FullBmca<SortedForeignClockRecordsVec>,
                TokioPhysicalPort,
                TokioTimerHost,
            >,
        >,
        event_rx: mpsc::UnboundedReceiver<(u8, EventMessage)>,
        general_rx: mpsc::UnboundedReceiver<GeneralMessage>,
        system_rx: mpsc::UnboundedReceiver<(u8, SystemMessage)>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            local_clock,
            portmap,
            event_socket,
            general_socket,
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

        let port = self
            .portmap
            .port_by_domain(0)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "no port for domain 0"))?;
        port.process_system_message(SystemMessage::Initialized);

        loop {
            tokio::select! {
                recv = self.event_socket.recv(&mut event_buf) => {
                    if let Ok((size, _peer)) = recv {
                        let domain_msg = DomainMessage::new(&event_buf[..size]);
                        let _ = domain_msg.dispatch_event(&mut self.portmap, self.local_clock.now());
                    }
                }
                recv = self.general_socket.recv(&mut general_buf) => {
                    if let Ok((size, _peer)) = recv {
                        let domain_msg = DomainMessage::new(&general_buf[..size]);
                        let _ = domain_msg.dispatch_general(&mut self.portmap);
                    }
                }
                msg = self.event_rx.recv() => {
                    if let Some((domain_number, msg)) = msg {
                        eprintln!("[event] send {:?}", msg);
                        let _ = self.event_socket.send(msg.to_wire().as_ref()).await;

                        let timestamp_msg = SystemMessage::Timestamp {
                            msg,
                            timestamp: self.local_clock.now(),
                        };
                        self.portmap.port_by_domain(domain_number).and_then(|port| {
                            port.process_system_message(timestamp_msg);
                            Ok(())
                        }).ok();
                    }
                }
                msg = self.general_rx.recv() => {
                    if let Some(msg) = msg {
                        eprintln!("[general] send {:?}", msg);
                        let _ = self.general_socket.send(msg.to_wire().as_ref()).await;
                    }
                }
                msg = self.system_rx.recv() => {
                    if let Some((domain_number, msg)) = msg {
                        self.portmap.port_by_domain(domain_number).and_then(|port| {
                            port.process_system_message(msg);
                            Ok(())
                        }).ok();
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

    use rptp::bmca::LocalClockDS;
    use rptp::clock::{ClockIdentity, ClockQuality, FakeClock};
    use rptp::message::{DelayCycleMessage, SyncCycleMessage};
    use rptp::port::{DomainPort, Port};
    use rptp::portstate::PortState;

    use crate::net::FakeNetworkSocket;

    #[test]
    fn tokio_node_state_size() {
        use std::mem::size_of;
        let s = size_of::<
            PortState<
                DomainPort<
                    '_,
                    Rc<FakeClock>,
                    FullBmca<SortedForeignClockRecordsVec>,
                    Box<TokioPhysicalPort>,
                    TokioTimerHost,
                >,
            >,
        >();
        println!("PortState<Box<TokioPort>> size: {}", s);
        assert!(s <= 256);
    }

    #[tokio::test(start_paused = true)]
    async fn master_node_sends_periodic_sync_follow_up() -> std::io::Result<()> {
        let (event_socket, mut event_socket_rx) = FakeNetworkSocket::new();
        let (general_socket, mut general_socket_rx) = FakeNetworkSocket::new();

        let domain_number = 0;

        let local_clock = LocalClock::new(
            Rc::new(FakeClock::default()),
            LocalClockDS::new(
                ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
        );

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (general_tx, general_rx) = mpsc::unbounded_channel();
        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let physical_port = TokioPhysicalPort::new(domain_number, event_tx, general_tx);
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            physical_port,
            TokioTimerHost::new(domain_number, system_tx.clone()),
            0,
        );
        let announce_send_timeout =
            domain_port.timeout(SystemMessage::AnnounceSendTimeout, Duration::from_secs(1));
        let sync_cycle_timeout = domain_port.timeout(
            SystemMessage::SyncCycle(SyncCycleMessage::new(0.into())),
            Duration::from_secs(0),
        );
        let port_state = PortState::master(domain_port, announce_send_timeout, sync_cycle_timeout);
        let portmap = SingleDomainPortMap::new(domain_number, port_state);
        let net = TokioNetwork::new(
            &local_clock,
            event_socket,
            general_socket,
            portmap,
            event_rx,
            general_rx,
            system_rx,
        )
        .await?;

        let mut sync_count = 0;
        let mut follow_up_count = 0;

        let cond = time::timeout(Duration::from_secs(10), async {
            loop {
                time::advance(Duration::from_millis(100)).await;

                while let Ok(msg) = event_socket_rx.try_recv() {
                    if matches!(
                        EventMessage::try_from(msg.as_ref()),
                        Ok(EventMessage::TwoStepSync(_))
                    ) {
                        sync_count += 1;
                    }
                }
                while let Ok(msg) = general_socket_rx.try_recv() {
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

        net.run_until(cond).await?;

        assert_eq!(sync_count, follow_up_count);
        assert_eq!(sync_count, 5);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn slave_node_sends_periodic_delay_requests() -> std::io::Result<()> {
        let (event_socket, mut event_socket_rx) = FakeNetworkSocket::new();
        let (general_socket, _) = FakeNetworkSocket::new();

        let domain_number = 0;

        let local_clock = LocalClock::new(
            Rc::new(FakeClock::default()),
            LocalClockDS::new(
                ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
        );

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (general_tx, general_rx) = mpsc::unbounded_channel();
        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let physical_port = TokioPhysicalPort::new(domain_number, event_tx, general_tx);
        let domain_port = DomainPort::new(
            &local_clock,
            FullBmca::new(SortedForeignClockRecordsVec::new()),
            physical_port,
            TokioTimerHost::new(domain_number, system_tx.clone()),
            0,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(10),
        );
        let delay_cycle_timeout = domain_port.timeout(
            SystemMessage::DelayCycle(DelayCycleMessage::new(0.into())),
            Duration::from_secs(1),
        );
        let port_state =
            PortState::slave(domain_port, announce_receipt_timeout, delay_cycle_timeout);
        let portmap = SingleDomainPortMap::new(domain_number, port_state);
        let net = TokioNetwork::new(
            &local_clock,
            event_socket,
            general_socket,
            portmap,
            event_rx,
            general_rx,
            system_rx,
        )
        .await?;

        let mut delay_request_count = 0;

        let cond = time::timeout(Duration::from_secs(10), async {
            loop {
                time::advance(Duration::from_millis(100)).await;

                while let Ok(msg) = event_socket_rx.try_recv() {
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

        net.run_until(cond).await?;

        assert!(delay_request_count >= 5);
        Ok(())
    }
}
