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

use core::cell::{Cell, RefCell};

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
use crate::portstate::PortState;
use crate::profile::PortProfile;
use crate::time::Duration;
use crate::timestamping::TxTimestamping;

/// Fully wired port state machine returned by [`OrdinaryClock::port`].
pub type OrdinaryPortState<'a, C, P, TH, TS, L, S> = PortState<'a, DomainPort<C, P, TH, TS, L>, S>;

/// An ordinary clock: one clock, one domain, one port.
///
/// This type owns the immutable configuration and shared collaborators that are reused when
/// constructing the port state machine:
/// - the local clock backend (`C: [`SynchronizableClock`]),
/// - the local default dataset used for BMCA comparisons ([`ClockDS`]),
/// - the domain and port numbers, and
/// - a shared store for best-foreign snapshots (`E_best` in BMCA terms).
///
/// The returned port state machine is constructed by [`OrdinaryClock::port`].
/// Port creation is exhaustive: [`port`](Self::port) returns `None` once the clock's single port
/// has been handed out.
pub struct OrdinaryClock<C: SynchronizableClock + Clone, T: Timeout> {
    clock: C,
    default_ds: ClockDS,
    domain_number: DomainNumber,
    port_enumeration: PortEnumeration,
    foreign_candidates: Cell<BestForeignSnapshot>,
    port_notification: SinglePortNotification<T>,
    profile: PortProfile,
}

impl<C: SynchronizableClock + Clone, T: Timeout> OrdinaryClock<C, T> {
    /// Create a new ordinary clock configuration.
    ///
    /// `default_ds` is the local clock dataset used by BMCA as the local candidate (`D_0`).
    /// An ordinary clock has exactly one port (port number 1); the enumeration is fixed to
    /// [`PortEnumeration::single`] internally.
    pub fn new(
        clock: C,
        default_ds: ClockDS,
        domain_number: DomainNumber,
        profile: PortProfile,
    ) -> Self {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);

        OrdinaryClock {
            clock,
            default_ds,
            domain_number,
            port_enumeration: PortEnumeration::single(),
            foreign_candidates,
            port_notification: SinglePortNotification::new(),
            profile,
        }
    }

    /// Return the local clock backend handle.
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Return the configured PTP domain number.
    pub fn domain_number(&self) -> DomainNumber {
        self.domain_number
    }

    /// Construct the port state machine for this ordinary clock.
    ///
    /// Yields `Some(port_state)` the first time (the clock's single port) and `None` thereafter.
    /// This performs the domain assembly:
    /// - wraps infrastructure-provided boundaries into a [`DomainPort`],
    /// - constructs the BMCA evaluator ([`BestMasterClockAlgorithm`]) using the local dataset and
    ///   shared best-foreign snapshot store, and
    /// - constructs a per-port best-foreign record store ([`BestForeignRecord`]) using
    ///   `foreign_clock_records`.
    /// - registers the port with `PortNotification` for state decision event delivery
    ///
    /// The initial state is `INITIALIZING` as assembled by the default [`PortProfile`]. The caller
    /// is responsible for delivering `SystemMessage::Initialized` to start the state machine (see
    /// `PortState::dispatch_system`).
    pub fn port<'a, P, TH, TS, S, L>(
        &'a self,
        physical_port: P,
        timer_host: TH,
        timestamping: TS,
        log: L,
        foreign_clock_records: S,
    ) -> Option<OrdinaryPortState<'a, C, P, TH, TS, L, S>>
    where
        P: PhysicalPort,
        TH: TimerHost<Timeout = T>,
        TS: TxTimestamping,
        S: ForeignClockRecords,
        L: PortLog,
    {
        let port_number = self.port_enumeration.next()?;
        let port_identity = PortIdentity::new(self.default_ds.identity(), port_number);

        // Register port with notification before creating port state
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

        Some(
            self.profile
                .initializing(domain_port, bmca, best_foreign, state_decision_trigger),
        )
    }
}

impl<C: SynchronizableClock + Clone, T: Timeout> StateDecisionEvent for OrdinaryClock<C, T> {
    fn trigger(&self, port_number: PortNumber, e_rbest: BestForeignSnapshot) {
        // Update shared state
        self.foreign_candidates.remember(port_number, e_rbest);
        let e_best = self.foreign_candidates.best();

        // Notify all ports (for ordinary clocks, this is just one port)
        self.port_notification.notify_all(StateDecisionSnapshot {
            d_0: self.default_ds,
            e_best,
        });
    }
}

struct SinglePortNotification<T: Timeout> {
    timeout: RefCell<Option<T>>,
}

impl<T: Timeout> SinglePortNotification<T> {
    fn new() -> Self {
        Self {
            timeout: RefCell::new(None),
        }
    }
}

impl<T: Timeout> PortNotification<T> for SinglePortNotification<T> {
    fn add_port(&self, timeout: T) {
        *self.timeout.borrow_mut() = Some(timeout);
    }

    fn notify_all(&self, snapshot: StateDecisionSnapshot) {
        if let Some(timeout) = self.timeout.borrow().as_ref() {
            timeout.restart_with_message(
                SystemMessage::StateDecisionEvent(snapshot),
                Duration::from_secs(0),
            );
        }
    }
}
