use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::time::{Duration, timeout};

use rptp::bmca::LocalClockDS;
use rptp::clock::{Clock, ClockIdentity, ClockQuality, FakeClock};
use rptp::time::TimeStamp;
use rptp_daemon::net::MulticastPort;
use rptp_daemon::node::TokioNode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));
    let event_port = MulticastPort::ptp_event_testing_port().await?;
    let general_port = MulticastPort::ptp_general_testing_port().await?;
    let localds = LocalClockDS::new(
        ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
        ClockQuality::new(250, 0xFE, 0xFFFF),
    );

    let slave = TokioNode::initializing(clock.clone(), event_port, general_port, localds).await?;

    println!("Slave ready");

    let clock_sync = async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if clock.now() == TimeStamp::new(10, 500_000_000) {
                break;
            }
        }
    };

    let result = timeout(Duration::from_secs(30), slave.run_until(clock_sync))
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
