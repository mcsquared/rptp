pub mod log;
pub mod net;
pub mod node;
pub mod ordinary;
pub mod timestamping;
pub mod virtualclock;

use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::bmca::{DefaultDS, Priority1, Priority2};
use rptp::clock::{ClockIdentity, ClockQuality, LocalClock, StepsRemoved};
use rptp::log::NOOP_CLOCK_METRICS;
use rptp::port::{DomainNumber, PortNumber, SingleDomainPortMap};
use rptp::time::TimeStamp;

use crate::net::MulticastSocket;
use crate::node::TokioPortsLoop;
use crate::ordinary::ordinary_clock_port;
use crate::timestamping::ClockTimestamping;
use crate::virtualclock::VirtualClock;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let domain = DomainNumber::new(0);

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
        &NOOP_CLOCK_METRICS,
    );

    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);

    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let timestamping = ClockTimestamping::new(&virtual_clock, system_tx.clone(), domain);
    let port = ordinary_clock_port(
        &local_clock,
        domain,
        event_socket.clone(),
        general_socket.clone(),
        system_tx.clone(),
        PortNumber::new(1),
        &timestamping,
    );
    let portmap = SingleDomainPortMap::new(domain, port);

    let ports_loop = TokioPortsLoop::new(
        portmap,
        event_socket,
        general_socket,
        &timestamping,
        system_rx,
    )
    .await?;

    ports_loop.run().await?;

    Ok(())
}
