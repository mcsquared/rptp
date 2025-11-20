use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use rptp::bmca::{LocalClockDS, Priority1, Priority2};
use rptp::clock::{ClockIdentity, ClockQuality, FakeClock, LocalClock};
use rptp::port::{DomainNumber, PortNumber, SingleDomainPortMap};
use rptp::time::TimeStamp;
use rptp_daemon::net::MulticastSocket;
use rptp_daemon::node::TokioPortsLoop;
use rptp_daemon::ordinary::ordinary_clock_port;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let domain = DomainNumber::new(0);

    let local_clock = LocalClock::new(
        FakeClock::new(TimeStamp::new(0, 0)),
        LocalClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            Priority1::new(250),
            Priority2::new(255),
            ClockQuality::new(250, 0xFE, 0xFFFF),
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
        system_tx,
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

    tracing::info!("Slave ready");

    let clock_sync = async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;

            // TODO: this is a quite weak acceptance criteria for clock sync, so it's probably
            // better to improve it in the future, to determine the ptp4l master's time through
            // some side channel. However, it indicates at least the successful message exchange
            // and clock update against ptp4l, which is already a good start.
            if local_clock.now() != TimeStamp::new(0, 0) {
                break;
            }
        }
    };

    let result = timeout(Duration::from_secs(30), ports_loop.run_until(clock_sync))
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
