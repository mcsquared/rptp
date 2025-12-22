use crate::port::ParentPortIdentity;
use crate::time::TimeInterval;

pub trait PortLog {
    fn port_event(&self, event: PortEvent);
}

#[allow(dead_code)]
pub(crate) struct NoopPortLog;

impl PortLog for NoopPortLog {
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
    MessageReceived(&'static str),
    MessageSent(&'static str),
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
