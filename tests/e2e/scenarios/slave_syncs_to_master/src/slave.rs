use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use rptp::{
    bmca::{DefaultDS, Priority1, Priority2},
    clock::{
        ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, LocalClock, StepsRemoved, TimeScale,
    },
    log::NOOP_CLOCK_METRICS,
    port::{DomainNumber, PortNumber, SingleDomainPortMap},
    servo::{Servo, SteppingServo},
    test_support::FakeClock,
    time::TimeStamp,
};
use rptp_daemon::net::MulticastSocket;
use rptp_daemon::node::TokioPortsLoop;
use rptp_daemon::ordinary::OrdinaryTokioClock;
use rptp_daemon::timestamping::{ClockRxTimestamping, ClockTxTimestamping};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let domain = DomainNumber::new(0);

    let fake_clock = FakeClock::new(TimeStamp::new(0, 0), TimeScale::Ptp);
    let local_clock = LocalClock::new(
        &fake_clock,
        DefaultDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            Priority1::new(250),
            Priority2::new(255),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Within10ms, 0xFFFF),
        ),
        StepsRemoved::new(0),
        Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
    );
    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);

    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let ordinary_clock = OrdinaryTokioClock::new(&local_clock, domain, PortNumber::new(1));

    let port = ordinary_clock.port(
        event_socket.clone(),
        general_socket.clone(),
        system_tx.clone(),
        ClockTxTimestamping::new(
            &fake_clock,
            system_tx.clone(),
            ordinary_clock.domain_number(),
        ),
    );
    let portmap = SingleDomainPortMap::new(ordinary_clock.domain_number(), port);

    let ports_loop = TokioPortsLoop::new(
        portmap,
        event_socket,
        general_socket,
        ClockRxTimestamping::new(&fake_clock),
        system_rx,
    )
    .await?;

    tracing::info!("Slave ready");

    let clock_sync = async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if local_clock.now() == TimeStamp::new(10, 500_000_000) {
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
