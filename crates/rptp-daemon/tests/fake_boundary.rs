//! Fake boundary clock: in-process & in-memory test over loopback sockets.
//!
//! Topology:
//!
//! ```text
//!   [GM]  =====  [BC upstream]  =====  (shared clock)  =====  [BC downstream]  =====  [Slave]
//!  master            slave              VirtualClock              master               slave
//! ```
//!
//! The “boundary clock” in the middle is currently a **poor man’s BC**: two ordinary clocks that
//! share a single [`VirtualClock`]. The upstream one slaves to the GM and disciplines that clock;
//! the downstream one masters toward the slave using the same clock. This setup is meant to be
//! evolved into a real boundary scenario by replacing those two ordinary clocks with a proper
//! boundary clock in the middle that exposes two ports (one for the upstream and one for the
//! downstream).
//!
//! So for now, this test validates that time can propagate GM → fake BC → slave over loopback
//! and that the test harness (nodes, loopback pairs, shared clock) is sufficient to drive that
//! evolution.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use rptp::{
    bmca::{ClockDS, Priority1, Priority2},
    clock::{
        Clock, ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, LocalClock, StepsRemoved,
        TimeScale,
    },
    infra::infra_support::ForeignClockRecordsVec,
    log::NOOP_CLOCK_METRICS,
    port::{DomainNumber, DomainPort, PortNumber, SingleDomainPortMap},
    servo::{Servo, SteppingServo},
    time::TimeStamp,
};
use rptp_daemon::log::TracingPortLog;
use rptp_daemon::net::LoopbackSocket;
use rptp_daemon::node::{TokioPhysicalPort, TokioPortsLoop, TokioTimerHost};
use rptp_daemon::ordinary::OrdinaryTokioClock;
use rptp_daemon::timestamping::{ClockRxTimestamping, ClockTxTimestamping};
use rptp_daemon::virtualclock::{SharedVirtualClock, VirtualClock};
use tokio::sync::mpsc;

/// Inner port type for the loop (DomainPort). Used in [`NodePortsLoop`].
type DomainPortInner<'a, C> =
    DomainPort<'a, C, TokioTimerHost, ClockTxTimestamping<C>, TracingPortLog>;

/// Ports loop type returned by [`Node::build_loop`].
type NodePortsLoop<'a> = TokioPortsLoop<
    SingleDomainPortMap<'a, DomainPortInner<'a, SharedVirtualClock>, ForeignClockRecordsVec>,
    LoopbackSocket,
    ClockRxTimestamping<SharedVirtualClock>,
>;

/// One participant node in this test: owns clock, sockets, ordinary clock, and physical port.
/// Loopback sockets are passed in at construction; the node owns everything else needed to run
/// a single PTP participant (GM, slave, or one “port” of the fake boundary clock).
struct Node {
    shared_clock: SharedVirtualClock,
    event_socket: Rc<LoopbackSocket>,
    general_socket: Rc<LoopbackSocket>,
    ordinary: OrdinaryTokioClock<SharedVirtualClock>,
    physical_port: TokioPhysicalPort<LoopbackSocket>,
}

impl Node {
    fn new(
        event_socket: Rc<LoopbackSocket>,
        general_socket: Rc<LoopbackSocket>,
        clock: Arc<VirtualClock>,
        default_ds: ClockDS,
    ) -> Self {
        const DOMAIN: DomainNumber = DomainNumber::new(0); // test assumes domain 0 only
        const PORT_NUMBER: PortNumber = PortNumber::new(1); // test assumes port 1 only
        let shared_clock = SharedVirtualClock(clock);
        let local_clock = LocalClock::new(
            shared_clock.clone(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );
        let ordinary = OrdinaryTokioClock::new(local_clock, default_ds, DOMAIN, PORT_NUMBER);
        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
        Self {
            shared_clock,
            event_socket,
            general_socket,
            ordinary,
            physical_port,
        }
    }

    fn clock(&self) -> Arc<VirtualClock> {
        Arc::clone(&self.shared_clock.0)
    }

    /// Build a ports loop that borrows from this node. The returned loop must not outlive the node.
    async fn build_loop<'a>(&'a mut self) -> std::io::Result<NodePortsLoop<'a>> {
        let domain = DomainNumber::new(0);
        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let timestamping =
            ClockTxTimestamping::new(self.shared_clock.clone(), system_tx.clone(), domain);
        let port = self
            .ordinary
            .port(&self.physical_port, system_tx, timestamping);
        let portmap = SingleDomainPortMap::new(domain, port);
        let rx_timestamping = ClockRxTimestamping::new(self.shared_clock.clone());
        TokioPortsLoop::new(
            portmap,
            self.event_socket.clone(),
            self.general_socket.clone(),
            rx_timestamping,
            system_rx,
        )
        .await
    }
}

fn gm_default_ds() -> ClockDS {
    ClockDS::new(
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
        Priority1::new(64),
        Priority2::new(127),
        ClockQuality::new(ClockClass::Default, ClockAccuracy::Within100ns, 0xFFFF),
        StepsRemoved::new(0),
    )
}

/// Default dataset for both ports of the fake boundary clock (shared identity/priorities).
fn fake_bc_default_ds() -> ClockDS {
    ClockDS::new(
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x42]),
        Priority1::new(128),
        Priority2::new(127),
        ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1us, 0xFFFF),
        StepsRemoved::new(0),
    )
}

fn slave_default_ds() -> ClockDS {
    ClockDS::new(
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
        Priority1::new(200),
        Priority2::new(255),
        ClockQuality::new(ClockClass::Default, ClockAccuracy::Within10ms, 0xFFFF),
        StepsRemoved::new(0),
    )
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn fake_boundary_clock() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    // Loopback pairs: GM <-> fake BC upstream (event + general); fake BC downstream <-> Slave.
    let (gm_ev, bc_up_ev) = LoopbackSocket::pair();
    let (gm_gen, bc_up_gen) = LoopbackSocket::pair();
    let (bc_down_ev, slave_ev) = LoopbackSocket::pair();
    let (bc_down_gen, slave_gen) = LoopbackSocket::pair();

    let gm_ev = Rc::new(gm_ev);
    let gm_gen = Rc::new(gm_gen);
    let bc_up_ev = Rc::new(bc_up_ev);
    let bc_up_gen = Rc::new(bc_up_gen);
    let bc_down_ev = Rc::new(bc_down_ev);
    let bc_down_gen = Rc::new(bc_down_gen);
    let slave_ev = Rc::new(slave_ev);
    let slave_gen = Rc::new(slave_gen);

    // GM: best clock, starts at 100s so we can assert propagation (slave starts at 0; it should
    // step to ~100 only if it receives Sync from the fake BC downstream).
    let gm_clock = Arc::new(VirtualClock::new(
        TimeStamp::new(100, 0),
        1.0,
        TimeScale::Ptp,
    ));
    let mut gm_node = Node::new(gm_ev, gm_gen, gm_clock, gm_default_ds());

    // Fake BC: one shared clock, two ordinary-clock “ports”. Upstream slaves to GM and disciplines
    // the clock; downstream masters toward the slave. To be replaced by a real boundary clock.
    let bc_clock = Arc::new(VirtualClock::new(TimeStamp::new(0, 0), 1.0, TimeScale::Ptp));
    let mut bc_up_node = Node::new(bc_up_ev, bc_up_gen, bc_clock.clone(), fake_bc_default_ds());
    let mut bc_down_node = Node::new(bc_down_ev, bc_down_gen, bc_clock, fake_bc_default_ds());

    // Slave: syncs to fake BC downstream. Test passes when its clock advances to the target.
    let slave_clock = Arc::new(VirtualClock::new(TimeStamp::new(0, 0), 1.0, TimeScale::Ptp));
    let mut slave_node = Node::new(slave_ev, slave_gen, slave_clock.clone(), slave_default_ds());

    let slave_clock_done = slave_node.clock();

    let gm_loop = gm_node.build_loop().await?;
    let bc_up_loop = bc_up_node.build_loop().await?;
    let bc_down_loop = bc_down_node.build_loop().await?;
    let slave_loop = slave_node.build_loop().await?;

    // Run all four loops on the current thread (Rc/refs are !Send). Time is manually advanced so
    // the test finishes quickly: PTP timeouts (e.g. 5s announce) fire after that much *virtual*
    // time. Slave exits when its clock reaches the target (only possible via Sync from fake BC).
    let target = TimeStamp::new(99, 0);
    const ADVANCE_STEP_MS: u64 = 100;
    const TIMEOUT_VIRTUAL_MS: u64 = 30_000;
    let slave_finished = async move {
        let steps = (TIMEOUT_VIRTUAL_MS / ADVANCE_STEP_MS) as usize;
        for _ in 0..steps {
            tokio::time::advance(Duration::from_millis(ADVANCE_STEP_MS)).await;
            if slave_clock_done.as_ref().now() >= target {
                return;
            }
        }
        panic!("slave did not reach target within 30s virtual (no Sync from fake BC?)");
    };

    let run_all = async move {
        tokio::select! {
            _ = gm_loop.run() => Err(std::io::Error::other("gm loop exited unexpectedly")),
            _ = bc_up_loop.run() => Err(std::io::Error::other("fake BC upstream loop exited unexpectedly")),
            _ = bc_down_loop.run() => Err(std::io::Error::other("fake BC downstream loop exited unexpectedly")),
            r = slave_loop.run_until(slave_finished) => r,
        }
    };

    run_all.await
}
