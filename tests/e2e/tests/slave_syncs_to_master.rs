use std::time::Duration;

use bollard::Docker;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};

use rptp_e2e_tests::{PrintLog, TestContainer, TestImage, TestNetwork};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn slave_syncs_to_master() -> anyhow::Result<()> {
        let docker = Docker::connect_with_local_defaults()?;

        let image = TestImage::new(docker.clone(), "e2e-scenarios");
        let tag = image.build().await?;

        let net = TestNetwork::new(docker.clone(), "ptpnet").await?;

        let slave = TestContainer::new(
            GenericImage::new(image.name(), &tag)
                .with_wait_for(WaitFor::message_on_stdout("Slave ready"))
                .with_log_consumer(PrintLog { prefix: "slave" })
                .with_cmd(["/app/slave-syncs-to-master-slave"])
                .with_network(net.name())
                .start()
                .await?,
            docker.clone(),
        );

        let master = TestContainer::new(
            GenericImage::new(image.name(), &tag)
                .with_wait_for(WaitFor::message_on_stdout("Master ready"))
                .with_log_consumer(PrintLog { prefix: "master" })
                .with_cmd(["/app/slave-syncs-to-master-master"])
                .with_network(net.name())
                .start()
                .await?,
            docker.clone(),
        );

        let result = tokio::try_join!(
            master.wait_for_exit(Duration::from_secs(60)),
            slave.wait_for_exit(Duration::from_secs(60)),
        );

        match result {
            Ok((master_status, slave_status)) => {
                assert!(master_status == 0);
                assert!(slave_status == 0);
            }
            Err(_) => {
                panic!("Test failed due to error or timeout");
            }
        }

        slave.rm().await?;
        master.stop().await?;
        master.rm().await?;

        Ok(())
    }
}
