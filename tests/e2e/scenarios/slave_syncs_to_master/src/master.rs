use std::rc::Rc;

use tokio::net::UdpSocket;
use tokio::time::{Duration, timeout};

use rptp::bmca::LocalClockDS;
use rptp::clock::{ClockIdentity, ClockQuality, FakeClock, LocalClock};
use rptp::time::TimeStamp;
use rptp_daemon::net::MulticastPort;
use rptp_daemon::node::TokioNode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let local_clock = LocalClock::new(
        Rc::new(FakeClock::new(TimeStamp::new(10, 500_000_000))),
        LocalClockDS::new(
            ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            ClockQuality::new(240, 0xFE, 0xFFFF),
        ),
    );
    let event_port = MulticastPort::ptp_event_testing_port().await?;
    let general_port = MulticastPort::ptp_general_testing_port().await?;

    let master = TokioNode::initializing(&local_clock, event_port, general_port).await?;

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

    let _ = timeout(Duration::from_secs(30), master.run_until(slave_accepted)).await;

    Ok(())
}
