use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use rptp::{
    bmca::{ClockDS, Priority1, Priority2},
    clock::{Clock, ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, LocalClock, TimeScale},
    log::NOOP_CLOCK_METRICS,
    port::{DomainNumber, SingleDomainPortMap},
    servo::{Servo, SteppingServo},
    test_support::FakeClock,
    time::TimeStamp,
};
use rptp_daemon::net::MulticastSocket;
use rptp_daemon::node::{TokioPhysicalPort, TokioPortsLoop};
use rptp_daemon::ordinary::OrdinaryTokioClock;
use rptp_daemon::timestamping::{ClockRxTimestamping, ClockTxTimestamping};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let fake_clock = FakeClock::new(TimeStamp::new(0, 0), TimeScale::Ptp);
    let default_ds = ClockDS::new(
        ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
        Priority1::new(250),
        Priority2::new(255),
        ClockQuality::new(ClockClass::Default, ClockAccuracy::Within10ms, 0xFFFF),
        rptp::clock::StepsRemoved::new(0),
    );

    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);

    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let mut ordinary_clock = OrdinaryTokioClock::new(
        LocalClock::new(
            &fake_clock,
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        ),
        default_ds,
        DomainNumber::new(0),
    );

    let domain_number = ordinary_clock.domain_number();
    let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
    let port = ordinary_clock
        .port(
            &physical_port,
            system_tx.clone(),
            ClockTxTimestamping::new(&fake_clock, system_tx.clone(), domain_number),
        )
        .expect("ordinary clock has one port");
    let portmap = SingleDomainPortMap::new(domain_number, port);

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

            // TODO: this is a quite weak acceptance criteria for clock sync, so it's probably
            // better to improve it in the future, to determine the ptp4l master's time through
            // some side channel. However, it indicates at least the successful message exchange
            // and clock update against ptp4l, which is already a good start.
            if fake_clock.now() != TimeStamp::new(0, 0) {
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
