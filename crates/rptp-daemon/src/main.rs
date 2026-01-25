//! `rptp-daemon` binary entrypoint.
//!
//! This is a small Tokio-based executable that wires together the `rptp` domain core with a UDP
//! transport and a clock implementation.
//!
//! Current characteristics (intentionally minimal / experimental):
//! - Uses a [`VirtualClock`](crate::virtualclock::VirtualClock) as the local time source.
//! - Uses an [`OrdinaryTokioClock`](crate::ordinary::OrdinaryTokioClock) with a single domain/port
//!   (`domain=0`, `port=1`).
//! - Joins the standard PTP multicast groups via [`MulticastSocket`](crate::net::MulticastSocket)
//!   and drives IO and timers through [`TokioPortsLoop`](crate::node::TokioPortsLoop).
//! - Installs tracing configured via `RUST_LOG` (default `info`) through
//!   [`rptp_daemon::init_tracing`].
//!
//! This wiring is expected to evolve into a configurable CLI; the hard-coded dataset and virtual
//! clock are placeholders for early development and integration testing.

pub mod log;
pub mod net;
pub mod node;
pub mod ordinary;
pub mod timestamping;
pub mod virtualclock;

use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::{
    bmca::{ClockDS, Priority1, Priority2},
    clock::{
        ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, LocalClock, StepsRemoved, TimeScale,
    },
    log::NOOP_CLOCK_METRICS,
    port::{DomainNumber, PortNumber, SingleDomainPortMap},
    servo::{Servo, SteppingServo},
    time::TimeStamp,
};

use crate::net::MulticastSocket;
use crate::node::{TokioPhysicalPort, TokioPortsLoop};
use crate::ordinary::OrdinaryTokioClock;
use crate::timestamping::{ClockRxTimestamping, ClockTxTimestamping};
use crate::virtualclock::VirtualClock;

#[tokio::main(flavor = "current_thread")]
/// Run the daemon IO loop until an error occurs.
///
/// The binary currently:
/// - configures tracing,
/// - creates sockets and system message channels,
/// - builds an ordinary clock and domain port,
/// - installs timestamping hooks for ingress/egress, and
/// - runs the `TokioPortsLoop`.
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let virtual_clock = VirtualClock::new(TimeStamp::new(0, 0), 1.0, TimeScale::Ptp);
    let default_ds = ClockDS::new(
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
        Priority1::new(127),
        Priority2::new(127),
        ClockQuality::new(ClockClass::Default, ClockAccuracy::Within1ms, 0xFFFF),
        StepsRemoved::new(0),
    );

    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);
    let (system_tx, system_rx) = mpsc::unbounded_channel();

    let mut ordinary_clock = OrdinaryTokioClock::new(
        LocalClock::new(
            &virtual_clock,
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        ),
        default_ds,
        DomainNumber::new(0),
        PortNumber::new(1),
    );

    let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
    let domain_number = ordinary_clock.domain_number();

    let port = ordinary_clock.port(
        &physical_port,
        system_tx.clone(),
        ClockTxTimestamping::new(&virtual_clock, system_tx.clone(), domain_number),
    );

    let portmap = SingleDomainPortMap::new(domain_number, port);

    let ports_loop = TokioPortsLoop::new(
        portmap,
        event_socket,
        general_socket,
        ClockRxTimestamping::new(&virtual_clock),
        system_rx,
    )
    .await?;

    ports_loop.run().await?;

    Ok(())
}
