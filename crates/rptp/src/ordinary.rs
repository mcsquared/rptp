//! Ordinary clock assembly.
//!
//! An **ordinary clock** is a single PTP clock instance with a single port in a single domain.
//! In `rptp`, this module provides a small convenience builder that wires the core domain objects
//! (port, BMCA, and storage seams) into a ready-to-run [`PortState`] state machine.
//!
//! This is not an infrastructure implementation: networking, timers, timestamping, and logging are
//! provided by the caller via domain-facing traits ([`PhysicalPort`], [`TimerHost`],
//! [`TxTimestamping`], [`PortLog`]).
//!
//! For a complete end-to-end wiring example, see the `rptp-daemon` crate in this repository.

use core::cell::Cell;

use crate::bmca::{
    BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, ClockDS, ForeignClockRecords,
};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::PortLog;
use crate::port::{DomainNumber, DomainPort, PhysicalPort, PortNumber, TimerHost};
use crate::portstate::PortState;
use crate::profile::PortProfile;
use crate::timestamping::TxTimestamping;

/// An ordinary clock: one clock, one domain, one port.
///
/// This type owns the immutable configuration and shared collaborators that are reused when
/// constructing the port state machine:
/// - the local clock discipline boundary ([`LocalClock`]),
/// - the local default dataset used for BMCA comparisons ([`ClockDS`]),
/// - the domain and port numbers, and
/// - a shared store for best-foreign snapshots (`E_best` in BMCA terms).
///
/// The returned port state machine is constructed by [`OrdinaryClock::port`].
pub struct OrdinaryClock<C: SynchronizableClock> {
    local_clock: LocalClock<C>,
    default_ds: ClockDS,
    domain_number: DomainNumber,
    port_number: PortNumber,
    foreign_candidates: Cell<BestForeignSnapshot>,
}

impl<C: SynchronizableClock> OrdinaryClock<C> {
    /// Create a new ordinary clock configuration.
    ///
    /// `default_ds` is the local clock dataset used by BMCA as the local candidate (`D_0`).
    /// The `domain_number` and `port_number` identify the single port constructed by
    /// [`port`](Self::port).
    pub fn new(
        local_clock: LocalClock<C>,
        default_ds: ClockDS,
        domain_number: DomainNumber,
        port_number: PortNumber,
    ) -> Self {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);

        OrdinaryClock {
            local_clock,
            default_ds,
            domain_number,
            port_number,
            foreign_candidates,
        }
    }

    /// Return the local clock discipline boundary.
    pub fn local_clock(&self) -> &LocalClock<C> {
        &self.local_clock
    }

    /// Return the configured PTP domain number.
    pub fn domain_number(&self) -> DomainNumber {
        self.domain_number
    }

    /// Return the configured PTP port number.
    pub fn port_number(&self) -> PortNumber {
        self.port_number
    }

    /// Construct the port state machine for this ordinary clock.
    ///
    /// This performs the domain assembly:
    /// - wraps infrastructure-provided boundaries into a [`DomainPort`],
    /// - constructs the BMCA evaluator ([`BestMasterClockAlgorithm`]) using the local dataset and
    ///   shared best-foreign snapshot store, and
    /// - constructs a per-port best-foreign record store ([`BestForeignRecord`]) using
    ///   `foreign_clock_records`.
    ///
    /// The initial state is `INITIALIZING` as assembled by the default [`PortProfile`]. The caller
    /// is responsible for delivering `SystemMessage::Initialized` to start the state machine (see
    /// `PortState::dispatch_system`).
    pub fn port<'a, T, TS, S, L>(
        &'a self,
        physical_port: &'a dyn PhysicalPort,
        timer_host: T,
        timestamping: TS,
        log: L,
        foreign_clock_records: S,
    ) -> PortState<'a, DomainPort<'a, C, T, TS, L>, S>
    where
        T: TimerHost,
        TS: TxTimestamping,
        S: ForeignClockRecords,
        L: PortLog,
    {
        let domain_port = DomainPort::new(
            &self.local_clock,
            physical_port,
            timer_host,
            timestamping,
            log,
            self.domain_number,
            self.port_number,
        );

        let bmca = BestMasterClockAlgorithm::new(
            &self.default_ds,
            &self.foreign_candidates,
            self.port_number,
        );

        let best_foreign = BestForeignRecord::new(self.port_number, foreign_clock_records);

        PortProfile::default().initializing(domain_port, bmca, best_foreign)
    }
}
