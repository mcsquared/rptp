use crate::port::ParentPortIdentity;

pub trait PortLog {
    fn message_sent(&self, msg: &str);
    fn message_received(&self, msg: &str);
    fn port_event(&self, event: PortEvent);
}

pub struct NoopPortLog;

impl PortLog for NoopPortLog {
    fn message_sent(&self, _msg: &str) {}
    fn message_received(&self, _msg: &str) {}
    fn port_event(&self, _event: PortEvent) {}
}

pub enum PortEvent {
    Initialized,
    RecommendedSlave { parent: ParentPortIdentity },
    RecommendedMaster,
    MasterClockSelected { parent: ParentPortIdentity },
    AnnounceReceiptTimeout,
    QualifiedMaster,
    Static(&'static str), // optional catch-all
}
