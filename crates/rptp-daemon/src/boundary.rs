//! Daemon-side assembly for an `rptp` boundary clock.
//!
//! The `rptp` core crate provides [`rptp::boundary::BoundaryClock`], which is generic over
//! infrastructure boundaries (physical port, timer host, timestamping, logging, foreign records,
//! port notification, and foreign grandmaster candidates).
//!
//! This module fixes those generic parameters to the Tokio-based daemon implementations:
//! - [`TokioTimerHost`] for timeouts,
//! - [`TokioPhysicalPort`] for UDP transmission,
//! - [`TracingPortLog`] for port event logging,
//! - [`ForeignClockRecordsVec`] as the per-port foreign master record store,
//! - [`MultiPortForeignCandidates`] for cross-port best-foreign aggregation, and
//! - [`MultiPortNotification`] for state decision event delivery to all ports.

use tokio::sync::mpsc;

use rptp::{
    clock::SynchronizableClock,
    dataset::ClockDS,
    infra::infra_support::{
        ForeignClockRecordsVec, MultiPortForeignCandidates, MultiPortNotification,
    },
    message::SystemMessage,
    port::{DomainNumber, DomainPort},
    portstate::PortState,
    timestamping::TxTimestamping,
};

use crate::log::TracingPortLog;
use crate::net::NetworkSocket;
use crate::node::{TokioPhysicalPort, TokioTimeout, TokioTimerHost};

use rptp::boundary::BoundaryClock;

/// Type alias for a fully wired Tokio-backed port state machine (shared with ordinary clock).
pub type TokioPort<C, P, TS, SDE> =
    PortState<DomainPort<C, P, TokioTimerHost, TS, TracingPortLog>, ForeignClockRecordsVec, SDE>;
type BoundaryInner<C> =
    BoundaryClock<C, TokioTimeout, MultiPortForeignCandidates, MultiPortNotification<TokioTimeout>>;
type BoundaryEvent<'a, C> = &'a BoundaryInner<C>;
type BoundaryTokioPort<'a, C, N, T> = TokioPort<C, TokioPhysicalPort<N>, T, BoundaryEvent<'a, C>>;

/// Wrapper around [`BoundaryClock`] that produces Tokio-wired ports.
///
/// This type owns the `rptp` boundary clock domain object and provides convenience methods for
/// producing configured [`TokioPort`] instances. Uses [`MultiPortForeignCandidates`] and
/// [`MultiPortNotification`] for the std-based daemon environment.
pub struct BoundaryTokioClock<C: SynchronizableClock + Clone> {
    boundary_clock: BoundaryInner<C>,
}

impl<C: SynchronizableClock + Clone> BoundaryTokioClock<C> {
    /// Create a new boundary clock for a specific domain.
    ///
    /// `num_ports` must be at least 1. The clock will yield up to `num_ports` ports via
    /// [`port`](Self::port).
    pub fn new(clock: C, default_ds: ClockDS, domain_number: DomainNumber, num_ports: u16) -> Self {
        BoundaryTokioClock {
            boundary_clock: BoundaryClock::new(
                clock,
                default_ds,
                domain_number,
                num_ports,
                MultiPortForeignCandidates::new(),
                MultiPortNotification::new(),
            ),
        }
    }

    /// Return the configured PTP domain number.
    pub fn domain_number(&self) -> DomainNumber {
        self.boundary_clock.domain_number()
    }

    /// Create a fully wired port state machine for the next port.
    ///
    /// Returns `Some(port)` for each port (1..=num_ports) and `None` once exhausted.
    /// The returned port uses the provided `physical_port` for UDP transmission, schedules
    /// timeouts via `system_tx`, and uses `timestamping` for egress timestamp feedback.
    pub fn port<N, T>(
        &self,
        physical_port: TokioPhysicalPort<N>,
        system_tx: mpsc::UnboundedSender<(DomainNumber, SystemMessage)>,
        timestamping: T,
    ) -> Option<BoundaryTokioPort<'_, C, N, T>>
    where
        N: NetworkSocket,
        T: TxTimestamping,
    {
        self.boundary_clock.port(
            physical_port,
            TokioTimerHost::new(self.boundary_clock.domain_number(), system_tx.clone()),
            timestamping,
            TracingPortLog::new(),
            ForeignClockRecordsVec::new(),
        )
    }
}
