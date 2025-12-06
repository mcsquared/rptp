use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use rptp::bmca::{DefaultDS, Priority1, Priority2};
use rptp::clock::{ClockIdentity, ClockQuality, LocalClock, StepsRemoved};
use rptp::log::NOOP_CLOCK_METRICS;
use rptp::port::{DomainNumber, PortNumber, SingleDomainPortMap};
use rptp::servo::{Servo, SteppingServo};
use rptp::test_support::FakeClock;
use rptp::time::TimeStamp;
use rptp_daemon::net::MulticastSocket;
use rptp_daemon::node::TokioPortsLoop;
use rptp_daemon::ordinary::ordinary_clock_port;
use rptp_daemon::timestamping::ClockTimestamping;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let domain = DomainNumber::new(0);

    let fake_clock = FakeClock::new(TimeStamp::new(10, 500_000_000));
    let local_clock = LocalClock::new(
        &fake_clock,
        DefaultDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            Priority1::new(120),
            Priority2::new(127),
            ClockQuality::new(240, 0xFE, 0xFFFF),
        ),
        StepsRemoved::new(0),
        Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
    );
    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);

    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let timestamping = ClockTimestamping::new(&fake_clock, system_tx.clone(), domain);
    let port = ordinary_clock_port(
        &local_clock,
        domain,
        event_socket.clone(),
        general_socket.clone(),
        system_tx,
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

    tracing::info!("Master ready");

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
        ports_loop.run_until(slave_accepted),
    )
    .await;

    Ok(())
}
