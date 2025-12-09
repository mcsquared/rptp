use std::time::Duration;

use bollard::Docker;
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};

use rptp_e2e_tests::{PrintLog, TestContainer, TestImage, TestNetwork};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn ptp4l_slave_syncs_to_master() -> anyhow::Result<()> {
        let docker = Docker::connect_with_local_defaults()?;

        let scenarios_image = TestImage::new(docker.clone(), "e2e-scenarios");
        let scenarios_tag = scenarios_image.build().await?;

        let ptp4l_image = TestImage::ptp4l(docker.clone());
        let ptp4l_tag = ptp4l_image.build().await?;

        let net = TestNetwork::new(docker.clone(), "ptpnet").await?;

        let master = TestContainer::new(
            GenericImage::new(scenarios_image.name(), &scenarios_tag)
                .with_wait_for(WaitFor::message_on_stdout("Master ready"))
                .with_env_var("RUST_LOG", "debug")
                .with_log_consumer(PrintLog { prefix: "master" })
                .with_cmd(["/app/ptp4l-slave-syncs-to-master"])
                .with_network(net.name())
                .start()
                .await?,
            docker.clone(),
        );

        let ptp4l = TestContainer::new(
            GenericImage::new(ptp4l_image.name(), &ptp4l_tag)
                .with_wait_for(WaitFor::message_on_stdout(
                    "UNCALIBRATED to SLAVE on MASTER_CLOCK_SELECTED",
                ))
                .with_cap_drop("ALL")
                .with_cap_add("CAP_SYS_TIME")
                .with_log_consumer(PrintLog { prefix: "ptp4l" })
                .with_cmd(["ptp4l", "-i", "eth0", "-S", "-m", "-l", "6", "-4"])
                .with_network(net.name())
                .with_startup_timeout(Duration::from_secs(60))
                .start()
                .await?,
            docker.clone(),
        );

        master.stop().await?;
        master.rm().await?;
        ptp4l.stop().await?;
        ptp4l.rm().await?;

        Ok(())
    }
}
