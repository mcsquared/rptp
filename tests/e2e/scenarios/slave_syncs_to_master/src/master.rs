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
        Rc::new(FakeClock::new(TimeStamp::new(10, 500_000_000))),
        LocalClockDS::new(
            ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            ClockQuality::new(240, 0xFE, 0xFFFF),
        ),
    );
    let event_socket = MulticastSocket::event().await?;
    let general_socket = MulticastSocket::general().await?;

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (general_tx, general_rx) = mpsc::unbounded_channel();
    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let physical_port = TokioPhysicalPort::new(domain, event_tx, general_tx);
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
        event_socket,
        general_socket,
        portmap,
        event_rx,
        general_rx,
        system_rx,
    )
    .await?;

    println!("Master ready");

    let slave_accepted = async {
        let socket = UdpSocket::bind("0.0.0.0:12345").await.unwrap();

        let mut buf = [0; 1024];
        loop {
            let size = socket.recv(&mut buf).await.unwrap();
            if let Ok(message) = std::str::from_utf8(&buf[..size]) {
                if message == "accept" {
                    break;
                }
            }
        }
    };

    let _ = timeout(
        Duration::from_secs(30),
        tokio_network.run_until(slave_accepted),
    )
    .await;

    Ok(())
}
