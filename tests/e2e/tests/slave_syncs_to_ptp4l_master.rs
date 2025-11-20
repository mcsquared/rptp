use std::time::Duration;

use bollard::Docker;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};

use rptp_e2e_tests::{PrintLog, TestContainer, TestImage, TestNetwork};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn slave_syncs_to_ptp4l_master() -> anyhow::Result<()> {
        let docker = Docker::connect_with_local_defaults()?;

        let image = TestImage::new(docker.clone(), "e2e-scenarios");
        let tag = image.build().await?;

        let net = TestNetwork::new(docker.clone(), "ptpnet").await?;

        let master = TestContainer::new(
            GenericImage::new(image.name(), &tag)
                .with_wait_for(WaitFor::message_on_stdout("assuming the grand master role"))
                .with_log_consumer(PrintLog { prefix: "ptp4l" })
                .with_cmd(["ptp4l", "-i", "eth0", "-S", "-m", "-l", "6", "-4"])
                .with_network(net.name())
                .start()
                .await?,
            docker.clone(),
        );

        let slave = TestContainer::new(
            GenericImage::new(image.name(), &tag)
                .with_wait_for(WaitFor::message_on_stdout("Slave ready"))
                .with_env_var("RUST_LOG", "debug")
                .with_log_consumer(PrintLog { prefix: "slave" })
                .with_cmd(["/app/slave-syncs-to-ptp4l-master"])
                .with_network(net.name())
                .start()
                .await?,
            docker.clone(),
        );

        let result = slave.wait_for_exit(Duration::from_secs(60)).await;

        match result {
            Ok(slave_status) => {
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
