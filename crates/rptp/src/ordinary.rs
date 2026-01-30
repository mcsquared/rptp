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

use crate::bmca::{
    BestForeignRecord, BestForeignSnapshot, BestMasterClockAlgorithm, ClockDS, ForeignClockRecords,
    ForeignGrandMasterCandidates, StateDecisionEvent, StateDecisionEventTrigger,
};
use crate::clock::{LocalClock, SynchronizableClock};
use crate::log::PortLog;
use crate::message::SystemMessage;
use crate::port::{
    DomainNumber, DomainPort, PhysicalPort, PortBroadcast, PortEnumeration, PortNumber, Timeout,
    TimerHost,
};
use crate::portstate::PortState;
use crate::profile::PortProfile;
use crate::time::Duration;
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
/// Port creation is exhaustive: [`port`](Self::port) returns `None` once the clock's single port
/// has been handed out.
pub struct OrdinaryClock<C: SynchronizableClock, T: Timeout> {
    local_clock: LocalClock<C>,
    default_ds: ClockDS,
    domain_number: DomainNumber,
    port_enumeration: PortEnumeration,
    foreign_candidates: Cell<BestForeignSnapshot>,
    port_broadcast: SinglePortBroadcast<T>,
}

impl<C: SynchronizableClock, T: Timeout> OrdinaryClock<C, T> {
    /// Create a new ordinary clock configuration.
    ///
    /// `default_ds` is the local clock dataset used by BMCA as the local candidate (`D_0`).
    /// An ordinary clock has exactly one port (port number 1); the enumeration is fixed to
    /// [`PortEnumeration::single`] internally.
    pub fn new(
        local_clock: LocalClock<C>,
        default_ds: ClockDS,
        domain_number: DomainNumber,
    ) -> Self {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);

        OrdinaryClock {
            local_clock,
            default_ds,
            domain_number,
            port_enumeration: PortEnumeration::single(),
            foreign_candidates,
            port_broadcast: SinglePortBroadcast::new(),
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

    /// Construct the port state machine for this ordinary clock.
    ///
    /// Yields `Some(port_state)` the first time (the clock's single port) and `None` thereafter.
    /// This performs the domain assembly:
    /// - wraps infrastructure-provided boundaries into a [`DomainPort`],
    /// - constructs the BMCA evaluator ([`BestMasterClockAlgorithm`]) using the local dataset and
    ///   shared best-foreign snapshot store, and
    /// - constructs a per-port best-foreign record store ([`BestForeignRecord`]) using
    ///   `foreign_clock_records`.
    /// - registers the port with `PortBroadcast` for state decision event delivery
    ///
    /// The initial state is `INITIALIZING` as assembled by the default [`PortProfile`]. The caller
    /// is responsible for delivering `SystemMessage::Initialized` to start the state machine (see
    /// `PortState::dispatch_system`).
    pub fn port<'a, TH, TS, S, L>(
        &'a mut self,
        physical_port: &'a dyn PhysicalPort,
        timer_host: TH,
        timestamping: TS,
        log: L,
        foreign_clock_records: S,
    ) -> Option<PortState<'a, DomainPort<'a, C, TH, TS, L>, S>>
    where
        TH: TimerHost<Timeout = T>,
        TS: TxTimestamping,
        S: ForeignClockRecords,
        L: PortLog,
    {
        let port_number = self.port_enumeration.next()?;

        // Register port with broadcast before creating port state
        let state_decision_timeout = timer_host.timeout(SystemMessage::StateDecisionEvent(
            BestForeignSnapshot::Empty,
        ));
        self.port_broadcast.add_port(state_decision_timeout);

        let domain_port = DomainPort::new(
            &self.local_clock,
            physical_port,
            timer_host,
            timestamping,
            log,
            self.domain_number,
            port_number,
        );

        let default_ds = &self.default_ds;
        let bmca = BestMasterClockAlgorithm::new(default_ds, port_number);
        let best_foreign = BestForeignRecord::new(port_number, foreign_clock_records);
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

impl<C: SynchronizableClock, T: Timeout> StateDecisionEvent for OrdinaryClock<C, T> {
    fn trigger(&self, port_number: PortNumber, e_rbest: BestForeignSnapshot) {
        // Update shared state
        self.foreign_candidates.remember(port_number, e_rbest);
        let e_best = self.foreign_candidates.best();

        // Broadcast to all ports (for ordinary clocks, this is just one port)
        self.port_broadcast.broadcast(e_best);
    }
}

struct SinglePortBroadcast<T: Timeout> {
    timeout: RefCell<Option<T>>,
}

impl<T: Timeout> SinglePortBroadcast<T> {
    fn new() -> Self {
        Self {
            timeout: RefCell::new(None),
        }
    }
}

impl<T: Timeout> PortBroadcast<T> for SinglePortBroadcast<T> {
    fn add_port(&mut self, timeout: T) {
        *self.timeout.borrow_mut() = Some(timeout);
    }

    fn broadcast(&self, e_best: BestForeignSnapshot) {
        if let Some(timeout) = self.timeout.borrow().as_ref() {
            timeout.restart_with_message(
                SystemMessage::StateDecisionEvent(e_best),
                Duration::from_secs(0),
            );
        }
    }
}
