use crate::port::ParentPortIdentity;
use crate::time::TimeInterval;

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
    SynchronizationFault,
    Static(&'static str), // optional catch-all
}

pub static NOOP_CLOCK_METRICS: NoopClockMetrics = NoopClockMetrics;

pub trait ClockMetrics {
    fn record_offset_from_master(&self, offset: TimeInterval);
}

pub struct NoopClockMetrics;

impl ClockMetrics for NoopClockMetrics {
    fn record_offset_from_master(&self, _offset: TimeInterval) {}
}
