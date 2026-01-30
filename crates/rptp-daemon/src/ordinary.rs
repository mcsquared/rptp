//! Daemon-side assembly for an `rptp` ordinary clock.
//!
//! The `rptp` core crate provides [`rptp::ordinary::OrdinaryClock`], which is generic over
//! infrastructure boundaries (physical port, timer host, timestamping, logging, foreign records).
//!
//! This module fixes those generic parameters to the Tokio-based daemon implementations:
//! - [`TokioTimerHost`] for timeouts,
//! - [`TokioPhysicalPort`] for UDP transmission,
//! - [`TracingPortLog`] for port event logging, and
//! - [`ForeignClockRecordsVec`] as the foreign master record store (std/Vec-backed).

use tokio::sync::mpsc;

use rptp::{
    bmca::ClockDS,
    clock::{LocalClock, SynchronizableClock},
    infra::infra_support::ForeignClockRecordsVec,
    message::SystemMessage,
    ordinary::OrdinaryClock,
    port::{DomainNumber, DomainPort},
    portstate::PortState,
    timestamping::TxTimestamping,
};

use crate::log::TracingPortLog;
use crate::net::NetworkSocket;
use crate::node::{TokioPhysicalPort, TokioTimeout, TokioTimerHost};

/// Type alias for a fully wired Tokio-backed port state machine.
pub type TokioPort<'a, C, TS> =
    PortState<'a, DomainPort<'a, C, TokioTimerHost, TS, TracingPortLog>, ForeignClockRecordsVec>;

/// Wrapper around [`OrdinaryClock`] that produces Tokio-wired ports.
///
/// This type owns the `rptp` ordinary clock domain object and provides convenience methods for
/// producing a configured [`TokioPort`].
pub struct OrdinaryTokioClock<C: SynchronizableClock> {
    ordinary_clock: OrdinaryClock<C, TokioTimeout>,
}

impl<C: SynchronizableClock> OrdinaryTokioClock<C> {
    /// Create a new ordinary clock for a specific domain.
    pub fn new(
        local_clock: LocalClock<C>,
        default_ds: ClockDS,
        domain_number: DomainNumber,
    ) -> Self {
        OrdinaryTokioClock {
            ordinary_clock: OrdinaryClock::new(local_clock, default_ds, domain_number),
        }
    }

    /// Return the configured PTP domain number.
    pub fn domain_number(&self) -> DomainNumber {
        self.ordinary_clock.domain_number()
    }

    /// Create a fully wired port state machine for this ordinary clock.
    ///
    /// Returns `Some(port)` the first time (the clock's single port) and `None` if called again.
    /// The returned port:
    /// - uses the provided `physical_port` for UDP transmission,
    /// - schedules timeouts by sending `(DomainNumber, SystemMessage)` through `system_tx`, and
    /// - uses `timestamping` for egress timestamp feedback integration.
    pub fn port<'a, N, T>(
        &'a mut self,
        physical_port: &'a TokioPhysicalPort<N>,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
        timestamping: T,
    ) -> Option<TokioPort<'a, C, T>>
    where
        N: NetworkSocket,
        T: TxTimestamping,
    {
        self.ordinary_clock.port(
            physical_port,
            TokioTimerHost::new(self.ordinary_clock.domain_number(), system_tx.clone()),
            timestamping,
            TracingPortLog::new(),
            ForeignClockRecordsVec::new(),
        )
    }
}
