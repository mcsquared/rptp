pub mod net;
pub mod node;

use std::rc::Rc;

use rptp::clock::FakeClock;
use rptp::time::TimeStamp;

use crate::net::MulticastPort;
use crate::node::TokioNode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let event_port = MulticastPort::ptp_event_port().await?;
    let general_port = MulticastPort::ptp_general_port().await?;

    let clock = Rc::new(FakeClock::new(TimeStamp::new(0, 0)));
    let master = TokioNode::master(clock, event_port, general_port).await?;
    println!("Master ready");
    master.run().await
}
