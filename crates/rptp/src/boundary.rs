//! Boundary clock assembly.
//!
//! A **boundary clock** is a PTP clock with multiple ports in a single domain.
//! Each port can be in a different state (master, slave, passive, etc.); the clock
//! shares a single local clock and aggregates foreign master candidates across all ports.
//!
//! This module provides a convenience builder that wires the core domain objects
//! (port, BMCA, and storage seams) into ready-to-run [`PortState`] state machines.
//!
//! Networking, timers, timestamping, and logging are provided by the caller via
//! domain-facing traits ([`PhysicalPort`], [`TimerHost`], [`TxTimestamping`], [`PortLog`]).
//!
//! Infrastructure supplies the storage and notification implementations
//! ([`ForeignGrandMasterCandidates`], [`PortNotification`]). For `std` environments,
//! see [`MultiPortForeignCandidates`](crate::infra::infra_support::MultiPortForeignCandidates) and
//! [`MultiPortNotification`](crate::infra::infra_support::MultiPortNotification).

use crate::bmca::{BestMasterClockAlgorithm, StateDecisionEvent, StateDecisionEventTrigger};
use crate::clock::SynchronizableClock;
use crate::dataset::ClockDS;
use crate::foreign::{
    BestForeignRecord, BestForeignSnapshot, ForeignClockRecords, ForeignGrandMasterCandidates,
};
use crate::log::PortLog;
use crate::message::{StateDecisionSnapshot, SystemMessage};
use crate::port::{
    DomainNumber, DomainPort, PhysicalPort, PortEnumeration, PortIdentity, PortNotification,
    PortNumber, Timeout, TimerHost,
};
use core::marker::PhantomData;

use crate::portstate::PortState;
use crate::profile::PortProfile;
use crate::timestamping::TxTimestamping;

/// Fully wired port state machine returned by [`BoundaryClock::port`].
pub type BoundaryPortState<C, P, TH, TS, L, R, SDE> =
    PortState<DomainPort<C, P, TH, TS, L>, R, SDE>;

/// A boundary clock: one clock, one domain, multiple ports.
///
/// This type owns the immutable configuration and shared collaborators:
/// - the local clock discipline boundary ([`LocalClock`]),
/// - the local default dataset used by BMCA ([`ClockDS`]),
/// - the domain number,
/// - a shared store for best-foreign snapshots across ports (`E_best`),
/// - and a notification handler to deliver state decision events to all ports.
///
/// `FGC` and `PN` are supplied by infrastructure so the domain core remains
/// std-agnostic. For `std` environments, use
/// [`MultiPortForeignCandidates`](crate::infra::infra_support::MultiPortForeignCandidates) and
/// [`MultiPortNotification`](crate::infra::infra_support::MultiPortNotification).
///
/// Port creation is exhaustive: [`port`](Self::port) returns `Some` for each
/// port number 1..=num_ports, then `None` thereafter.
pub struct BoundaryClock<C, T, FGC, PN>
where
    C: SynchronizableClock + Clone,
{
    clock: C,
    default_ds: ClockDS,
    domain_number: DomainNumber,
    port_enumeration: PortEnumeration,
    foreign_candidates: FGC,
    port_notification: PN,
    _timeout: PhantomData<T>,
}

impl<C, T, FGC, PN> BoundaryClock<C, T, FGC, PN>
where
    C: SynchronizableClock + Clone,
    T: Timeout,
    FGC: ForeignGrandMasterCandidates,
    PN: PortNotification<T>,
{
    /// Create a new boundary clock configuration.
    ///
    /// `default_ds` is the local clock dataset used by BMCA as the local candidate (`D_0`).
    /// `num_ports` must be at least 1. `foreign_candidates` and `port_notification` are
    /// supplied by infrastructure (e.g.
    /// [`MultiPortForeignCandidates`](crate::infra::infra_support::MultiPortForeignCandidates) and
    /// [`MultiPortNotification`](crate::infra::infra_support::MultiPortNotification) for `std`).
    ///
    /// # Panics
    /// Panics if `num_ports` is zero.
    pub fn new(
        clock: C,
        default_ds: ClockDS,
        domain_number: DomainNumber,
        num_ports: u16,
        foreign_candidates: FGC,
        port_notification: PN,
    ) -> Self {
        BoundaryClock {
            clock,
            default_ds,
            domain_number,
            port_enumeration: PortEnumeration::sequential(num_ports),
            foreign_candidates,
            port_notification,
            _timeout: PhantomData,
        }
    }

    /// Return the local clock discipline boundary.
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Return the configured PTP domain number.
    pub fn domain_number(&self) -> DomainNumber {
        self.domain_number
    }

    /// Construct the port state machine for the next port.
    ///
    /// Yields `Some(port_state)` for each port (1..=num_ports) and `None` once exhausted.
    /// Assembly is the same as for an ordinary clock: [`DomainPort`], BMCA, and
    /// state decision event delivery via [`PortNotification`].
    ///
    /// The initial state is `INITIALIZING`. The caller must deliver
    /// `SystemMessage::Initialized` to start the state machine.
    pub fn port<P, TH, TS, R, L>(
        &self,
        physical_port: P,
        timer_host: TH,
        timestamping: TS,
        log: L,
        foreign_clock_records: R,
    ) -> Option<BoundaryPortState<C, P, TH, TS, L, R, &Self>>
    where
        P: PhysicalPort,
        TH: TimerHost<Timeout = T>,
        TS: TxTimestamping,
        R: ForeignClockRecords,
        L: PortLog,
    {
        let port_number = self.port_enumeration.next()?;
        let port_identity = PortIdentity::new(self.default_ds.identity(), port_number);

        let state_decision_timeout =
            timer_host.timeout(SystemMessage::StateDecisionEvent(StateDecisionSnapshot {
                d_0: self.default_ds,
                e_best: BestForeignSnapshot::Empty,
            }));
        self.port_notification.add_port(state_decision_timeout);

        let domain_port = DomainPort::new(
            self.clock.clone(),
            port_identity,
            physical_port,
            timer_host,
            timestamping,
            log,
            self.domain_number,
        );

        let bmca = BestMasterClockAlgorithm::new(self.default_ds, port_number);
        let best_foreign = BestForeignRecord::new(port_identity, foreign_clock_records);
        let state_decision_trigger =
            StateDecisionEventTrigger::new(self, best_foreign.snapshot(), port_number);

        Some(PortProfile::default().initializing(
            domain_port,
            bmca,
            best_foreign,
            state_decision_trigger,
        ))
    }
}

impl<C, T, FGC, PN> StateDecisionEvent for BoundaryClock<C, T, FGC, PN>
where
    C: SynchronizableClock + Clone,
    T: Timeout,
    FGC: ForeignGrandMasterCandidates,
    PN: PortNotification<T>,
{
    fn trigger(&self, port_number: PortNumber, e_rbest: BestForeignSnapshot) {
        self.foreign_candidates.remember(port_number, e_rbest);
        let e_best = self.foreign_candidates.best();
        self.port_notification.notify_all(StateDecisionSnapshot {
            d_0: self.default_ds,
            e_best,
        });
    }
}
