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
#[derive(Clone, Copy, Debug, Default)]
pub struct TracingPortLog;

impl TracingPortLog {
    /// Create a tracing log sink.
    pub fn new() -> Self {
        Self
    }
}

impl PortLog for TracingPortLog {
    fn port_event(&self, port_identity: PortIdentity, event: PortEvent) {
        match event {
            PortEvent::Initialized => {
                tracing::info!("{}: Initialized", port_identity);
            }
            PortEvent::RecommendedSlave { parent } => {
                tracing::info!("{}: Recommended Slave, parent {}", port_identity, parent);
            }
            PortEvent::RecommendedMaster => {
                tracing::info!("{}: Recommended Master", port_identity);
            }
            PortEvent::MasterClockSelected { parent } => {
                tracing::info!(
                    "{}: Master Clock Selected, parent {}",
                    port_identity,
                    parent
                );
            }
            PortEvent::AnnounceReceiptTimeout => {
                tracing::info!("{}: Announce Receipt Timeout", port_identity);
            }
            PortEvent::QualifiedMaster => {
                tracing::info!("{}: Qualified Master", port_identity);
            }
            PortEvent::SynchronizationFault => {
                tracing::warn!("{}: Synchronization Fault", port_identity);
            }
            PortEvent::MessageReceived(msg) => {
                tracing::debug!("{}: Message Received: {}", port_identity, msg);
            }
            PortEvent::MessageSent(msg) => {
                tracing::debug!("{}: Message Sent: {}", port_identity, msg);
            }
            PortEvent::Static(desc) => {
                tracing::info!("{}: {}", port_identity, desc);
            }
        }
    }
}
