use rptp_daemon::net::MulticastPort;
use rptp_daemon::node::TokioNode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let event_port = MulticastPort::ptp_event_testing_port().await?;
    let general_port = MulticastPort::ptp_general_testing_port().await?;

    let master = TokioNode::master(event_port, general_port).await?;
    println!("Master ready");
    master.run().await
}
