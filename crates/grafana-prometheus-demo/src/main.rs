mod loopback;
mod metrics;
mod node;

use std::rc::Rc;

use rptp::bmca::ClockDS;
use rptp::bmca::{Priority1, Priority2};
use rptp::clock::{ClockAccuracy, ClockClass, ClockIdentity, ClockQuality, StepsRemoved};
use rptp::port::DomainNumber;
use rptp::profile::PortProfile;
use rptp::servo::{Drift, ServoThreshold};
use rptp::time::{TimeInterval, TimeStamp};

use crate::loopback::LoopbackSocket;
use crate::metrics::run_metrics_server;
use crate::node::{PiServoConfig, PrometheusNode, PrometheusNodeConfig};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    rptp_daemon::init_tracing();

    let domain = DomainNumber::new(0);

    let (master_event_sock, slave_event_sock) = LoopbackSocket::pair();
    let (master_gen_sock, slave_gen_sock) = LoopbackSocket::pair();

    let master_event_sock = Rc::new(master_event_sock);
    let master_gen_sock = Rc::new(master_gen_sock);
    let slave_event_sock = Rc::new(slave_event_sock);
    let slave_gen_sock = Rc::new(slave_gen_sock);

    let master_config = PrometheusNodeConfig {
        port_number: 1.into(),
        clock_start: TimeStamp::new(2, 0),
        clock_rate: 1.0,
        default_ds: ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x42]),
            Priority1::new(60),
            Priority2::new(127),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Unknown, 0xFFFF),
            StepsRemoved::new(0),
        ),
        profile: PortProfile::default(),
        servo: PiServoConfig {
            first_step_threshold: ServoThreshold::disabled(),
            step_threshold: ServoThreshold::disabled(),
            drift_min: Drift::from_ppb(-100000),
            drift_max: Drift::from_ppb(100000),
            drift_update_interval: TimeInterval::new(4, 0),
            kp: 1e-3,
            ki: 1e-6,
        },
        clock_steps: vec![
            (
                std::time::Duration::from_secs(90),
                TimeInterval::new(0, 750_000_000),
            ),
            (
                std::time::Duration::from_secs(10),
                TimeInterval::new(0, 250_000_000),
            ),
        ],
    };

    let slave_config = PrometheusNodeConfig {
        port_number: 2.into(),
        clock_start: TimeStamp::new(0, 0),
        clock_rate: 1.0,
        default_ds: ClockDS::new(
            ClockIdentity::new(&[0x00, 0x1B, 0x19, 0xFF, 0xFE, 0x00, 0x00, 0x02]),
            Priority1::new(180),
            Priority2::new(200),
            ClockQuality::new(ClockClass::Default, ClockAccuracy::Unknown, 0xFFFF),
            StepsRemoved::new(0),
        ),
        profile: PortProfile::default(),
        servo: PiServoConfig {
            first_step_threshold: ServoThreshold::new(TimeInterval::new(1, 0)),
            step_threshold: ServoThreshold::new(TimeInterval::new(0, 500_000_000)),
            drift_min: Drift::from_ppb(-100000),
            drift_max: Drift::from_ppb(100000),
            drift_update_interval: TimeInterval::new(4, 0),
            kp: 5e-3,
            ki: 3e-6,
        },
        clock_steps: vec![],
    };

    let master = PrometheusNode::new(domain, master_event_sock, master_gen_sock, master_config);
    let slave = PrometheusNode::new(domain, slave_event_sock, slave_gen_sock, slave_config);

    // Expose Prometheus metrics on a simple HTTP endpoint for Grafana.
    tokio::spawn(run_metrics_server("0.0.0.0:9898"));

    tokio::try_join!(master.run(), slave.run())?;

    Ok(())
}
