//! Two-port boundary clock: in-process & in-memory test over loopback sockets.
//!
//! Topology:
//!
//! ```text
//!                           +----------------+
//!                           | Boundary Clock |
//!                           +----------------+
//!                               |       |
//!             +-----------------+       +-------------------+
//!             |                                             |
//!   +-------------------+                        +---------------------+
//!   | Port 1 (upstream) |                        | Port 2 (downstream) |
//!   +-------------------+                        +---------------------+
//!   bc_up_ev | bc_up_gen                         bc_down_ev | bc_down_gen
//!             |                                             |
//!             |                                             |
//!       gm_ev | gm_gen                             slave_ev | slave_gen
//!        +--------+                                    +--------+
//!        | Port 1 |                                    | Port 1 |
//!        +--------+                                    +--------+
//!             |                                             |
//!        +--------+                                   +----------+
//!        |   GM   |                                   |  Slave   |
//!        +--------+                                   +----------+
//! ```

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use rptp::{
    clock::{Clock, ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, TimeScale},
    dataset::{ClockDS, Priority1, Priority2, StepsRemoved},
    infra::infra_support::ForeignClockRecordsVec,
    log::NOOP_CLOCK_METRICS,
    port::{DomainNumber, DomainPort, SingleDomainPortMap},
    profile::PortProfile,
    servo::ServoConfig,
    time::TimeStamp,
};
use rptp_daemon::boundary::BoundaryTokioClock;
use rptp_daemon::log::TracingPortLog;
use rptp_daemon::net::LoopbackSocket;
use rptp_daemon::node::{TokioPhysicalPort, TokioPortsLoop, TokioTimeout, TokioTimerHost};
use rptp_daemon::ordinary::OrdinaryTokioClock;
use rptp_daemon::timestamping::{ClockRxTimestamping, ClockTxTimestamping};
use rptp_daemon::virtualclock::{SharedVirtualClock, VirtualClock};
use tokio::sync::mpsc;

const DOMAIN: DomainNumber = DomainNumber::new(0);
const ADVANCE_STEP_MS: u64 = 100;
const TIMEOUT_VIRTUAL_MS: u64 = 30_000;

/// Inner port type for the loop (DomainPort).
type DomainPortInner<C> = DomainPort<
    C,
    TokioPhysicalPort<LoopbackSocket>,
    TokioTimerHost,
    ClockTxTimestamping<C>,
    TracingPortLog,
>;

/// Ports loop type for GM and Slave nodes.
type NodePortsLoop<'a, SDE> = TokioPortsLoop<
    SingleDomainPortMap<DomainPortInner<SharedVirtualClock>, ForeignClockRecordsVec, SDE>,
    LoopbackSocket,
    ClockRxTimestamping<SharedVirtualClock>,
>;

/// One ordinary-clock participant node (GM and Slave).
///
/// Owns clock, sockets, ordinary clock, and physical port; builds a `TokioPortsLoop` that borrows
/// from those pieces.
struct OrdinaryNode {
    clock: SharedVirtualClock,
    event_socket: Rc<LoopbackSocket>,
    general_socket: Rc<LoopbackSocket>,
    ordinary: OrdinaryTokioClock<SharedVirtualClock>,
    physical_port: TokioPhysicalPort<LoopbackSocket>,
}

impl OrdinaryNode {
    fn new(
        event_socket: Rc<LoopbackSocket>,
        general_socket: Rc<LoopbackSocket>,
        clock: SharedVirtualClock,
        default_ds: ClockDS,
    ) -> Self {
        let profile = PortProfile::default().with_servo_config(ServoConfig::Stepping {
            metrics: &NOOP_CLOCK_METRICS,
        });
        let ordinary = OrdinaryTokioClock::new(clock.clone(), default_ds, DOMAIN, profile);
        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
        Self {
            clock,
            event_socket,
            general_socket,
            ordinary,
            physical_port,
        }
    }

    fn clock(&self) -> SharedVirtualClock {
        self.clock.clone()
    }

    async fn build_loop<'a>(
        &'a mut self,
    ) -> std::io::Result<
        NodePortsLoop<'a, &'a rptp::ordinary::OrdinaryClock<SharedVirtualClock, TokioTimeout>>,
    > {
        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let timestamping = ClockTxTimestamping::new(self.clock.clone(), system_tx.clone(), DOMAIN);
        let port = self
            .ordinary
            .port(self.physical_port.clone(), system_tx, timestamping)
            .expect("ordinary clock has one port");
        let portmap = SingleDomainPortMap::new(DOMAIN, port);
        let rx_timestamping = ClockRxTimestamping::new(self.clock.clone());
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

/// Default dataset for both ports of the boundary clock (shared identity/priorities).
fn bc_default_ds() -> ClockDS {
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
async fn time_propagates_through_two_port_boundary_clock() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    // Loopback pairs: GM <-> BC upstream, and BC downstream <-> Slave.
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

    // GM: best clock, starts at 100s so the test can assert propagation.
    let gm_clock = SharedVirtualClock(Arc::new(VirtualClock::new(
        TimeStamp::new(100, 0),
        1.0,
        TimeScale::Ptp,
    )));
    let mut gm_node = OrdinaryNode::new(gm_ev.clone(), gm_gen.clone(), gm_clock, gm_default_ds());

    // Boundary clock: real two-port BoundaryTokioClock with a VirtualClock.
    let shared_bc = SharedVirtualClock(Arc::new(VirtualClock::new(
        TimeStamp::new(0, 0),
        1.0,
        TimeScale::Ptp,
    )));
    let boundary_clock = BoundaryTokioClock::new(shared_bc.clone(), bc_default_ds(), DOMAIN, 2);

    let bc_up_physical = TokioPhysicalPort::new(bc_up_ev.clone(), bc_up_gen.clone());
    let bc_down_physical = TokioPhysicalPort::new(bc_down_ev.clone(), bc_down_gen.clone());

    let (bc_tx_1, bc_rx_1) = mpsc::unbounded_channel();
    let (bc_tx_2, bc_rx_2) = mpsc::unbounded_channel();

    let bc_port_1 = boundary_clock.port(
        bc_up_physical,
        bc_tx_1.clone(),
        ClockTxTimestamping::new(shared_bc.clone(), bc_tx_1, DOMAIN),
    );
    let bc_port_2 = boundary_clock.port(
        bc_down_physical,
        bc_tx_2.clone(),
        ClockTxTimestamping::new(shared_bc.clone(), bc_tx_2, DOMAIN),
    );

    let bc_port_1 = bc_port_1.expect("BC port 1");
    let bc_port_2 = bc_port_2.expect("BC port 2");

    let bc_up_loop = TokioPortsLoop::new(
        SingleDomainPortMap::new(DOMAIN, bc_port_1),
        bc_up_ev.clone(),
        bc_up_gen.clone(),
        ClockRxTimestamping::new(shared_bc.clone()),
        bc_rx_1,
    )
    .await?;

    let bc_down_loop = TokioPortsLoop::new(
        SingleDomainPortMap::new(DOMAIN, bc_port_2),
        bc_down_ev,
        bc_down_gen,
        ClockRxTimestamping::new(shared_bc.clone()),
        bc_rx_2,
    )
    .await?;

    // Slave: syncs to the boundary clock downstream port.
    let slave_clock = SharedVirtualClock(Arc::new(VirtualClock::new(
        TimeStamp::new(0, 0),
        1.0,
        TimeScale::Ptp,
    )));
    let mut slave_node = OrdinaryNode::new(slave_ev, slave_gen, slave_clock, slave_default_ds());

    let slave_clock_done = slave_node.clock();
    let bc_clock_done = shared_bc.clone();

    let gm_loop = gm_node.build_loop().await?;
    let slave_loop = slave_node.build_loop().await?;

    // Run all four loops on the current thread. Slave exits when its clock reaches the target.
    let target = TimeStamp::new(99, 0);
    let slave_finished = async move {
        let steps = (TIMEOUT_VIRTUAL_MS / ADVANCE_STEP_MS) as usize;
        for _ in 0..steps {
            tokio::time::advance(Duration::from_millis(ADVANCE_STEP_MS)).await;
            if slave_clock_done.now() >= target {
                return;
            }
        }
        panic!("slave did not reach target within 30s virtual (no Sync from BC?)");
    };

    let run_all = async move {
        tokio::select! {
            _ = gm_loop.run() => Err(std::io::Error::other("gm loop exited unexpectedly")),
            _ = bc_up_loop.run() => Err(std::io::Error::other("BC upstream loop exited unexpectedly")),
            _ = bc_down_loop.run() => Err(std::io::Error::other("BC downstream loop exited unexpectedly")),
            r = slave_loop.run_until(slave_finished) => r,
        }
    };

    let result = run_all.await;
    if result.is_ok() {
        assert!(
            bc_clock_done.now() >= target,
            "boundary clock did not advance to target time"
        );
    }
    result
}
