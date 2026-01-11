//! Daemon-side log sinks for `rptp` domain events.
//!
//! The `rptp` core crate emits structured domain events via [`rptp::log::PortLog`]. The daemon
//! integrates this with the `tracing` ecosystem by providing a `PortLog` implementation that
//! formats those events as tracing spans/events.

use rptp::{log::PortEvent, log::PortLog, port::PortIdentity};

/// [`PortLog`] implementation that forwards domain [`PortEvent`]s to `tracing`.
///
/// This is a simple “human readable” sink intended for the daemon binary and tests. It preserves
/// the port identity as context and maps events to `info`/`warn`/`debug` levels.
#[derive(Clone, Copy, Debug)]
pub struct TracingPortLog {
    port_identity: PortIdentity,
}

impl TracingPortLog {
    /// Create a tracing log sink for a specific port.
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
