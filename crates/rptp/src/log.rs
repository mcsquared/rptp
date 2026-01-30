//! Logging and metrics adapter surfaces.
//!
//! The `rptp` domain core emits coarse-grained events and numeric measurements through small
//! domain-facing traits. Infrastructure is expected to provide concrete implementations that route
//! those signals into whatever logging/metrics system is used (e.g. `tracing`, Prometheus).
//!
//! This module intentionally keeps the interface minimal:
//! - [`PortLog`] receives discrete [`PortEvent`] values from port state code.
//! - [`ClockMetrics`] receives numeric time-series values produced by the servo/discipline pipeline.
//!
//! The provided `Noop*` implementations are convenient defaults for tests and examples.

use crate::port::{ParentPortIdentity, PortIdentity};
use crate::time::TimeInterval;

/// Logging sink for port-level events.
///
/// The domain core calls this interface whenever something relevant happens in the port state
/// machine (state entry, recommendations, timeouts, message send/receive).
///
/// Implementations are expected to be lightweight; expensive formatting or I/O should typically be
/// deferred to the infrastructure logging backend.
pub trait PortLog {
    /// Record a port event.
    fn port_event(&self, port_identity: PortIdentity, event: PortEvent);
}

/// `PortLog` implementation that drops all events.
///
/// This is mainly used in unit tests and simple harnesses.
#[allow(dead_code)]
pub(crate) struct NoopPortLog;

impl PortLog for NoopPortLog {
    fn port_event(&self, _port_identity: PortIdentity, _event: PortEvent) {}
}

/// Discrete events emitted by the port state machine.
///
/// This is intentionally coarse-grained: it is designed to be stable across refactors and useful
/// as a “what happened” stream in tests, demos, and early observability experiments.
///
/// Some variants carry a `&'static str` label to avoid formatting allocations in `no_std` contexts.
pub enum PortEvent {
    /// The port finished initialization (`INITIALIZING → LISTENING` transition applied).
    Initialized,
    /// BMCA recommended transitioning toward slave with the selected parent.
    RecommendedSlave { parent: ParentPortIdentity },
    /// BMCA recommended transitioning toward master/pre-master.
    RecommendedMaster,
    /// The local clock reports it has selected/synchronized to the master clock.
    MasterClockSelected { parent: ParentPortIdentity },
    /// Announce receipt timeout expired.
    AnnounceReceiptTimeout,
    /// Pre-master qualification timeout expired; port becomes master.
    QualifiedMaster,
    /// Servo reported a synchronization fault while in slave mode.
    SynchronizationFault,
    /// A message was received (label identifies the message kind).
    MessageReceived(&'static str),
    /// A message was sent (label identifies the message kind).
    MessageSent(&'static str),
    /// Catch-all marker for ad-hoc instrumentation.
    Static(&'static str), // optional catch-all
}

/// A [`ClockMetrics`] instance that discards all measurements.
pub static NOOP_CLOCK_METRICS: NoopClockMetrics = NoopClockMetrics;

/// Metrics sink for clock/servo measurements.
///
/// This is used by the servo pipeline to record time-series measurements such as the current
/// offset estimate.
pub trait ClockMetrics {
    /// Record the current estimated offset from the selected master.
    fn record_offset_from_master(&self, offset: TimeInterval);
}

/// `ClockMetrics` implementation that drops all measurements.
pub struct NoopClockMetrics;

impl ClockMetrics for NoopClockMetrics {
    fn record_offset_from_master(&self, _offset: TimeInterval) {}
}
