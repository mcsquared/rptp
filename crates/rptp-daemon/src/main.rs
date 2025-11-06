pub mod net;
pub mod node;

use std::rc::Rc;

use rptp::bmca::LocalClockDS;
use rptp::clock::{ClockIdentity, ClockQuality, FakeClock, LocalClock};

use crate::net::MulticastPort;
use crate::node::TokioNode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let event_port = MulticastPort::ptp_event_port().await?;
    let general_port = MulticastPort::ptp_general_port().await?;

    let local_clock = LocalClock::new(
        Rc::new(FakeClock::default()),
        LocalClockDS::new(
            ClockIdentity::new([0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x01]),
            ClockQuality::new(248, 0xFE, 0xFFFF),
        ),
    );
    let master = TokioNode::master(&local_clock, event_port, general_port).await?;
    println!("Master ready");
    master.run().await
}
