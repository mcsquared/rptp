use rptp::{log::PortEvent, log::PortLog, port::PortIdentity};

#[derive(Clone, Copy, Debug)]
pub struct TracingPortLog {
    port_identity: PortIdentity,
}

impl TracingPortLog {
    pub fn new(port_identity: PortIdentity) -> Self {
        Self { port_identity }
    }
}

impl PortLog for TracingPortLog {
    fn port_event(&self, event: PortEvent) {
        match event {
            PortEvent::Initialized => {
                tracing::info!("{}: Initialized", self.port_identity);
            }
            PortEvent::RecommendedSlave { parent } => {
                tracing::info!(
                    "{}: Recommended Slave, parent {}",
                    self.port_identity,
                    parent
                );
            }
            PortEvent::RecommendedMaster => {
                tracing::info!("{}: Recommended Master", self.port_identity);
            }
            PortEvent::MasterClockSelected { parent } => {
                tracing::info!(
                    "{}: Master Clock Selected, parent {}",
                    self.port_identity,
                    parent
                );
            }
            PortEvent::AnnounceReceiptTimeout => {
                tracing::info!("{}: Announce Receipt Timeout", self.port_identity);
            }
            PortEvent::QualifiedMaster => {
                tracing::info!("{}: Qualified Master", self.port_identity);
            }
            PortEvent::SynchronizationFault => {
                tracing::warn!("{}: Synchronization Fault", self.port_identity);
            }
            PortEvent::MessageReceived(msg) => {
                tracing::debug!("{}: Message Received: {}", self.port_identity, msg);
            }
            PortEvent::MessageSent(msg) => {
                tracing::debug!("{}: Message Sent: {}", self.port_identity, msg);
            }
            PortEvent::Static(desc) => {
                tracing::info!("{}: {}", self.port_identity, desc);
            }
        }
    }
}
