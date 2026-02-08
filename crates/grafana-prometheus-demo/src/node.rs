use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::metrics::offset_gauge;
use rptp::clock::{Clock, SynchronizableClock, TimeScale};
use rptp::dataset::ClockDS;
use rptp::log::ClockMetrics;
use rptp::port::{DomainNumber, SingleDomainPortMap};
use rptp::profile::PortProfile;
use rptp::servo::{PiServoConfig, ServoConfig};
use rptp::time::{TimeInterval, TimeStamp};
use rptp_daemon::net::NetworkSocket;
use rptp_daemon::node::{TokioPhysicalPort, TokioPortsLoop};
use rptp_daemon::ordinary::OrdinaryTokioClock;
use rptp_daemon::timestamping::{ClockRxTimestamping, ClockTxTimestamping};
use rptp_daemon::virtualclock::VirtualClock;
use tokio::time::sleep;

pub struct TracingClockMetrics;

impl ClockMetrics for TracingClockMetrics {
    fn record_offset_from_master(&self, offset: rptp::time::TimeInterval) {
        let secs = offset.as_f64_seconds();
        offset_gauge().set(secs);
        tracing::info!("offset_from_master: {:.6} s", secs);
    }
}

pub static TRACING_CLOCK_METRICS: TracingClockMetrics = TracingClockMetrics;

pub struct PrometheusNodeConfig {
    pub clock_start: TimeStamp,
    pub clock_rate: f64,
    pub default_ds: ClockDS,
    pub profile: PortProfile,
    pub servo: PiServoConfig,
    /// Sequential step schedule: sleep `delay`, then apply `step_by`, then continue.
    pub clock_steps: Vec<(Duration, TimeInterval)>,
}

pub struct PrometheusNode<N: NetworkSocket> {
    domain: DomainNumber,
    event_socket: Rc<N>,
    general_socket: Rc<N>,
    config: PrometheusNodeConfig,
}

impl<N: NetworkSocket> PrometheusNode<N> {
    pub fn new(
        domain: DomainNumber,
        event_socket: Rc<N>,
        general_socket: Rc<N>,
        config: PrometheusNodeConfig,
    ) -> Self {
        Self {
            domain,
            event_socket,
            general_socket,
            config,
        }
    }

    pub async fn run(self) -> std::io::Result<()> {
        let Self {
            domain,
            event_socket,
            general_socket,
            config:
                PrometheusNodeConfig {
                    clock_start,
                    clock_rate,
                    default_ds,
                    profile,
                    servo,
                    clock_steps,
                },
        } = self;

        let virtual_clock = Arc::new(VirtualClock::new(clock_start, clock_rate, TimeScale::Ptp));

        if !clock_steps.is_empty() {
            let clock = virtual_clock.clone();
            tokio::spawn(async move {
                for (delay, step_by) in clock_steps {
                    sleep(delay).await;
                    if let Some(step_to) = clock.now().checked_add(step_by) {
                        clock.step(step_to);
                    }
                }
            });
        }

        let profile = profile.with_servo_config(ServoConfig::Pi {
            config: servo,
            metrics: &TRACING_CLOCK_METRICS,
        });

        let (system_tx, system_rx) = mpsc::unbounded_channel();
        let timestamping = ClockTxTimestamping::new(&*virtual_clock, system_tx.clone(), domain);

        let physical_port = TokioPhysicalPort::new(event_socket.clone(), general_socket.clone());
        let mut ordinary = OrdinaryTokioClock::new(&*virtual_clock, default_ds, domain, profile);
        let port_state = ordinary
            .port(physical_port, system_tx, &timestamping)
            .expect("ordinary clock has one port");
        let portmap = SingleDomainPortMap::new(domain, port_state);

        let ports_loop = TokioPortsLoop::new(
            portmap,
            event_socket,
            general_socket,
            ClockRxTimestamping::new(&*virtual_clock),
            system_rx,
        )
        .await?;

        ports_loop.run().await
    }
}
