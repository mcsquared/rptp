pub mod net;
pub mod node;

use crate::net::MulticastPort;
use crate::node::TokioNode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let event_port = MulticastPort::ptp_event_port().await?;
    let general_port = MulticastPort::ptp_general_port().await?;

    let master = TokioNode::master(event_port, general_port).await?;
    println!("Master ready");
    master.run().await
}
