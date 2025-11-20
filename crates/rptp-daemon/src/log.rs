use rptp::log::PortLog;
use rptp::port::PortIdentity;

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
    fn message_sent(&self, msg: &str) {
        tracing::debug!("{}: Sent {}", self.port_identity, msg);
    }

    fn message_received(&self, msg: &str) {
        tracing::debug!("{}: Received {}", self.port_identity, msg);
    }

    fn state_transition(&self, from: &str, to: &str, reason: &str) {
        tracing::info!("{}: {} to {} - {}", self.port_identity, from, to, reason);
    }
}
