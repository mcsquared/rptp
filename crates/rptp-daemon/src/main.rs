pub mod net;
pub mod node;

use std::rc::Rc;

use tokio::sync::mpsc;

use rptp::bmca::{FullBmca, LocalClockDS};
use rptp::clock::{ClockIdentity, ClockQuality, FakeClock, LocalClock};
use rptp::infra::infra_support::SortedForeignClockRecordsVec;
use rptp::port::{DomainPort, PortNumber, SingleDomainPortMap};
use rptp::portstate::PortState;

use crate::net::MulticastSocket;
use crate::node::{TokioNetwork, TokioPhysicalPort, TokioTimerHost};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let domain = 0;

    let local_clock = LocalClock::new(
        Rc::new(FakeClock::default()),
        LocalClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            ClockQuality::new(248, 0xFE, 0xFFFF),
        ),
    );

    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);

    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let physical_port = TokioPhysicalPort::new(
        &local_clock,
        domain,
        event_socket.clone(),
        general_socket.clone(),
        system_tx.clone(),
    );
    let bmca = FullBmca::new(SortedForeignClockRecordsVec::new());
    let timer_host = TokioTimerHost::new(domain, system_tx.clone());

    let port_state = PortState::initializing(DomainPort::new(
        &local_clock,
        bmca,
        physical_port,
        timer_host,
        domain,
        PortNumber::new(1),
    ));

    let portmap = SingleDomainPortMap::new(domain, port_state);

    let tokio_network = TokioNetwork::new(
        &local_clock,
        portmap,
        event_socket,
        general_socket,
        system_rx,
    )
    .await?;

    tokio_network.run().await?;

    Ok(())
}
