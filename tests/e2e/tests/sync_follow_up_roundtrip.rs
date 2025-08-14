use std::time::Duration;

use bollard::Docker;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};

use rptp_e2e_tests::{PrintLog, TestContainer, TestImage, TestNetwork};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn sync_follow_up_roundtrip() -> anyhow::Result<()> {
        let docker = Docker::connect_with_local_defaults()?;

        let image = TestImage::new(docker.clone(), "e2e-scenarios");
        let tag = image.build().await?;

        let net = TestNetwork::new(docker.clone(), "ptpnet").await?;

        let slave = TestContainer::new(
            GenericImage::new(image.name(), &tag)
                .with_wait_for(WaitFor::message_on_stdout("Slave ready"))
                .with_log_consumer(PrintLog { prefix: "slave" })
                .with_cmd(["/app/sync-follow-up-roundtrip-slave"])
                .with_network(net.name())
                .start()
                .await?,
            docker.clone(),
        );

        let master = TestContainer::new(
            GenericImage::new(image.name(), &tag)
                .with_wait_for(WaitFor::message_on_stdout("Master ready"))
                .with_log_consumer(PrintLog { prefix: "master" })
                .with_cmd(["/app/sync-follow-up-roundtrip-master"])
                .with_network(net.name())
                .start()
                .await?,
            docker.clone(),
        );

        let exit_code = slave.wait_for_exit(Duration::from_secs(30)).await?;
        assert_eq!(exit_code, 0, "Slave exited non-zero (code={exit_code})");

        slave.rm().await?;
        master.stop().await?;
        master.rm().await?;

        Ok(())
    }
}
