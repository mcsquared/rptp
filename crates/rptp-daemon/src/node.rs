//! Tokio runtime glue for driving `rptp` ports.
//!
//! This module provides the daemon-side implementations of the `rptp` infrastructure boundaries:
//! - [`TokioTimerHost`] / [`TokioTimeout`]: schedule [`SystemMessage`]s using Tokio tasks,
//! - [`TokioPhysicalPort`]: send event/general datagrams over UDP sockets, and
//! - [`TokioPortsLoop`]: the main IO loop that receives UDP datagrams, parses them through
//!   [`MessageIngress`], and forwards timer/timestamp feedback into the port state machine.
//!
//! The goal of this layer is to keep the async/runtime concerns out of the domain core (`crates/rptp`)
//! while still making it easy to assemble runnable nodes.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant as StdInstant;

use tokio::sync::mpsc;

use rptp::{
    message::{MessageIngress, SystemMessage},
    port::{DomainNumber, PhysicalPort, PortMap, SendError, SendResult, Timeout, TimerHost},
    result::{Error as RptpError, ProtocolError},
    time::{Duration, Instant},
};

use crate::net::NetworkSocket;
use crate::timestamping::RxTimestamping;

/// Tokio-backed implementation of the `rptp` [`Timeout`] boundary.
///
/// Each timeout owns:
/// - the [`SystemMessage`] it should emit when fired, and
/// - a join handle for the currently scheduled Tokio sleep task.
///
/// Calling [`Timeout::restart`] cancels any previously scheduled task (if present) and schedules a
/// new one.
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
    ) -> Self {
        let inner = Arc::new(TokioTimeoutInner {
            domain_number,
            tx,
            msg: Mutex::new(msg),
            handle: Mutex::new(None),
        });

        Self { inner }
    }

    /// Cancel the currently scheduled task (if any) and schedule a new one for `delay`.
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

    /// Cancel the currently scheduled task (if any).
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

/// Tokio-backed implementation of the `rptp` [`TimerHost`] boundary.
///
/// The timer host provides per-port `Timeout` handles. When a timeout fires it sends a
/// `(DomainNumber, SystemMessage)` into an unbounded channel consumed by [`TokioPortsLoop`].
pub struct TokioTimerHost {
    domain_number: DomainNumber,
    tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
}

impl TokioTimerHost {
    /// Create a new timer host for a specific domain.
    pub fn new(
        domain_number: DomainNumber,
        tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
    ) -> Self {
        Self { domain_number, tx }
    }
}

impl TimerHost for TokioTimerHost {
    type Timeout = TokioTimeout;

    fn timeout(&self, msg: SystemMessage) -> Self::Timeout {
        TokioTimeout::new(self.domain_number, self.tx.clone(), msg)
    }
}

/// `rptp` [`PhysicalPort`] implementation backed by two UDP sockets.
///
/// IEEE 1588 uses two UDP ports:
/// - 319 for **event** messages, and
/// - 320 for **general** messages.
///
/// `TokioPhysicalPort` expects the sockets to already be configured and uses `try_send` to avoid
/// awaiting in the domain send path. IO errors are mapped to [`SendError`] and logged.
pub struct TokioPhysicalPort<N: NetworkSocket> {
    event_socket: Rc<N>,
    general_socket: Rc<N>,
}

impl<N: NetworkSocket> TokioPhysicalPort<N> {
    /// Create a new physical port over the provided event and general sockets.
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
                Err(SendError)
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
                Err(SendError)
            }
        }
    }
}

enum RxKind {
    Event,
    General,
}

/// Main Tokio loop that drives one or more domain ports.
///
/// The loop multiplexes:
/// - event socket receives,
/// - general socket receives, and
/// - internally generated [`SystemMessage`]s (timeouts, egress timestamp feedback, etc.).
///
/// It parses incoming datagrams through [`MessageIngress`] which dispatches into the configured
/// [`PortMap`].
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
    /// Create a new ports loop over the provided port map and sockets.
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

    /// Run the loop until externally terminated (signals or fatal IO error).
    pub async fn run(self) -> std::io::Result<()> {
        self.run_until(std::future::pending::<()>()).await
    }

    /// Run the loop until `shutdown` completes.
    ///
    /// This is primarily used by tests to stop the loop after some condition has been observed.
    pub async fn run_until<F>(mut self, shutdown: F) -> std::io::Result<()>
    where
        F: std::future::Future<Output = ()>,
    {
        let start = StdInstant::now();

        let mut event_buf = [0u8; 2048];
        let mut general_buf = [0u8; 2048];

        tokio::pin!(shutdown);

        // Kick the state machine(s) into LISTENING once infrastructure is ready.
        //
        // Note: this is currently hard-coded to domain 0 and will likely evolve into a more
        // general initialization mechanism as multi-domain support matures.
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

    /// Process a single received datagram by parsing and dispatching it into the port map.
    ///
    /// - For event messages, ingress timestamps are provided by `timestamping.ingress_stamp()`.
    /// - For general messages, `now` is a monotonic [`Instant`] used for time-window logic in the
    ///   domain.
    fn process_datagram(&mut self, kind: RxKind, buf: &[u8], size: usize, now: Instant) {
        let label = match kind {
            RxKind::Event => "event",
            RxKind::General => "general",
        };

        let res = match kind {
            RxKind::Event => MessageIngress::new(&mut self.portmap)
                .receive_event(&buf[..size], self.timestamping.ingress_stamp()),
            RxKind::General => {
                MessageIngress::new(&mut self.portmap).receive_general(&buf[..size], now)
            }
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

    use rptp::{
        bmca::{BestForeignSnapshot, ClockDS, ForeignGrandMasterCandidates, Priority1, Priority2},
        clock::{
            ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, LocalClock, StepsRemoved,
            SynchronizableClock, TimeScale,
        },
        infra::infra_support::ForeignClockRecordsVec,
        log::NOOP_CLOCK_METRICS,
        message::{EventMessage, GeneralMessage},
        port::{DomainPort, ParentPortIdentity, PortIdentity, PortNumber, SingleDomainPortMap},
        portstate::PortState,
        profile::PortProfile,
        servo::{Servo, SteppingServo},
        test_support::{
            FakeClock, FakeTimestamping, TestMessage, master_test_port, slave_test_port,
        },
        time::{LogInterval, TimeStamp},
    };

    use crate::log::TracingPortLog;
    use crate::net::{FakeNetworkSocket, NetworkSocket};
    use crate::timestamping::ClockTxTimestamping;
    use crate::virtualclock::VirtualClock;

    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::time::Duration as StdDuration;

    type TestPortMap<'a, C> = SingleDomainPortMap<
        'a,
        Box<DomainPort<'a, C, TokioTimerHost, FakeTimestamping, TracingPortLog>>,
        ForeignClockRecordsVec,
    >;

    type TestPortsLoop<'a, C, N> = TokioPortsLoop<TestPortMap<'a, C>, N, FakeTimestamping>;

    // Prebuilt PTPv2 Sync frames used by error-handling tests.
    const DOMAIN_SEVEN_SYNC: [u8; 44] = [
        0x00, 0x02, 0x00, 0x2C, 0x07, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x01,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // PTPv1 sync
    const PTP_V1_SYNC: [u8; 44] = [
        0x00, 0x01, 0x00, 0x2C, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x01,
        0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // Declared length (0x002D) does not match actual byte length (44).
    const LENGTH_MISMATCH_SYNC: [u8; 44] = [
        0x00, 0x02, 0x00, 0x2D, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x01,
        0x00, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // Length field matches (0x0028) but payload is only 6 bytes (origin timestamp requires 10).
    const SHORT_PAYLOAD_SYNC: [u8; 40] = [
        0x00, 0x02, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x01,
        0x00, 0x0B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xAA, 0xBB,
    ];

    struct MasterTestNode<'a, C: SynchronizableClock, N: NetworkSocket> {
        portsloop: TestPortsLoop<'a, C, N>,
    }

    impl<'a, C: SynchronizableClock, N: NetworkSocket> MasterTestNode<'a, C, N> {
        async fn new(
            local_clock: &'a LocalClock<C>,
            default_ds: &'a ClockDS,
            foreign_candidates: &'a dyn ForeignGrandMasterCandidates,
            event_socket: Rc<N>,
            general_socket: Rc<N>,
            physical_port: &'a TokioPhysicalPort<N>,
        ) -> std::io::Result<Self> {
            let domain_number = DomainNumber::new(0);

            let (system_tx, system_rx) = mpsc::unbounded_channel();
            let tx_timestamping = FakeTimestamping::new();
            let port_number = PortNumber::new(1);
            let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
            let domain_port = Box::new(DomainPort::new(
                local_clock,
                physical_port,
                TokioTimerHost::new(domain_number, system_tx.clone()),
                tx_timestamping,
                TracingPortLog::new(port_identity),
                domain_number,
                port_number,
            ));
            let port_state = master_test_port(
                domain_port,
                default_ds,
                ForeignClockRecordsVec::new(),
                foreign_candidates,
                PortProfile::default(),
            );
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
                        TokioTimerHost,
                        ClockTxTimestamping<FakeClock>,
                        TracingPortLog,
                    >,
                >,
                ForeignClockRecordsVec,
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

        let virtual_clock = VirtualClock::new(TimeStamp::new(0, 0), 1.0, TimeScale::Arb);
        let default_ds = ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
            StepsRemoved::new(0),
        );
        let local_clock = LocalClock::new(
            &virtual_clock,
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let timestamping =
            ClockTxTimestamping::new(&virtual_clock, system_tx.clone(), domain_number);
        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
        let port_number = PortNumber::new(1);
        let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
        let domain_port = Box::new(DomainPort::new(
            &local_clock,
            &physical_port,
            TokioTimerHost::new(domain_number, system_tx.clone()),
            &timestamping,
            TracingPortLog::new(port_identity),
            domain_number,
            port_number,
        ));
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let port_state = master_test_port(
            domain_port,
            &default_ds,
            ForeignClockRecordsVec::new(),
            &foreign_candidates,
            PortProfile::default(),
        );
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
                        TestMessage::new(msg.as_ref()).event(),
                        Ok(EventMessage::TwoStepSync(_))
                    ) {
                        sync_count += 1;
                    }
                }
                while let Ok(msg) = general_socket_rx.try_recv() {
                    if matches!(
                        TestMessage::new(msg.as_ref()).general(),
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

        let default_ds = ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
            StepsRemoved::new(0),
        );
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
        let port_number = PortNumber::new(1);
        let timer_host = TokioTimerHost::new(domain_number, system_tx.clone());
        let port_identity = PortIdentity::new(*local_clock.identity(), port_number);
        let domain_port = Box::new(DomainPort::new(
            &local_clock,
            &physical_port,
            timer_host,
            FakeTimestamping::new(),
            TracingPortLog::new(port_identity),
            domain_number,
            port_number,
        ));
        let parent_port_identity = ParentPortIdentity::new(PortIdentity::new(
            ClockIdentity::new(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
            PortNumber::new(1),
        ));
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let port_state = slave_test_port(
            domain_port,
            &default_ds,
            ForeignClockRecordsVec::new(),
            &foreign_candidates,
            parent_port_identity,
            PortProfile::new(
                Duration::from_secs(60),
                LogInterval::new(0),
                LogInterval::new(0),
                LogInterval::new(0),
            ),
        );
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
                        TestMessage::new(msg.as_ref()).event(),
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
        let default_ds = ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x03]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
            StepsRemoved::new(0),
        );
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket, event_queue) = InjectingNetworkSocket::new();
        let (general_socket, _) = InjectingNetworkSocket::new();

        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());

        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let node = MasterTestNode::new(
            &local_clock,
            &default_ds,
            &foreign_candidates,
            event_socket,
            general_socket,
            &physical_port,
        )
        .await?;

        event_queue
            .lock()
            .unwrap()
            .push_back(DOMAIN_SEVEN_SYNC.to_vec());

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ports_loop_handles_unsupported_ptp_version_event_message() -> std::io::Result<()> {
        let default_ds = ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x04]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
            StepsRemoved::new(0),
        );
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket, event_queue) = InjectingNetworkSocket::new();
        let (general_socket, _) = InjectingNetworkSocket::new();

        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());

        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let node = MasterTestNode::new(
            &local_clock,
            &default_ds,
            &foreign_candidates,
            event_socket,
            general_socket,
            &physical_port,
        )
        .await?;

        event_queue.lock().unwrap().push_back(PTP_V1_SYNC.to_vec());

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ports_loop_handles_header_too_short_event_message() -> std::io::Result<()> {
        let default_ds = ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x05]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
            StepsRemoved::new(0),
        );
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket, event_queue) = InjectingNetworkSocket::new();
        let (general_socket, _) = InjectingNetworkSocket::new();

        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());

        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let node = MasterTestNode::new(
            &local_clock,
            &default_ds,
            &foreign_candidates,
            event_socket,
            general_socket,
            &physical_port,
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
        let default_ds = ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x06]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
            StepsRemoved::new(0),
        );
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket, event_queue) = InjectingNetworkSocket::new();
        let (general_socket, _) = InjectingNetworkSocket::new();

        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());

        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let node = MasterTestNode::new(
            &local_clock,
            &default_ds,
            &foreign_candidates,
            event_socket,
            general_socket,
            &physical_port,
        )
        .await?;

        event_queue
            .lock()
            .unwrap()
            .push_back(LENGTH_MISMATCH_SYNC.to_vec());

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }

    #[tokio::test]
    async fn ports_loop_handles_short_payload_event_message() -> std::io::Result<()> {
        let default_ds = ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x07]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
            StepsRemoved::new(0),
        );
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let (event_socket, event_queue) = InjectingNetworkSocket::new();
        let (general_socket, _) = InjectingNetworkSocket::new();

        let event_socket = Rc::new(event_socket);
        let general_socket = Rc::new(general_socket);

        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());

        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let node = MasterTestNode::new(
            &local_clock,
            &default_ds,
            &foreign_candidates,
            event_socket,
            general_socket,
            &physical_port,
        )
        .await?;

        event_queue
            .lock()
            .unwrap()
            .push_back(SHORT_PAYLOAD_SYNC.to_vec());

        let shutdown = time::sleep(StdDuration::from_millis(20));
        node.run_until(shutdown).await?;

        Ok(())
    }
}
