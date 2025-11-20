pub mod log;
pub mod net;
pub mod node;
pub mod ordinary;

use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::bmca::{LocalClockDS, Priority1, Priority2};
use rptp::clock::{ClockIdentity, ClockQuality, FakeClock, LocalClock};
use rptp::port::{DomainNumber, PortNumber, SingleDomainPortMap};

use crate::net::MulticastSocket;
use crate::node::TokioPortsLoop;
use crate::ordinary::ordinary_clock_port;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let domain = DomainNumber::new(0);

    let local_clock = LocalClock::new(
        FakeClock::default(),
        LocalClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            Priority1::new(127),
            Priority2::new(127),
            ClockQuality::new(248, 0xFE, 0xFFFF),
        ),
    );

    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);

    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let port = ordinary_clock_port(
        &local_clock,
        domain,
        event_socket.clone(),
        general_socket.clone(),
        system_tx.clone(),
        PortNumber::new(1),
    );
    let portmap = SingleDomainPortMap::new(domain, port);

    let ports_loop = TokioPortsLoop::new(
        &local_clock,
        portmap,
        event_socket,
        general_socket,
        system_rx,
    )
    .await?;

    ports_loop.run().await?;

    Ok(())
}
