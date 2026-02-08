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
    clock::SynchronizableClock,
    dataset::ClockDS,
    infra::infra_support::ForeignClockRecordsVec,
    message::SystemMessage,
    ordinary::OrdinaryClock,
    port::{DomainNumber, DomainPort},
    portstate::PortState,
    profile::PortProfile,
    timestamping::TxTimestamping,
};

use crate::log::TracingPortLog;
use crate::net::NetworkSocket;
use crate::node::{TokioPhysicalPort, TokioTimeout, TokioTimerHost};

/// Type alias for a fully wired Tokio-backed port state machine.
pub type TokioPort<'a, C, P, TS> =
    PortState<'a, DomainPort<C, P, TokioTimerHost, TS, TracingPortLog>, ForeignClockRecordsVec>;

/// Wrapper around [`OrdinaryClock`] that produces Tokio-wired ports.
///
/// This type owns the `rptp` ordinary clock domain object and provides convenience methods for
/// producing a configured [`TokioPort`].
pub struct OrdinaryTokioClock<C: SynchronizableClock + Clone> {
    ordinary_clock: OrdinaryClock<C, TokioTimeout>,
}

impl<C: SynchronizableClock + Clone> OrdinaryTokioClock<C> {
    /// Create a new ordinary clock for a specific domain.
    pub fn new(
        clock: C,
        default_ds: ClockDS,
        domain_number: DomainNumber,
        profile: PortProfile,
    ) -> Self {
        OrdinaryTokioClock {
            ordinary_clock: OrdinaryClock::new(clock, default_ds, domain_number, profile),
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
        physical_port: TokioPhysicalPort<N>,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
        timestamping: T,
    ) -> Option<TokioPort<'a, C, TokioPhysicalPort<N>, T>>
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
