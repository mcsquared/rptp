use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant as StdInstant;

use tokio::sync::mpsc;

use rptp::wire::UnvalidatedMessage;
use rptp::{
    message::{DomainMessage, SystemMessage},
    port::{DomainNumber, PhysicalPort, PortMap, SendResult, Timeout, TimerHost},
    result::{Error as RptpError, ProtocolError},
    time::{Duration, Instant},
};

use crate::net::NetworkSocket;
use crate::timestamping::RxTimestamping;

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
            let delay = std::time::Duration::from_nanos(delay.as_u64_nanos());
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

pub struct TokioPhysicalPort<N: NetworkSocket> {
    event_socket: Rc<N>,
    general_socket: Rc<N>,
}

impl<N: NetworkSocket> TokioPhysicalPort<N> {
    pub fn new(event_socket: Rc<N>, general_socket: Rc<N>) -> Self {
        Self {
            event_socket,
            general_socket,
        }
    }
}

impl<N: NetworkSocket> PhysicalPort for TokioPhysicalPort<N> {
    fn send_event(&self, buf: &[u8]) -> SendResult {
        match self.event_socket.try_send(buf) {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    target: "rptp::tx::event",
                    error = %e,
                    "event socket send error"
                );
                Err(rptp::port::SendError)
            }
        }
    }

    fn send_general(&self, buf: &[u8]) -> SendResult {
        match self.general_socket.try_send(buf) {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    target: "rptp::tx::general",
                    error = %e,
                    "general socket send error"
                );
                Err(rptp::port::SendError)
            }
        }
    }
}

enum RxKind {
    Event,
    General,
}

pub struct TokioPortsLoop<P, N: NetworkSocket, R: RxTimestamping> {
    portmap: P,
    event_socket: Rc<N>,
    general_socket: Rc<N>,
    timestamping: R,
    system_rx: mpsc::UnboundedReceiver<(DomainNumber, SystemMessage)>,
}

impl<P, N, R> TokioPortsLoop<P, N, R>
where
    P: PortMap,
    N: NetworkSocket,
    R: RxTimestamping,
{
    pub async fn new(
        portmap: P,
        event_socket: Rc<N>,
        general_socket: Rc<N>,
        timestamping: R,
        system_rx: mpsc::UnboundedReceiver<(DomainNumber, SystemMessage)>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            portmap,
            event_socket,
            general_socket,
            timestamping,
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
        let start = StdInstant::now();

        let mut event_buf = [0u8; 2048];
        let mut general_buf = [0u8; 2048];

        tokio::pin!(shutdown);

        let port = self
            .portmap
            .port_by_domain(DomainNumber::new(0))
            .map_err(|_| std::io::Error::other("no port for domain 0"))?;
        port.process_system_message(SystemMessage::Initialized);

        loop {
            tokio::select! {
                recv = self.event_socket.recv(&mut event_buf) => {
                    if let Ok((size, _peer)) = recv {
                        let now = Instant::from_nanos(start.elapsed().as_nanos() as u64);
                        self.process_datagram(RxKind::Event, &event_buf, size, now);
                    } else if let Err(e) = recv {
                        tracing::warn!(target: "rptp::rx::event", error = %e, "event socket receive error");
                        // TODO: extended & more granular receive error handling
                    }
                }
                recv = self.general_socket.recv(&mut general_buf) => {
                    let now = Instant::from_nanos(start.elapsed().as_nanos() as u64);
                    if let Ok((size, _peer)) = recv {
                        self.process_datagram(RxKind::General, &general_buf, size, now);
                    } else if let Err(e) = recv {
                        tracing::warn!(target: "rptp::rx::general", error = %e, "general socket receive error");
                        // TODO: extended & more granular receive error handling
                    }
                }
                msg = self.system_rx.recv() => {
                    if let Some((domain_number, msg)) = msg {
                        self.portmap.port_by_domain(domain_number).map(|port| {
                            port.process_system_message(msg);
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

    fn process_datagram(&mut self, kind: RxKind, buf: &[u8], size: usize, now: Instant) {
        let label = match kind {
            RxKind::Event => "event",
            RxKind::General => "general",
        };

        let length_checked = match UnvalidatedMessage::new(&buf[..size]).length_checked_v2() {
            Ok(msg) => msg,
            Err(e) => {
                tracing::trace!(
                    target: "rptp::rx",
                    error = %e,
                    "dropping malformed {label} message"
                );
                return;
            }
        };

        let domain_msg = DomainMessage::new(length_checked);
        let res = match kind {
            RxKind::Event => {
                domain_msg.dispatch_event(&mut self.portmap, self.timestamping.ingress_stamp())
            }
            RxKind::General => domain_msg.dispatch_general(&mut self.portmap, now),
        };

        if let Err(e) = res {
            self.log_rx_error(label, &e);
        }
    }

    fn log_rx_error(&self, label: &str, e: &RptpError) {
        match e {
            RptpError::Protocol(ProtocolError::DomainNotFound(_)) => tracing::trace!(
                target: "rptp::rx",
                error = %e,
                "dropping {label} message for unknown domain"
            ),
            RptpError::Protocol(ProtocolError::UnsupportedPtpVersion(_)) => tracing::trace!(
                target: "rptp::rx",
                error = %e,
                "dropping {label} message with unsupported PTP version"
            ),
            RptpError::Protocol(ProtocolError::UnknownMessageType(_)) => tracing::trace!(
                target: "rptp::rx",
                error = %e,
                "dropping {label} message with unknown type"
            ),
            RptpError::Parse(_) => tracing::trace!(
                target: "rptp::rx",
                error = %e,
                "dropping {label} message with parse error"
            ),
            _ => tracing::debug!(
                target: "rptp::rx",
                error = %e,
                "{label} message domain/protocol error"
            ),
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

    use rptp::bmca::{
        DefaultDS, LocalMasterTrackingBmca, ParentTrackingBmca, Priority1, Priority2,
    };
    use rptp::clock::{ClockIdentity, ClockQuality, LocalClock, StepsRemoved, SynchronizableClock};
    use rptp::infra::infra_support::SortedForeignClockRecordsVec;
    use rptp::log::NOOP_CLOCK_METRICS;
    use rptp::message::{EventMessage, GeneralMessage, OneStepSyncMessage, TwoStepSyncMessage};
    use rptp::port::{
        AnnounceReceiptTimeout, DomainPort, ParentPortIdentity, Port, PortIdentity, PortNumber,
    };
    use rptp::portstate::{PortProfile, PortState};
    use rptp::servo::{Servo, SteppingServo};
    use rptp::slave::{DelayCycle, SlavePort};
    use rptp::test_support::FakeClock;
    use rptp::test_support::FakeTimestamping;
    use rptp::time::{LogInterval, TimeStamp};
    use rptp::wire::{MessageBuffer, PtpVersion, TransportSpecific};

    use crate::log::TracingPortLog;
    use crate::net::{FakeNetworkSocket, MulticastSocket, NetworkSocket};
    use crate::timestamping::ClockTimestamping;
    use crate::virtualclock::VirtualClock;

    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::time::Duration as StdDuration;

    use rptp::bmca::IncrementalBmca;
    use rptp::port::SingleDomainPortMap;

    type TestPortMap<'a, C, N> = SingleDomainPortMap<
        Box<DomainPort<'a, C, TokioPhysicalPort<N>, TokioTimerHost, FakeTimestamping>>,
        IncrementalBmca<SortedForeignClockRecordsVec>,
        TracingPortLog,
    >;

    type TestPortsLoop<'a, C, N> = TokioPortsLoop<TestPortMap<'a, C, N>, N, FakeTimestamping>;

    struct MasterTestNode<'a, C: SynchronizableClock, N: NetworkSocket> {
        portsloop: TestPortsLoop<'a, C, N>,
    }

    impl<'a, C: SynchronizableClock, N: NetworkSocket> MasterTestNode<'a, C, N> {
        async fn new(
            local_clock: &'a LocalClock<C>,
            event_socket: Rc<N>,
            general_socket: Rc<N>,
        ) -> std::io::Result<Self> {
            let domain_number = DomainNumber::new(0);

            let (system_tx, system_rx) = mpsc::unbounded_channel();
            let tx_timestamping = FakeTimestamping::new();
            let physical_port =
                TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
            let port_number = PortNumber::new(1);
            let domain_port = Box::new(DomainPort::new(
                local_clock,
                physical_port,
                TokioTimerHost::new(domain_number, system_tx.clone()),
                tx_timestamping,
                domain_number,
                port_number,
            ));
            let bmca = LocalMasterTrackingBmca::new(IncrementalBmca::new(
                SortedForeignClockRecordsVec::new(),
            ));
            let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
            let log = TracingPortLog::new(port_identity);
            let port_state = PortProfile::default().master(domain_port, bmca, log);
            let portmap = SingleDomainPortMap::new(domain_number, port_state);

            let rx_timestamping = FakeTimestamping::new();

            let portsloop = TokioPortsLoop::new(
                portmap,
                event_socket,
                general_socket,
                rx_timestamping,
                system_rx,
            )
            .await?;

            Ok(Self { portsloop })
        }

        async fn run_until<F>(self, shutdown: F) -> std::io::Result<()>
        where
            F: std::future::Future<Output = ()>,
        {
            self.portsloop.run_until(shutdown).await
        }
    }

    #[test]
    fn tokio_node_state_size() {
        use std::mem::size_of;
        let s = size_of::<
            PortState<
                Box<
                    DomainPort<
                        'static,
                        FakeClock,
                        TokioPhysicalPort<MulticastSocket>,
                        TokioTimerHost,
                        ClockTimestamping<FakeClock>,
                    >,
                >,
                IncrementalBmca<SortedForeignClockRecordsVec>,
                TracingPortLog,
            >,
        >();
        println!("PortState<Box<TokioPort>> size: {}", s);
        assert!(s <= 512);
    }

    impl RxTimestamping for FakeTimestamping {
        fn ingress_stamp(&self) -> TimeStamp {
            TimeStamp::new(0, 0)
        }
    }

    struct InjectingNetworkSocket {
        queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
    }

    impl InjectingNetworkSocket {
        fn new() -> (Self, Arc<Mutex<VecDeque<Vec<u8>>>>) {
            let queue = Arc::new(Mutex::new(VecDeque::new()));
            (
                Self {
                    queue: queue.clone(),
                },
                queue,
            )
        }
    }

    impl NetworkSocket for InjectingNetworkSocket {
        async fn recv(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
            loop {
                if let Some(msg) = {
                    let mut q = self.queue.lock().unwrap();
                    q.pop_front()
                } {
                    let len = msg.len().min(buf.len());
                    buf[..len].copy_from_slice(&msg[..len]);
                    return Ok((len, SocketAddr::from(([127, 0, 0, 1], 0))));
                } else {
                    time::sleep(StdDuration::from_millis(1)).await;
                }
            }
        }

        async fn send(&self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }

        fn try_send(&self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn master_node_sends_periodic_sync_follow_up() -> std::io::Result<()> {
        let (event_socket, mut event_socket_rx) = FakeNetworkSocket::new();
        let (general_socket, mut general_socket_rx) = FakeNetworkSocket::new();
        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let domain_number = DomainNumber::new(0);

        let virtual_clock = VirtualClock::new(TimeStamp::new(0, 0), 1.0);
        let local_clock = LocalClock::new(
            &virtual_clock,
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let timestamping = ClockTimestamping::new(&virtual_clock, system_tx.clone(), domain_number);
        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
        let port_number = PortNumber::new(1);
        let domain_port = Box::new(DomainPort::new(
            &local_clock,
            physical_port,
            TokioTimerHost::new(domain_number, system_tx.clone()),
            &timestamping,
            domain_number,
            port_number,
        ));
        let bmca =
            LocalMasterTrackingBmca::new(IncrementalBmca::new(SortedForeignClockRecordsVec::new()));
        let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
        let log = TracingPortLog::new(port_identity);
        let port_state = PortProfile::default().master(domain_port, bmca, log);
        let portmap = SingleDomainPortMap::new(domain_number, port_state);

        let rx_timestamping = FakeTimestamping::new();

        let portsloop = TokioPortsLoop::new(
            portmap,
            event_socket,
            general_socket,
            rx_timestamping,
            system_rx,
        )
        .await?;

        let mut sync_count = 0;
        let mut follow_up_count = 0;

        let cond = time::timeout(StdDuration::from_secs(10), async {
            loop {
                time::advance(StdDuration::from_millis(100)).await;

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
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
        let port_number = PortNumber::new(1);
        let domain_port = Box::new(DomainPort::new(
            &local_clock,
            physical_port,
            TokioTimerHost::new(domain_number, system_tx.clone()),
            FakeTimestamping::new(),
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
        let announce_receipt_timeout = AnnounceReceiptTimeout::new(
            domain_port.timeout(
                SystemMessage::AnnounceReceiptTimeout,
                Duration::from_secs(10),
            ),
            Duration::from_secs(10),
        );
        let delay_timeout =
            domain_port.timeout(SystemMessage::DelayRequestTimeout, Duration::from_secs(0));
        let delay_cycle = DelayCycle::new(0.into(), delay_timeout, LogInterval::new(0));

        let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
        let log = TracingPortLog::new(port_identity);
        let port_state = PortState::Slave(SlavePort::new(
            domain_port,
            bmca,
            announce_receipt_timeout,
            delay_cycle,
            log,
            PortProfile::default(),
        ));
        let portmap = SingleDomainPortMap::new(domain_number, port_state);
        let rx_timestamping = FakeTimestamping::new();

        let portsloop = TokioPortsLoop::new(
            portmap,
            event_socket,
            general_socket,
            rx_timestamping,
            system_rx,
        )
        .await?;

        let mut delay_request_count = 0;

        let cond = time::timeout(StdDuration::from_secs(10), async {
            loop {
                time::advance(StdDuration::from_millis(100)).await;

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

    #[tokio::test]
    async fn ports_loop_handles_unknown_domain_event_message() -> std::io::Result<()> {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x03]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket_impl, event_queue) = InjectingNetworkSocket::new();
        let (general_socket_impl, _) = InjectingNetworkSocket::new();

        let node = MasterTestNode::new(
            &local_clock,
            Rc::new(event_socket_impl),
            Rc::new(general_socket_impl),
        )
        .await?;

        // Build a valid PTPv2 Sync message for a different domain (e.g., 7).
        let mut msg_buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(7),
            PortIdentity::fake(),
        );
        let sync_msg = TwoStepSyncMessage::new(1.into());
        let wire = sync_msg.serialize(&mut msg_buf);
        event_queue
            .lock()
            .unwrap()
            .push_back(wire.as_ref().to_vec());

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ports_loop_handles_unsupported_ptp_version_event_message() -> std::io::Result<()> {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x04]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket_impl, event_queue) = InjectingNetworkSocket::new();
        let (general_socket_impl, _) = InjectingNetworkSocket::new();

        let node = MasterTestNode::new(
            &local_clock,
            Rc::new(event_socket_impl),
            Rc::new(general_socket_impl),
        )
        .await?;

        // Build a valid PTPv2 Sync message for domain 0, then corrupt the version field.
        let mut msg_buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::fake(),
        );
        let sync_msg = TwoStepSyncMessage::new(2.into());
        let wire = sync_msg.serialize(&mut msg_buf);
        let mut bytes = wire.as_ref().to_vec();
        // Overwrite the PTP version field (offset 1) with an unsupported value.
        bytes[1] = 1;
        event_queue.lock().unwrap().push_back(bytes);

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ports_loop_handles_header_too_short_event_message() -> std::io::Result<()> {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x05]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket_impl, event_queue) = InjectingNetworkSocket::new();
        let (general_socket_impl, _) = InjectingNetworkSocket::new();

        let node = MasterTestNode::new(
            &local_clock,
            Rc::new(event_socket_impl),
            Rc::new(general_socket_impl),
        )
        .await?;

        // Inject a buffer that is shorter than the PTP header (34 bytes).
        event_queue.lock().unwrap().push_back(vec![0u8; 10]);

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ports_loop_handles_length_mismatch_event_message() -> std::io::Result<()> {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x06]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket_impl, event_queue) = InjectingNetworkSocket::new();
        let (general_socket_impl, _) = InjectingNetworkSocket::new();

        let node = MasterTestNode::new(
            &local_clock,
            Rc::new(event_socket_impl),
            Rc::new(general_socket_impl),
        )
        .await?;

        // Build a valid PTPv2 TwoStepSync message, then corrupt the length field.
        let mut msg_buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::fake(),
        );
        let sync_msg = TwoStepSyncMessage::new(10.into());
        let wire = sync_msg.serialize(&mut msg_buf);
        let mut bytes = wire.as_ref().to_vec();
        let len = bytes.len() as u16;
        let bad_len = len.wrapping_add(1);
        bytes[2] = (bad_len >> 8) as u8;
        bytes[3] = (bad_len & 0xFF) as u8;
        event_queue.lock().unwrap().push_back(bytes);

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ports_loop_handles_short_payload_event_message() -> std::io::Result<()> {
        let local_clock = LocalClock::new(
            FakeClock::default(),
            DefaultDS::new(
                ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x07]),
                Priority1::new(127),
                Priority2::new(127),
                ClockQuality::new(248, 0xFE, 0xFFFF),
            ),
            StepsRemoved::new(0),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket_impl, event_queue) = InjectingNetworkSocket::new();
        let (general_socket_impl, _) = InjectingNetworkSocket::new();

        let node = MasterTestNode::new(
            &local_clock,
            Rc::new(event_socket_impl),
            Rc::new(general_socket_impl),
        )
        .await?;

        // Build a valid PTPv2 OneStepSync message, then truncate the payload
        // so that the header length matches but the payload is too short.
        let mut msg_buf = MessageBuffer::new(
            TransportSpecific,
            PtpVersion::V2,
            DomainNumber::new(0),
            PortIdentity::fake(),
        );
        let sync_msg = OneStepSyncMessage::new(11.into(), TimeStamp::new(1, 2));
        let wire = sync_msg.serialize(&mut msg_buf);
        let mut bytes = wire.as_ref().to_vec();
        // Ensure length is at least header (34) + some payload, but less than 34 + 10.
        let truncated_len = 40usize;
        bytes.truncate(truncated_len);
        let len_field = truncated_len as u16;
        bytes[2] = (len_field >> 8) as u8;
        bytes[3] = (len_field & 0xFF) as u8;
        event_queue.lock().unwrap().push_back(bytes);

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }
}
