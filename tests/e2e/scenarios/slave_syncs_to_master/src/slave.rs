use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use rptp::bmca::{FullBmca, LocalClockDS};
use rptp::clock::{ClockIdentity, ClockQuality, FakeClock, LocalClock};
use rptp::infra::infra_support::SortedForeignClockRecordsVec;
use rptp::port::{DomainPort, SingleDomainPortMap};
use rptp::portstate::PortState;
use rptp::time::TimeStamp;
use rptp_daemon::net::MulticastSocket;
use rptp_daemon::node::{TokioNetwork, TokioPhysicalPort, TokioTimerHost};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let domain = 0;

    let local_clock = LocalClock::new(
        Rc::new(FakeClock::new(TimeStamp::new(0, 0))),
        LocalClockDS::new(
            ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            ClockQuality::new(250, 0xFE, 0xFFFF),
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
    let timer_host = TokioTimerHost::new(domain, system_tx);

    let port_state = PortState::initializing(DomainPort::new(
        &local_clock,
        bmca,
        physical_port,
        timer_host,
        domain,
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

    println!("Slave ready");

    let clock_sync = async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if local_clock.now() == TimeStamp::new(10, 500_000_000) {
                break;
            }
        }
    };

    let result = timeout(Duration::from_secs(30), tokio_network.run_until(clock_sync))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Slave node timed out before clock sync",
            )
        })?;

    let socket = UdpSocket::bind("0.0.0.0:12345").await?;
    socket.set_broadcast(true)?;
    socket.send_to(b"accept", "255.255.255.255:12345").await?;

    result
}
