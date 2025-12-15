use std::rc::Rc;
use std::time::SystemTime;

use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use rptp::{
    bmca::{DefaultDS, Priority1, Priority2},
    clock::{ClockAccuracy, ClockIdentity, ClockQuality, LocalClock, StepsRemoved},
    log::NOOP_CLOCK_METRICS,
    message::TimeScale,
    port::{DomainNumber, PortNumber, SingleDomainPortMap},
    servo::{Servo, SteppingServo},
    time::TimeStamp,
};
use rptp_daemon::net::MulticastSocket;
use rptp_daemon::node::TokioPortsLoop;
use rptp_daemon::ordinary::OrdinaryTokioClock;
use rptp_daemon::timestamping::{ClockRxTimestamping, ClockTxTimestamping};
use rptp_daemon::virtualclock::VirtualClock;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let domain = DomainNumber::new(0);

    // Derive the time to start with coarsely from current system time
    const TAI_MINUS_UTC_SECONDS: u64 = 37;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let now_secs = now.as_secs() + TAI_MINUS_UTC_SECONDS;
    let now_nanos = now.subsec_nanos();

    let virtual_clock = VirtualClock::new(TimeStamp::new(now_secs, now_nanos), 1.0);
    let local_clock = LocalClock::new(
        &virtual_clock,
        DefaultDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            Priority1::new(100),
            Priority2::new(127),
            ClockQuality::new(100, ClockAccuracy::Within10ms, 0xFFFF),
            TimeScale::Ptp,
        ),
        StepsRemoved::new(0),
        Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
    );
    let event_socket = Rc::new(MulticastSocket::event().await?);
    let general_socket = Rc::new(MulticastSocket::general().await?);

    let (system_tx, system_rx) = mpsc::unbounded_channel();
    let ordinary_clock =
        OrdinaryTokioClock::new(&local_clock, domain, PortNumber::new(1));

    let port = ordinary_clock.port(
        event_socket.clone(),
        general_socket.clone(),
        system_tx.clone(),
        ClockTxTimestamping::new(
            &virtual_clock,
            system_tx.clone(),
            ordinary_clock.domain_number(),
        ),
    );
    let portmap = SingleDomainPortMap::new(ordinary_clock.domain_number(), port);

    let ports_loop = TokioPortsLoop::new(
        portmap,
        event_socket,
        general_socket,
        ClockRxTimestamping::new(&virtual_clock),
        system_rx,
    )
    .await?;

    tracing::info!("Master ready");

    let result = timeout(
        Duration::from_secs(60),
        ports_loop.run_until(std::future::pending::<()>()),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Slave node timed out before clock sync",
        )
    })?;

    result
}
