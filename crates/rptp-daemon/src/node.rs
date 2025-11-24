use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rptp::bmca::IncrementalBmca;
use rptp::clock::SynchronizableClock;
use rptp::port::TimerHost;
use tokio::sync::mpsc;

use rptp::{
    clock::LocalClock,
    infra::infra_support::SortedForeignClockRecordsVec,
    message::{DomainMessage, EventMessage, SystemMessage, TimestampMessage},
    port::{DomainNumber, DomainPort, PhysicalPort, PortMap, SingleDomainPortMap, Timeout},
};

use crate::log::TracingPortLog;
use crate::net::NetworkSocket;

pub struct TokioTimeout {
    inner: Arc<TokioTimeoutInner>,
}

struct TokioTimeoutInner {
    domain_number: DomainNumber,
    tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    msg: Mutex<SystemMessage>,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TokioTimeout {
    fn new(
        domain_number: DomainNumber,
        tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
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
}

impl Drop for TokioTimeout {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub struct TokioTimerHost {
    domain_number: DomainNumber,
    tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
}

impl TokioTimerHost {
    pub fn new(
        domain_number: DomainNumber,
        tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    ) -> Self {
        Self { domain_number, tx }
    }
}

impl TimerHost for TokioTimerHost {
    type Timeout = TokioTimeout;

    fn timeout(&self, msg: SystemMessage, delay: Duration) -> Self::Timeout {
        TokioTimeout::new(self.domain_number, self.tx.clone(), msg, delay)
    }
}

pub struct TokioPhysicalPort<'a, C: SynchronizableClock, N: NetworkSocket> {
    clock: &'a LocalClock<C>,
    domain_number: DomainNumber,
    event_socket: Rc<N>,
    general_socket: Rc<N>,
    system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
}

impl<'a, C: SynchronizableClock, N: NetworkSocket> TokioPhysicalPort<'a, C, N> {
    pub fn new(
        clock: &'a LocalClock<C>,
        domain_number: DomainNumber,
        event_socket: Rc<N>,
        general_socket: Rc<N>,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    ) -> Self {
        Self {
            clock,
            domain_number,
            event_socket,
            general_socket,
            system_tx,
        }
    }
}

impl<'a, C: SynchronizableClock, N: NetworkSocket> PhysicalPort for TokioPhysicalPort<'a, C, N> {
    fn send_event(&self, buf: &[u8]) {
        let _ = self.event_socket.try_send(buf);

        let timestamp_msg = SystemMessage::Timestamp(TimestampMessage::new(
            EventMessage::try_from(buf).unwrap(),
            self.clock.now(),
        ));

        self.system_tx
            .send((self.domain_number, timestamp_msg))
            .ok();
    }

    fn send_general(&self, buf: &[u8]) {
        let _ = self.general_socket.try_send(buf);
    }
}

pub struct TokioPortsLoop<'a, C: SynchronizableClock, N: NetworkSocket> {
    local_clock: &'a LocalClock<C>,
    portmap: SingleDomainPortMap<
        Box<DomainPort<'a, C, TokioPhysicalPort<'a, C, N>, TokioTimerHost>>,
        IncrementalBmca<SortedForeignClockRecordsVec>,
        TracingPortLog,
    >,
    event_socket: Rc<N>,
    general_socket: Rc<N>,
    system_rx: mpsc::UnboundedReceiver<(DomainNumber, SystemMessage)>,
}

impl<'a, C: SynchronizableClock, N: NetworkSocket> TokioPortsLoop<'a, C, N> {
    pub async fn new(
        local_clock: &'a LocalClock<C>,
        portmap: SingleDomainPortMap<
            Box<DomainPort<'a, C, TokioPhysicalPort<'a, C, N>, TokioTimerHost>>,
            IncrementalBmca<SortedForeignClockRecordsVec>,
            TracingPortLog,
        >,
        event_socket: Rc<N>,
        general_socket: Rc<N>,
        system_rx: mpsc::UnboundedReceiver<(DomainNumber, SystemMessage)>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            local_clock,
            portmap,
            event_socket,
            general_socket,
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
            .port_by_domain(DomainNumber::new(0))
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

    use rptp::bmca::{DefaultDS, ParentTrackingBmca, Priority1, Priority2};
    use rptp::clock::{ClockIdentity, ClockQuality, FakeClock, StepsRemoved};
    use rptp::message::{EventMessage, GeneralMessage};
    use rptp::port::{
        DomainPort, ParentPortIdentity, Port, PortIdentity, PortNumber, PortTimingPolicy,
    };
    use rptp::portstate::PortState;
    use rptp::slave::{DelayCycle, SlavePort};

    use crate::log::TracingPortLog;
    use crate::net::{FakeNetworkSocket, MulticastSocket};

    #[test]
    fn tokio_node_state_size() {
        use std::mem::size_of;
        let s = size_of::<
            PortState<
                Box<
                    DomainPort<
                        'static,
                        FakeClock,
                        TokioPhysicalPort<'static, FakeClock, MulticastSocket>,
                        TokioTimerHost,
                    >,
                >,
                IncrementalBmca<SortedForeignClockRecordsVec>,
                TracingPortLog,
            >,
        >();
        println!("PortState<Box<TokioPort>> size: {}", s);
        assert!(s <= 512);
    }

    #[tokio::test(start_paused = true)]
    async fn master_node_sends_periodic_sync_follow_up() -> std::io::Result<()> {
        let (event_socket, mut event_socket_rx) = FakeNetworkSocket::new();
        let (general_socket, mut general_socket_rx) = FakeNetworkSocket::new();
        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let domain_number = DomainNumber::new(0);

        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
        );

        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let physical_port = TokioPhysicalPort::new(
            &local_clock,
            domain_number,
            event_socket.clone(),
            general_socket.clone(),
            system_tx.clone(),
        );
        let port_number = PortNumber::new(1);
        let domain_port = Box::new(DomainPort::new(
            &local_clock,
            physical_port,
            TokioTimerHost::new(domain_number, system_tx.clone()),
            domain_number,
            port_number,
        ));
        let bmca = IncrementalBmca::new(SortedForeignClockRecordsVec::new());
        let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
        let log = TracingPortLog::new(port_identity);
        let port_state = PortState::master(domain_port, bmca, log, PortTimingPolicy::default());
        let portmap = SingleDomainPortMap::new(domain_number, port_state);
        let portsloop = TokioPortsLoop::new(
            &local_clock,
            portmap,
            event_socket,
            general_socket,
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

        portsloop.run_until(cond).await?;

        assert_eq!(sync_count, follow_up_count);
        assert_eq!(sync_count, 5);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn slave_node_sends_periodic_delay_requests() -> std::io::Result<()> {
        let (event_socket, mut event_socket_rx) = FakeNetworkSocket::new();
        let (general_socket, _) = FakeNetworkSocket::new();
        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let domain_number = DomainNumber::new(0);

        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
        );

        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let physical_port = TokioPhysicalPort::new(
            &local_clock,
            domain_number,
            event_socket.clone(),
            general_socket.clone(),
            system_tx.clone(),
        );
        let port_number = PortNumber::new(1);
        let domain_port = Box::new(DomainPort::new(
            &local_clock,
            physical_port,
            TokioTimerHost::new(domain_number, system_tx.clone()),
            domain_number,
            port_number,
        ));
        let parent_port_identity = ParentPortIdentity::new(PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            PortNumber::new(1),
        ));
        let bmca = ParentTrackingBmca::new(
            IncrementalBmca::new(SortedForeignClockRecordsVec::new()),
            parent_port_identity,
        );
        let announce_receipt_timeout = domain_port.timeout(
            SystemMessage::AnnounceReceiptTimeout,
            Duration::from_secs(10),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout);

        let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
        let log = TracingPortLog::new(port_identity);
        let port_state = PortState::Slave(SlavePort::new(
            domain_port,
            bmca,
            parent_port_identity,
            announce_receipt_timeout,
            delay_cycle,
            log,
            PortTimingPolicy::default(),
        ));
        let portmap = SingleDomainPortMap::new(domain_number, port_state);
        let portsloop = TokioPortsLoop::new(
            &local_clock,
            portmap,
            event_socket,
            general_socket,
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

        portsloop.run_until(cond).await?;

        assert!(delay_request_count >= 5);
        Ok(())
    }
}
