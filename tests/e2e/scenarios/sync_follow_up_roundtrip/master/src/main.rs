use rptp_daemon::node::TokioNode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let master = TokioNode::master().await?;
    println!("Master ready");
    master.run().await
}
