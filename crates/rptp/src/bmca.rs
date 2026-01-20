//! Best Master Clock Algorithm (BMCA) and related data sets.
//!
//! This module implements the parts of the IEEE 1588-2019 Precision Time Protocol (PTP) best-master
//! selection story that `rptp` currently needs:
//!
//! - Tracking and qualifying foreign master candidates learned from Announce messages
//! - Comparing local and foreign clock data sets (`ClockDS`) and producing high-level decisions
//! - Gating decision emission while tracking an already-selected parent/grandmaster
//!
//! The implementation is deliberately domain-oriented: the output of the algorithm is expressed as
//! a small set of domain decisions (`BmcaDecision`) that the port state machine can apply.
//!
//! ## Current limitations
//!
//! - Multi-port / boundary-clock scenarios are still evolving; some collaborators are intentionally
//!   modeled as traits so that storage and topology can be swapped without changing core logic.
//! - Some topology-only comparisons are currently simplified; TODOs in this module mark those
//!   places explicitly.

use core::cell::Cell;
use core::ops::Range;

use crate::clock::{ClockIdentity, ClockQuality, StepsRemoved, TimeScale};
use crate::message::{AnnounceMessage, SequenceId};
use crate::port::{ParentPortIdentity, Port, PortIdentity, PortNumber};
use crate::portstate::{PortState, StateDecision};
use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

/// Incremental Best Master Clock Algorithm interface.
///
/// The BMCA is driven by Announce reception: each received Announce contributes a new observation
/// about a foreign clock candidate. Implementations keep local state (e.g. qualification windows)
/// and can emit a high-level decision once enough information is available.
pub(crate) trait Bmca {
    /// Consider a new Announce-derived snapshot for a foreign clock candidate.
    ///
    /// `log_announce_interval` should be the interval announced by the sender, and is used for
    /// qualification and staleness decisions.
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    );

    /// Return the latest BMCA decision, if one should be acted upon.
    ///
    /// Returning `None` means “no new decision”: either not enough information is available yet
    /// (e.g. no qualified foreign candidates), or the decision would be identical to the currently
    /// tracked parent/grandmaster.
    fn decision(&self) -> Option<BmcaDecision>;
}

/// Storage surface for foreign clock records.
///
/// The BMCA needs to retain foreign clock candidates, update them on subsequent Announce messages,
/// qualify them, and efficiently retrieve the best qualified candidate.
///
/// This is a trait to keep the core portable across `std` and `no_std` environments, and to allow
/// different storage strategies (heap, heapless, fixed arrays, etc.).
pub trait ForeignClockRecords {
    /// Remember (insert or update) a record.
    fn remember(&mut self, record: ForeignClockRecord);
    /// Return the best *qualified* record, if any.
    fn best_qualified(&self) -> Option<&ForeignClockRecord>;
    /// Prune stale records and return `true` if any were removed.
    fn prune_stale(&mut self, now: Instant) -> bool;
}

/// Representation of a best foreign dataset observed on a port as the input to
/// BestMasterClockAlgorithm.
///
/// Maps to `E_best` and `E_rbest` in the IEEE 1588-2019 §9.3
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BestForeignDataset<'a> {
    Qualified {
        ds: &'a ClockDS,
        source_port_identity: &'a PortIdentity,
        received_on_port: PortNumber,
    },
    Empty,
}

impl<'a> BestForeignDataset<'a> {
    /// Return the grandmaster identity of this dataset, if non-empty.
    fn grandmaster_id(&self) -> Option<&ClockIdentity> {
        match self {
            BestForeignDataset::Qualified { ds, .. } => Some(ds.identity()),
            BestForeignDataset::Empty => None,
        }
    }

    /// Return `true` if `self` is better than `other` according to BMCA dataset comparison.
    fn better_than(&self, other: &BestForeignDataset) -> bool {
        match (self, other) {
            (
                BestForeignDataset::Qualified { ds: ds1, .. },
                BestForeignDataset::Qualified { ds: ds2, .. },
            ) => ds1.better_than(ds2),
            (BestForeignDataset::Qualified { .. }, BestForeignDataset::Empty) => true,
            (BestForeignDataset::Empty, BestForeignDataset::Qualified { .. }) => false,
            (BestForeignDataset::Empty, BestForeignDataset::Empty) => false,
        }
    }

    // Return the parent port identity if this dataset was received on the given port.
    fn parent_if_received_on_port(self, port: PortNumber) -> Option<ParentPortIdentity> {
        match self {
            BestForeignDataset::Qualified {
                source_port_identity,
                received_on_port,
                ..
            } => (received_on_port == port).then(|| ParentPortIdentity::new(*source_port_identity)),
            BestForeignDataset::Empty => None,
        }
    }

    // Convert to a snapshot owning the dataset.
    fn snapshot(self) -> BestForeignSnapshot {
        match self {
            BestForeignDataset::Qualified {
                ds,
                source_port_identity,
                received_on_port,
            } => BestForeignSnapshot::Qualified {
                ds: *ds,
                source_port_identity: *source_port_identity,
                received_on_port,
            },
            BestForeignDataset::Empty => BestForeignSnapshot::Empty,
        }
    }
}

pub struct BestForeignRecord<S: ForeignClockRecords> {
    received_on_port: PortNumber,
    foreign_clock_records: S,
}

impl<S: ForeignClockRecords> BestForeignRecord<S> {
    pub fn new(received_on_port: PortNumber, foreign_clock_records: S) -> Self {
        Self {
            received_on_port,
            foreign_clock_records,
        }
    }

    /// Consider a new foreign clock record observation.
    pub(crate) fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.foreign_clock_records.prune_stale(now);
        self.foreign_clock_records.remember(ForeignClockRecord::new(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        ));
    }

    /// Return the best qualified foreign dataset observed so far.
    fn dataset<'a>(&'a self) -> BestForeignDataset<'a> {
        match self.foreign_clock_records.best_qualified() {
            Some(record) => BestForeignDataset::Qualified {
                ds: record
                    .qualified_ds()
                    .expect("best_qualified must return a qualified record"),
                source_port_identity: record.source_port_identity(),
                received_on_port: self.received_on_port,
            },
            None => BestForeignDataset::Empty,
        }
    }
}

/// ListeningBmca
///
/// BMCA role decorator, that encodes listening mode behavior/gating of decisions.
///
/// Represents the first diamond in the BMCA decision flow chart,
/// see IEEE 1588-2019 §9.3.3; Fig. 33.
pub(crate) struct ListeningBmca<'a, S: ForeignClockRecords> {
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
}

impl<'a, S: ForeignClockRecords> ListeningBmca<'a, S> {
    pub fn new(bmca: BestMasterClockAlgorithm<'a>, best_foreign: BestForeignRecord<S>) -> Self {
        Self { bmca, best_foreign }
    }

    /// Convert to a ParentTrackingBmca that tracks the given parent.
    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(self.bmca, self.best_foreign, parent_port_identity)
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given grandmaster.
    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(self.bmca, self.best_foreign, grandmaster_id)
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the currently known grandmaster.
    pub(crate) fn into_current_grandmaster_tracking(self) -> GrandMasterTrackingBmca<'a, S> {
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| *gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }

    /// Convert to a PassiveBmca for transitioning to passive state.
    pub(crate) fn into_passive(self) -> PassiveBmca<'a, S> {
        PassiveBmca::new(self.bmca, self.best_foreign)
    }

    /// Decompose ListeningBmca into its parts.
    pub(crate) fn into_parts(self) -> (BestMasterClockAlgorithm<'a>, BestForeignRecord<S>) {
        (self.bmca, self.best_foreign)
    }
}

impl<'a, S: ForeignClockRecords> Bmca for ListeningBmca<'a, S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.best_foreign.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    /// Return a decision in listening mode only if there is a qualified foreign dataset, as in
    /// IEEE 1588-2019 §9.3.3; Fig. 33 - first flow chart diamond.
    fn decision(&self) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.dataset();

        match e_rbest {
            BestForeignDataset::Qualified { .. } => Some(self.bmca.decision(e_rbest)),
            BestForeignDataset::Empty => None,
        }
    }
}

/// A view of the currently selected GrandMaster.
///
/// It's the announce surface that hides whether the current grandmaster is local or foreign.
/// MasterPort uses this to generate Announce messages.
pub(crate) struct GrandMaster<'a> {
    ds: &'a ClockDS,
}

impl<'a> GrandMaster<'a> {
    fn new(ds: &'a ClockDS) -> Self {
        Self { ds }
    }

    pub fn identity(&self) -> &ClockIdentity {
        self.ds.identity()
    }

    pub fn announce(
        &self,
        sequence_id: SequenceId,
        log_message_interval: LogMessageInterval,
        time_scale: TimeScale,
    ) -> AnnounceMessage {
        AnnounceMessage::new(sequence_id, log_message_interval, *self.ds, time_scale)
    }
}

/// BMCA role decorator that suppresses “master” decisions while tracking a grandmaster.
///
/// If the underlying BMCA would decide to be master *for the same grandmaster*, this wrapper
/// returns `None` from `decision()` to avoid repeatedly re-emitting identical transitions.
/// If the underlying BMCA selects a different grandmaster (or decides slave/passive), the
/// decision is emitted.
pub(crate) struct GrandMasterTrackingBmca<'a, S: ForeignClockRecords> {
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
    grandmaster_id: ClockIdentity,
}

impl<'a, S: ForeignClockRecords> GrandMasterTrackingBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        grandmaster_id: ClockIdentity,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            grandmaster_id,
        }
    }

    /// Access the currently tracked grandmaster via the given closure.
    pub(crate) fn using_grandmaster<R>(&self, f: impl FnOnce(GrandMaster<'_>) -> R) -> R {
        self.bmca.using_grandmaster(f)
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given new grandmaster.
    pub(crate) fn with_grandmaster_id(self, grandmaster_id: ClockIdentity) -> Self {
        Self {
            bmca: self.bmca,
            best_foreign: self.best_foreign,
            grandmaster_id,
        }
    }

    /// Convert to a ParentTrackingBmca that tracks the given parent.
    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(self.bmca, self.best_foreign, parent_port_identity)
    }

    /// Convert to a PassiveBmca for transitioning to passive state.
    pub(crate) fn into_passive(self) -> PassiveBmca<'a, S> {
        PassiveBmca::new(self.bmca, self.best_foreign)
    }

    /// Decompose GrandMasterTrackingBmca into its parts.
    pub(crate) fn into_parts(self) -> (BestMasterClockAlgorithm<'a>, BestForeignRecord<S>) {
        (self.bmca, self.best_foreign)
    }
}

impl<'a, S: ForeignClockRecords> Bmca for GrandMasterTrackingBmca<'a, S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.best_foreign.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    /// Return a decision while suppressing identical master decisions.
    fn decision(&self) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.dataset();
        let decision = self.bmca.decision(e_rbest);

        match decision {
            BmcaDecision::Master(master_decision) => {
                if master_decision.grandmaster_id() == self.grandmaster_id {
                    None
                } else {
                    Some(decision)
                }
            }
            BmcaDecision::Slave(_) | BmcaDecision::Passive => Some(decision),
        }
    }
}

/// ParentTrackingBmca
///
/// BMCA role decorator that suppresses “slave” decisions while tracking a parent.
///
/// If the underlying BMCA would decide to be slave of the same `parent_port_identity`, this
/// wrapper returns `None` from `decision()` to avoid repeatedly re-emitting identical
/// transitions. If a different parent is selected, the decision is emitted.
///
/// Encodes the transition gating (new_master != old_master) in IEEE 1588-2019 §9.2.5; Fig. 30 & 31.
pub(crate) struct ParentTrackingBmca<'a, S: ForeignClockRecords> {
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
    parent_port_identity: ParentPortIdentity,
}

impl<'a, S: ForeignClockRecords> ParentTrackingBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        parent_port_identity: ParentPortIdentity,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            parent_port_identity,
        }
    }

    /// Return `true` if the given source matches the currently tracked parent. This is useful
    /// for filtering sync, follow-up & delay response messages.
    pub(crate) fn matches_parent(&self, source: &PortIdentity) -> bool {
        self.parent_port_identity.matches(source)
    }

    /// Convert to a ParentTrackingBmca that tracks the given new parent.
    pub(crate) fn with_parent(self, parent: ParentPortIdentity) -> Self {
        Self {
            bmca: self.bmca,
            best_foreign: self.best_foreign,
            parent_port_identity: parent,
        }
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given grandmaster.
    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(self.bmca, self.best_foreign, grandmaster_id)
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the currently known grandmaster.
    pub(crate) fn into_current_grandmaster_tracking(self) -> GrandMasterTrackingBmca<'a, S> {
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| *gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }

    /// Convert to a PassiveBmca for transitioning to passive state.
    pub(crate) fn into_passive(self) -> PassiveBmca<'a, S> {
        PassiveBmca::new(self.bmca, self.best_foreign)
    }

    /// Decompose ParentTrackingBmca into its parts.
    pub(crate) fn into_parts(self) -> (BestMasterClockAlgorithm<'a>, BestForeignRecord<S>) {
        (self.bmca, self.best_foreign)
    }
}

impl<'a, S: ForeignClockRecords> Bmca for ParentTrackingBmca<'a, S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.best_foreign.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    /// Return a decision while suppressing identical decisions with the same parent.
    fn decision(&self) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.dataset();
        let decision = self.bmca.decision(e_rbest);

        match decision {
            BmcaDecision::Slave(parent) => {
                if self.parent_port_identity != parent {
                    Some(decision)
                } else {
                    None
                }
            }
            _ => Some(decision),
        }
    }
}

/// PassiveBmca
///
/// BMCA role decorator that suppresses "passive" decisions while in passive state.
///
/// If the underlying BMCA would decide to be passive, this wrapper returns `None` from
/// `decision()` to avoid repeatedly re-emitting identical transitions. Master and slave
/// decisions are always emitted to allow transitions away from passive state.
///
/// Encodes the transition gating for passive state in IEEE 1588-2019 §9.2.5.
pub(crate) struct PassiveBmca<'a, S: ForeignClockRecords> {
    bmca: BestMasterClockAlgorithm<'a>,
    best_foreign: BestForeignRecord<S>,
}

impl<'a, S: ForeignClockRecords> PassiveBmca<'a, S> {
    pub fn new(bmca: BestMasterClockAlgorithm<'a>, best_foreign: BestForeignRecord<S>) -> Self {
        Self { bmca, best_foreign }
    }

    /// Convert to a ParentTrackingBmca that tracks the given parent.
    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(self.bmca, self.best_foreign, parent_port_identity)
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given grandmaster.
    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(self.bmca, self.best_foreign, grandmaster_id)
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the currently known grandmaster.
    pub(crate) fn into_current_grandmaster_tracking(self) -> GrandMasterTrackingBmca<'a, S> {
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| *gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }

    /// Decompose PassiveBmca into its parts.
    pub(crate) fn into_parts(self) -> (BestMasterClockAlgorithm<'a>, BestForeignRecord<S>) {
        (self.bmca, self.best_foreign)
    }
}

impl<'a, S: ForeignClockRecords> Bmca for PassiveBmca<'a, S> {
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) {
        self.best_foreign.consider(
            source_port_identity,
            foreign_clock_ds,
            log_announce_interval,
            now,
        );
    }

    /// Return a decision while suppressing passive decisions.
    ///
    /// Suppresses `BmcaDecision::Passive` to avoid repeatedly re-emitting identical transitions
    /// while in passive state. Master and slave decisions are always emitted to allow transitions
    /// away from passive state.
    fn decision(&self) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.dataset();
        let decision = self.bmca.decision(e_rbest);

        match decision {
            BmcaDecision::Passive => None,
            _ => Some(decision),
        }
    }
}

/// Snapshot of best foreign datasets.
///
/// This is the dataset-owning counterpart of BestForeignDataset. BestForeignDataset is to present
/// momentary dataset views to the BMCA, while BestForeignSnapshot is to store and retrieve them
/// as needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BestForeignSnapshot {
    /// A qualified foreign candidate and the identity of the port that announced it.
    Qualified {
        ds: ClockDS,
        source_port_identity: PortIdentity,
        received_on_port: PortNumber,
    },
    /// No qualified foreign candidate is currently known. The empty dataset in spec language.
    Empty,
}

impl BestForeignSnapshot {
    /// Convert to a BestForeignDataset for BMCA consumption.
    fn as_best_foreign_dataset(&self) -> BestForeignDataset<'_> {
        match self {
            BestForeignSnapshot::Qualified {
                ds,
                source_port_identity,
                received_on_port,
            } => BestForeignDataset::Qualified {
                ds,
                source_port_identity,
                received_on_port: *received_on_port,
            },
            BestForeignSnapshot::Empty => BestForeignDataset::Empty,
        }
    }
}

/// Foreign grandmaster candidates storage surface to present the cross-port shared best
/// foreign dataset.
///
/// Infrastructure implementations own storage strategy, locking, etc.
pub trait ForeignGrandMasterCandidates {
    /// Remember or update what a port reports (qualified candidate or empty).
    ///
    /// `port` identifies which port is reporting. For `Empty` snapshots, this signals
    /// that the port has no qualified candidates and allows the store to update its
    /// per-port tracking accordingly.
    fn remember(&self, port: PortNumber, snapshot: BestForeignSnapshot);

    /// Return the best qualified foreign candidate across all ports.
    fn best(&self) -> BestForeignSnapshot;
}

// Simple in-memory implementation of ForeignGrandMasterCandidates for single-port use cases.
impl ForeignGrandMasterCandidates for Cell<BestForeignSnapshot> {
    fn remember(&self, port: PortNumber, snapshot: BestForeignSnapshot) {
        debug_assert!(
            port == PortNumber::new(1),
            "only single-port use cases supported"
        );
        debug_assert!(
            match snapshot {
                BestForeignSnapshot::Qualified {
                    received_on_port, ..
                } => received_on_port == PortNumber::new(1),
                BestForeignSnapshot::Empty => true,
            },
            "only single-port use cases supported"
        );
        self.set(snapshot);
    }

    fn best(&self) -> BestForeignSnapshot {
        self.get()
    }
}

/// Local grandmaster dataset storage surface to present the local clock's data set shared
/// across ports.
///
/// Infrastructure implementations own storage strategy, locking, etc.
pub trait LocalGrandMasterCandidate {
    fn snapshot(&self) -> ClockDS;
}

// Simple in-memory implementation of LocalGrandMasterCandidate for single-port use cases.
impl LocalGrandMasterCandidate for ClockDS {
    fn snapshot(&self) -> ClockDS {
        *self
    }
}

/// Best Master Clock Algorithm (BMCA).
///
/// Implements the core best master clock decision algorithm as per IEEE 1588-2019 §9.3.3, Fig. 33,
/// excluding the first listening-mode diamond, which is extracted into `ListeningBmca`, to keep the
/// core logic clean and simple.
///
/// Each BMCA instance is identified by the port number it serves, to allow multi-port scenarios.
/// Clock-level shared storage surfaces for local and foreign candidates are injected via trait
/// objects which represent the cross-port shared state (`D_0` and `E_best` in spec).
///
/// ## Known limitations:
///  - M3 topology-only comparison is not yet implemented; see TODO in
///    `e_best_better_by_topology_than_e_rbest`.
pub(crate) struct BestMasterClockAlgorithm<'a> {
    local_candidate: &'a dyn LocalGrandMasterCandidate,
    foreign_candidates: &'a dyn ForeignGrandMasterCandidates,
    port_number: PortNumber,
}

impl<'a> BestMasterClockAlgorithm<'a> {
    /// Create a new BMCA evaluator for `port_number`.
    ///
    /// `local_candidate` supplies the local data set snapshot (`D_0`).
    /// `foreign_candidates` aggregates best foreign snapshots across ports. (`E_best`)
    pub fn new(
        local_candidate: &'a dyn LocalGrandMasterCandidate,
        foreign_candidates: &'a dyn ForeignGrandMasterCandidates,
        port_number: PortNumber,
    ) -> Self {
        Self {
            local_candidate,
            foreign_candidates,
            port_number,
        }
    }

    /// Access the currently known grandmaster via the given closure.
    pub(crate) fn using_grandmaster<R>(&self, f: impl FnOnce(GrandMaster<'_>) -> R) -> R {
        // TODO: once multi-port is supported, this should return the grandmaster, whether local or
        // foreign. With only single port support atm, it's always local d_0.
        let d_0 = self.local_candidate.snapshot();
        f(GrandMaster::new(&d_0))
    }

    /// Evaluate the BMCA decision for the current inputs.
    ///
    /// The argument `e_rbest` is the best qualified foreign candidate observed on the current
    /// port (if any).
    pub(crate) fn decision(&self, e_rbest: BestForeignDataset) -> BmcaDecision {
        self.foreign_candidates
            .remember(self.port_number, e_rbest.snapshot());

        let d_0 = self.local_candidate.snapshot();
        if d_0.is_authoritative() {
            Self::d0_better_or_better_by_topology_than_e_rbest(&d_0, e_rbest)
        } else {
            let best_snapshot = self.foreign_candidates.best();
            let e_best = best_snapshot.as_best_foreign_dataset();
            self.d0_better_or_better_by_topology_than_e_best(&d_0, e_best, e_rbest)
        }
    }

    fn d0_better_or_better_by_topology_than_e_rbest(
        d_0: &ClockDS,
        e_rbest: BestForeignDataset,
    ) -> BmcaDecision {
        if d_0.better_than_foreign(e_rbest) {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0), // IEEE 1588-2019 §3.9.5; Tbl. 30 (stepsRemoved=0 when acting as GM)
                *d_0.identity(),
            ))
        } else {
            BmcaDecision::Passive // Passive decision point P1
        }
    }

    fn d0_better_or_better_by_topology_than_e_best(
        &self,
        d_0: &ClockDS,
        e_best: BestForeignDataset,
        e_rbest: BestForeignDataset,
    ) -> BmcaDecision {
        if d_0.better_than_foreign(e_best) {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0), // IEEE 1588-2019 §3.9.5; Tbl. 30 (stepsRemoved=0 when acting as master)
                *d_0.identity(),
            ))
        } else if let Some(parent) = e_best.parent_if_received_on_port(self.port_number) {
            BmcaDecision::Slave(parent) // Slave decision point S1
        } else {
            // Only a qualified e_best can reach this point (IEEE 1588-2019 §9.3.3; Fig. 33).
            debug_assert!(matches!(e_best, BestForeignDataset::Qualified { .. }));
            Self::e_best_better_by_topology_than_e_rbest(e_best, e_rbest)
        }
    }

    fn e_best_better_by_topology_than_e_rbest(
        e_best: BestForeignDataset,
        e_rbest: BestForeignDataset,
    ) -> BmcaDecision {
        // TODO: this comparison shall be topology-only, as the method name suggests.
        // See IEEE 1588-2019 §9.3.3; Fig. 33, diamond for decision point P2 vs M3.
        if e_best.better_than(&e_rbest) {
            BmcaDecision::Passive // Passive decision point P2
        } else {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M3,
                StepsRemoved::new(0), // TODO: use currently used steps removed field here
                // unwrap is safe here, because only a qualified e_best can win against d_0, see above.
                e_best.grandmaster_id().copied().unwrap(),
            ))
        }
    }
}

/// A BMCA implementation that never produces decisions.
///
/// Useful as a placeholder in early experiments, testing or partially wired port states.
#[allow(dead_code)]
pub(crate) struct NoopBmca;

impl Bmca for NoopBmca {
    fn consider(
        &mut self,
        _source_port_identity: crate::port::PortIdentity,
        _foreign_clock_ds: ClockDS,
        _log_announce_interval: LogInterval,
        _now: Instant,
    ) {
    }

    fn decision(&self) -> Option<BmcaDecision> {
        None
    }
}

/// Master decision points used by the BMCA as defined in IEEE 1588-2019.
///
/// See:
/// - IEEE 1588-2019 §9.3.1
/// - IEEE 1588-2019 §9.3.3; Fig. 33
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BmcaMasterDecisionPoint {
    M1,
    M2,
    M3,
}

/// High‑level BMCA outcome for a given port.
///
/// This is the “so what?” result that the port state machine can act upon, matching the state
/// transition annotations in IEEE 1588-2019 §9.2.5; Fig. 30 & 31.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BmcaDecision {
    /// The port should act as a master. (BMC_MASTER as to IEEE 1588-2019)
    Master(BmcaMasterDecision),
    /// The port should act as a slave, following the given parent. (BMC_SLAVE as to IEEE 1588-2019)
    Slave(ParentPortIdentity),
    /// The port should be passive. (BMC_PASSIVE as to IEEE 1588-2019)
    Passive,
}

impl BmcaDecision {
    pub(crate) fn to_state_decision(self) -> Option<StateDecision> {
        match self {
            BmcaDecision::Master(decision) => Some(StateDecision::RecommendedMaster(decision)),
            BmcaDecision::Slave(parent) => Some(StateDecision::RecommendedSlave(parent)),
            BmcaDecision::Passive => Some(StateDecision::RecommendedPassive),
        }
    }
}

/// BMCA decision for entering a master/pre‑master state.
///
/// The master decision includes:
/// - the decision point (useful for qualification timing rules),
/// - the `stepsRemoved` value to apply, and
/// - the selected grandmaster identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BmcaMasterDecision {
    decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
    grandmaster_id: ClockIdentity,
}

impl BmcaMasterDecision {
    /// Create a new master decision for the given decision point and
    /// `stepsRemoved`.
    pub(crate) fn new(
        decision_point: BmcaMasterDecisionPoint,
        steps_removed: StepsRemoved,
        grandmaster_id: ClockIdentity,
    ) -> Self {
        Self {
            decision_point,
            steps_removed,
            grandmaster_id,
        }
    }

    pub fn grandmaster_id(&self) -> ClockIdentity {
        self.grandmaster_id
    }

    /// Apply this master decision to a [`Port`], producing a new [`PortState`] using the given
    /// `new_port_state` constructor closure that caller provide to choose the new state.
    pub(crate) fn apply<'a, F, P, S>(&self, new_port_state: F) -> PortState<'a, P, S>
    where
        P: Port,
        S: ForeignClockRecords,
        F: FnOnce(QualificationTimeoutPolicy, ClockIdentity) -> PortState<'a, P, S>,
    {
        let qualification_timeout_policy =
            QualificationTimeoutPolicy::new(self.decision_point, self.steps_removed);

        new_port_state(qualification_timeout_policy, self.grandmaster_id)
    }
}

/// Qualification timeout policy.
///
/// Policy for determining the master qualification timeout interval to be used when
/// entering pre-master state as per IEEE 1588-2019 §9.2.6.11.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QualificationTimeoutPolicy {
    master_decision_point: BmcaMasterDecisionPoint,
    steps_removed: StepsRemoved,
}

impl QualificationTimeoutPolicy {
    pub fn new(
        master_decision_point: BmcaMasterDecisionPoint,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            master_decision_point,
            steps_removed,
        }
    }

    /// Return the qualification timeout duration based on the given `log_announce_interval`.
    pub fn duration(&self, log_announce_interval: LogInterval) -> Duration {
        match self.master_decision_point {
            BmcaMasterDecisionPoint::M1 => Duration::from_secs(0),
            BmcaMasterDecisionPoint::M2 => Duration::from_secs(0),
            BmcaMasterDecisionPoint::M3 => {
                let n = self.steps_removed.as_u16() as u64 + 1;
                log_announce_interval.duration().saturating_mul(n)
            }
        }
    }
}

/// A single foreign clock record tracked by BestForeignRecord and stored in
/// `ForeignClockRecords`. It represents a foreign master clock candidate discovered via
/// Announce messages along with its source port identity and qualification state.
///
/// A record becomes *qualified* once enough Announce messages have been received within the
/// `ForeignMasterTimeWindow`. Until qualified, the record is retained but not considered for
/// `best_qualified` selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignClockRecord {
    source_port_identity: PortIdentity,
    foreign_clock_ds: ClockDS,
    qualification: SlidingWindowQualification,
}

impl ForeignClockRecord {
    pub(crate) fn new(
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) -> Self {
        let window = ForeignMasterTimeWindow::new(log_announce_interval).duration();

        Self {
            source_port_identity,
            foreign_clock_ds,
            qualification: SlidingWindowQualification::new_unqualified(now, window),
        }
    }

    /// Create a record for a new foreign clock that is already qualified. This is mainly
    /// useful for tests, where we want to set up qualified foreign clock records directly.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn qualified(
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    ) -> Self {
        let window = ForeignMasterTimeWindow::new(log_announce_interval).duration();

        Self {
            source_port_identity,
            foreign_clock_ds,
            qualification: SlidingWindowQualification {
                last: now,
                prev: Some(now),
                window,
            },
        }
    }

    /// Return `true` if this record originates from `source_port_identity`.
    pub(crate) fn same_source_as(&self, source_port_identity: &PortIdentity) -> bool {
        self.source_port_identity == *source_port_identity
    }

    /// Return `true` if this record is better than `other` purely based on the data sets,
    /// ignoring qualification status.
    pub(crate) fn better_by_dataset_than(&self, other: &ForeignClockRecord) -> bool {
        self.foreign_clock_ds.better_than(&other.foreign_clock_ds)
    }

    /// Update this record from a newly observed record of the same source.
    ///
    /// This potentially:
    /// - advances the qualification window,
    /// - changes qualification status, and/or
    /// - updates the underlying data set.
    pub(crate) fn update_from(&mut self, record: &ForeignClockRecord) -> ForeignClockStatus {
        debug_assert!(self.source_port_identity == record.source_port_identity);

        let was_qualified = self.qualification.is_qualified();
        self.qualification
            .qualify(record.qualification.last, record.qualification.window);
        let is_qualified = self.qualification.is_qualified();

        let data_set_changed = self.foreign_clock_ds != record.foreign_clock_ds;
        if data_set_changed {
            self.foreign_clock_ds = record.foreign_clock_ds;
        }

        if was_qualified != is_qualified || (is_qualified && data_set_changed) {
            ForeignClockStatus::Updated
        } else {
            ForeignClockStatus::Unchanged
        }
    }

    /// Return the underlying data set if this record has become qualified as
    /// a foreign master candidate, or `None` otherwise.
    pub(crate) fn qualified_ds(&self) -> Option<&ClockDS> {
        if self.qualification.is_qualified() {
            Some(&self.foreign_clock_ds)
        } else {
            None
        }
    }

    /// Return the source port identity that originated this record.
    pub(crate) fn source_port_identity(&self) -> &PortIdentity {
        &self.source_port_identity
    }

    /// Return `true` if this record is stale at the given time.
    pub(crate) fn is_stale(&self, now: Instant) -> bool {
        self.qualification.is_stale(now)
    }
}

impl Ord for ForeignClockRecord {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        use core::cmp::Ordering::*;

        let ord = match (
            self.qualification.is_qualified(),
            other.qualification.is_qualified(),
        ) {
            (true, true) => {
                if self.foreign_clock_ds.better_than(&other.foreign_clock_ds) {
                    Less
                } else if other.foreign_clock_ds.better_than(&self.foreign_clock_ds) {
                    Greater
                } else {
                    Equal
                }
            }
            (true, false) => Less,
            (false, true) => Greater,
            (false, false) => Equal,
        };

        if ord == Equal {
            self.source_port_identity.cmp(&other.source_port_identity)
        } else {
            ord
        }
    }
}

impl PartialOrd for ForeignClockRecord {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Result of considering a foreign clock record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignClockStatus {
    /// The record remained unqualified or unchanged.
    Unchanged,
    /// The record became newly qualified or its data set changed.
    Updated,
}

/// The `ForeignMasterTimeWindow` derived from a port's announce interval.
///
/// The window defines how tightly spaced Announce messages must be to qualify a foreign record and
/// how long a record can remain silent before being considered stale.
///
/// See IEEE 1588-2019 §9.3.2.4.5
struct ForeignMasterTimeWindow {
    log_announce_interval: LogInterval,
}

impl ForeignMasterTimeWindow {
    fn new(log_announce_interval: LogInterval) -> Self {
        Self {
            log_announce_interval,
        }
    }

    fn duration(&self) -> Duration {
        self.log_announce_interval.duration().saturating_mul(4)
    }
}

/// Sliding window qualification policy for foreign master records. Encodes the logic to only
/// consider an observed foreign clock as qualified once enough Announce messages have been received
/// within a given time window.
///
/// This is intentionally a small and self-contained state machine that can later be replaced by a
/// different strategy (e.g. higher thresholds) without rippling into the rest of the module.
///
/// Notes
/// - Current behavior assumes a threshold of `2` Announce messages within the window. As defined in
///   IEEE 1588-2019 §9.3.2.4.5 (FOREIGN_MASTER_THRESHOLD = 2).
///
/// See
/// - IEEE 1588-2019 §9.3.2.4.5
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlidingWindowQualification {
    last: Instant,
    prev: Option<Instant>,
    window: Duration,
}

impl SlidingWindowQualification {
    fn new_unqualified(first: Instant, window: Duration) -> Self {
        Self {
            last: first,
            prev: None,
            window,
        }
    }

    /// Try to qualify this record based on a newly observed announce at `now`.
    fn qualify(&mut self, now: Instant, window: Duration) {
        self.prev = Some(self.last);
        self.last = now;
        self.window = window;
    }

    /// Return `true` if this record is currently qualified.
    fn is_qualified(&self) -> bool {
        let delta = match self.prev {
            Some(prev) => self.last.checked_sub(prev),
            None => None,
        };

        delta.is_some_and(|delta| delta <= self.window)
    }

    /// Return `true` if this record is stale at the given time.
    fn is_stale(&self, now: Instant) -> bool {
        let delta = now.checked_sub(self.last);
        delta.is_none_or(|delta| delta > self.window)
    }
}

/// Data set describing a grandmaster candidate.
///
/// This corresponds closely to the fields carried in Announce messages and
/// used by the BMCA when comparing local and foreign clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockDS {
    identity: ClockIdentity,
    priority1: Priority1,
    priority2: Priority2,
    quality: ClockQuality,
    steps_removed: StepsRemoved,
}

impl ClockDS {
    const PRIORITY1_OFFSET: usize = 0;
    const QUALITY_RANGE: Range<usize> = 1..5;
    const PRIORITY2_OFFSET: usize = 5;
    const IDENTITY_RANGE: Range<usize> = 6..14;
    const STEPS_REMOVED_OFFSET: Range<usize> = 14..16;

    pub fn new(
        identity: ClockIdentity,
        priority1: Priority1,
        priority2: Priority2,
        quality: ClockQuality,
        steps_removed: StepsRemoved,
    ) -> Self {
        Self {
            identity,
            priority1,
            priority2,
            quality,
            steps_removed,
        }
    }

    /// Return the clock identity of this dataset.
    pub fn identity(&self) -> &ClockIdentity {
        &self.identity
    }

    /// Return `true` if this dataset represents an authoritative clock.
    ///
    /// A clock is authoritative when its clock class is 1 to 127 inclusive, as denoted in
    /// the spec multiple times, but not named explicitly.
    ///
    /// See
    /// - IEEE 1588-2019 §9.3.3; Fig. 33
    pub(crate) fn is_authoritative(&self) -> bool {
        self.quality.is_authoritative()
    }

    /// Parse a `ClockDS` from the binary representation used on the
    /// wire (16‑byte BMCA comparison tuple).
    pub(crate) fn from_wire(buf: &[u8; 16]) -> Self {
        Self {
            identity: ClockIdentity::new(&[
                buf[Self::IDENTITY_RANGE.start],
                buf[Self::IDENTITY_RANGE.start + 1],
                buf[Self::IDENTITY_RANGE.start + 2],
                buf[Self::IDENTITY_RANGE.start + 3],
                buf[Self::IDENTITY_RANGE.start + 4],
                buf[Self::IDENTITY_RANGE.start + 5],
                buf[Self::IDENTITY_RANGE.start + 6],
                buf[Self::IDENTITY_RANGE.start + 7],
            ]),
            priority1: Priority1::new(buf[ClockDS::PRIORITY1_OFFSET]),
            priority2: Priority2::new(buf[ClockDS::PRIORITY2_OFFSET]),
            quality: ClockQuality::from_wire(&[
                buf[ClockDS::QUALITY_RANGE.start],
                buf[ClockDS::QUALITY_RANGE.start + 1],
                buf[ClockDS::QUALITY_RANGE.start + 2],
                buf[ClockDS::QUALITY_RANGE.start + 3],
            ]),
            steps_removed: StepsRemoved::new(u16::from_be_bytes([
                buf[ClockDS::STEPS_REMOVED_OFFSET.start],
                buf[ClockDS::STEPS_REMOVED_OFFSET.start + 1],
            ])),
        }
    }

    /// Return `true` if this dataset is strictly better than `other`
    /// according to BMCA ranking rules.
    pub(crate) fn better_than(&self, other: &ClockDS) -> bool {
        if self.identity == other.identity {
            return self.steps_removed < other.steps_removed;
        }

        let a = (
            &self.priority1,
            &self.quality,
            &self.priority2,
            &self.identity,
        );
        let b = (
            &other.priority1,
            &other.quality,
            &other.priority2,
            &other.identity,
        );

        a < b
    }

    pub(crate) fn better_than_foreign(&self, other: BestForeignDataset) -> bool {
        match other {
            BestForeignDataset::Qualified { ds: other_ds, .. } => self.better_than(other_ds),
            BestForeignDataset::Empty => true,
        }
    }

    /// Serialize this data set into the 16‑byte BMCA comparison tuple
    /// representation.
    pub(crate) fn to_wire(self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[Self::PRIORITY1_OFFSET] = self.priority1.as_u8();
        bytes[Self::QUALITY_RANGE].copy_from_slice(&self.quality.to_wire());
        bytes[Self::PRIORITY2_OFFSET] = self.priority2.as_u8();
        bytes[Self::IDENTITY_RANGE].copy_from_slice(self.identity.as_bytes());
        bytes[Self::STEPS_REMOVED_OFFSET.start..Self::STEPS_REMOVED_OFFSET.end]
            .copy_from_slice(&self.steps_removed.to_be_bytes());
        bytes
    }
}

/// `defaultDS.priority1` as defined in IEEE 1588-2019.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority1(u8);

impl Priority1 {
    /// Create a new `Priority1` from a raw `u8` value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Return the underlying `u8` value.
    pub(crate) fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority1 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

/// `defaultDS.priority2` as defined in IEEE 1588-2019.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority2(u8);

impl Priority2 {
    /// Create a new `Priority2` from a raw `u8` value.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Return the underlying `u8` value.
    pub(crate) fn as_u8(self) -> u8 {
        self.0
    }
}

impl From<u8> for Priority2 {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::clock::LocalClock;
    use crate::infra::infra_support::{ForeignClockRecordsVec, MultiPortForeignCandidates};
    use crate::log::NOOP_CLOCK_METRICS;
    use crate::port::PortNumber;
    use crate::servo::{Servo, SteppingServo};
    use crate::test_support::{FakeClock, TestClockDS};
    use crate::time::{Duration, Instant};

    #[test]
    fn grandmaster_tracking_bmca_gates_when_grandmaster_id_matches() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_high_grade().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
            *local_clock.identity(),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn grandmaster_tracking_bmca_emits_when_grandmaster_id_differs() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_high_grade().dataset();

        let other_grandmaster_id = TestClockDS::gps_grandmaster().clock_identity();
        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
            other_grandmaster_id,
        );

        assert!(matches!(bmca.decision(), Some(BmcaDecision::Master(_))));
    }

    #[test]
    fn grandmaster_tracking_bmca_does_not_gate_passive_decisions() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let now = Instant::from_secs(0);

        let default_ds = TestClockDS::gps_grandmaster().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let better = TestClockDS::atomic_grandmaster();
        let better_port_id = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        let better_ds = better.dataset();
        let records = [ForeignClockRecord::qualified(
            better_port_id,
            better_ds,
            LogInterval::new(0),
            now,
        )];

        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(
                PortNumber::new(1),
                ForeignClockRecordsVec::from_records(&records),
            ),
            *local_clock.identity(),
        );

        assert_eq!(bmca.decision(), Some(BmcaDecision::Passive));
    }

    #[test]
    fn grandmaster_tracking_bmca_does_not_gate_slave_decisions() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let now = Instant::from_secs(0);

        let default_ds = TestClockDS::default_low_grade_slave_only().dataset();
        let local_clock = LocalClock::new(
            FakeClock::default(),
            *default_ds.identity(),
            Servo::Stepping(SteppingServo::new(&NOOP_CLOCK_METRICS)),
        );

        let better = TestClockDS::default_high_grade();
        let better_port_id = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        let better_ds = better.dataset();
        let records = [ForeignClockRecord::qualified(
            better_port_id,
            better_ds,
            LogInterval::new(0),
            now,
        )];

        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(
                PortNumber::new(1),
                ForeignClockRecordsVec::from_records(&records),
            ),
            *local_clock.identity(),
        );

        assert!(matches!(bmca.decision(), Some(BmcaDecision::Slave(_))));
    }

    // Test for IEEE 1588-2019 Section 9.3.3, figure 33, Slave decision point S1.
    //
    // This test verifies that when E_best was received on a different port, the BMCA does not
    // recommend SLAVE (S1 decision point). Instead, it should recommend PASSIVE or MASTER (P2/M3).
    #[test]
    fn bmca_does_not_choose_slave_when_ebest_received_on_other_port() {
        let foreign_candidates = MultiPortForeignCandidates::new();

        let local_port = PortNumber::new(1);
        let other_port = PortNumber::new(2);

        let default_ds = TestClockDS::default_low_grade_slave_only().dataset();
        debug_assert!(
            !default_ds.is_authoritative(),
            "test expects non-authoritative local clock behavior"
        );

        // Seed Ebest on another local receive port. It must remain best after Erbest is remembered.
        let better = TestClockDS::default_high_grade().with_priority1(Priority1::new(1));
        let e_best_parent = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        foreign_candidates.remember(
            other_port,
            BestForeignSnapshot::Qualified {
                ds: better.dataset(),
                source_port_identity: e_best_parent,
                received_on_port: other_port,
            },
        );

        let bmca = BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, local_port);

        let worse = TestClockDS::default_mid_grade();
        let e_rbest_parent = PortIdentity::new(worse.clock_identity(), PortNumber::new(1));
        let e_rbest_ds = worse.dataset();
        let e_rbest = BestForeignDataset::Qualified {
            ds: &e_rbest_ds,
            source_port_identity: &e_rbest_parent,
            received_on_port: local_port,
        };

        // Not-S1 condition: when Ebest was not received on this port, we must not recommend Slave.
        let decision = bmca.decision(e_rbest);
        assert!(!matches!(decision, BmcaDecision::Slave(_)));
    }

    #[test]
    fn sliding_window_qualification_requires_two_fast_announces() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // First Announce: record is created, still unqualified.
        assert!(record.qualified_ds().is_none());

        // Second Announce within the window makes it qualified.
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified_ds().is_some());
    }

    #[test]
    fn sliding_window_qualification_drops_on_slow_announces() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // A second fast announce to qualify.
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified_ds().is_some());

        // After a long gap, a single announce is not enough to stay qualified.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());
    }

    #[test]
    fn sliding_window_never_qualifies_with_one_annonce_per_window() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // Announce just slower than foreignMasterTimeWindow; density never reaches 2
        // within any sliding window.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(5),
        );
        record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());

        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());

        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(15),
        );
        record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());
    }

    #[test]
    fn qualified_dataset_change_reports_updated() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let foreign_low = TestClockDS::default_low_grade_slave_only().dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // First fast Announce qualifies the record (two samples: t0 and t1).
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified_ds().is_some());

        // Change dataset while still qualified.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(2),
        );
        let status = record.update_from(&updated_record);
        assert_eq!(status, ForeignClockStatus::Updated);
        assert_eq!(record.qualified_ds(), Some(&foreign_low));
    }

    #[test]
    fn unqualified_dataset_change_does_not_report_updated() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let foreign_low = TestClockDS::default_low_grade_slave_only().dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // First slow Announce: still unqualified (spacing >= window).
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        let _ = record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());

        // Second slow Announce with changed dataset: still unqualified and should report Unchanged.
        let updated_record = ForeignClockRecord::new(
            port_id,
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(20),
        );
        let status = record.update_from(&updated_record);
        assert_eq!(status, ForeignClockStatus::Unchanged);
        assert!(record.qualified_ds().is_none());
    }

    #[test]
    fn sliding_window_edge_drops_qualification_but_not_stale() {
        let t0 = Instant::from_secs(0);
        let high = TestClockDS::default_high_grade();
        let foreign_high = high.dataset();
        let port_id = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let mut record = ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), t0);

        // Qualify with a fast Announce.
        let new_record = ForeignClockRecord::new(
            port_id,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(1),
        );
        record.update_from(&new_record);
        assert!(record.qualified_ds().is_some());

        // Next Announce spaced just beyond the window drops qualification
        // but the record is not stale yet.
        let now = Instant::from_nanos(5_000_000_001);
        let updated_record =
            ForeignClockRecord::new(port_id, foreign_high, LogInterval::new(0), now);
        let _ = record.update_from(&updated_record);
        assert!(record.qualified_ds().is_none());
        assert!(!record.is_stale(now));
    }

    #[test]
    fn test_foreign_clock_ordering() {
        let high = TestClockDS::default_high_grade().dataset();
        let mid = TestClockDS::default_mid_grade().dataset();
        let low = TestClockDS::default_low_grade_slave_only().dataset();

        assert!(high.better_than(&mid));
        assert!(!mid.better_than(&high));

        assert!(high.better_than(&low));
        assert!(!low.better_than(&high));

        assert!(mid.better_than(&low));
        assert!(!low.better_than(&mid));
    }

    #[test]
    fn test_foreign_clock_equality() {
        let c1 = TestClockDS::default_high_grade().dataset();
        let c2 = TestClockDS::default_high_grade().dataset();

        assert!(!c1.better_than(&c2));
        assert!(!c2.better_than(&c1));
        assert_eq!(c1, c2);
    }

    #[test]
    fn foreign_clock_ds_compares_priority1_before_quality() {
        let a = TestClockDS::default_high_grade()
            .with_priority1(Priority1::new(10))
            .dataset();
        let b = TestClockDS::atomic_grandmaster()
            .with_priority1(Priority1::new(20))
            .dataset();

        // Atomic grandmaster looses againt default high grade because of lower priority1.
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_compares_priority2_after_quality() {
        let a = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x00; 8]))
            .with_priority2(Priority2::new(10))
            .dataset();
        let b = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x01; 8]))
            .with_priority2(Priority2::new(20))
            .dataset();

        // Same p1 and quality; lower p2 wins.
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_tiebreaks_on_identity_last() {
        let a = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x00; 8]))
            .dataset();
        let b = TestClockDS::default_high_grade()
            .with_clock_identity(ClockIdentity::new(&[0x01; 8]))
            .dataset();

        // All fields equal except identity; lower identity better than higher.
        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn foreign_clock_ds_prefers_lower_steps_removed_when_identity_equal() {
        let a = TestClockDS::default_high_grade()
            .with_steps_removed(StepsRemoved::new(1))
            .dataset();
        let b = TestClockDS::default_high_grade()
            .with_steps_removed(StepsRemoved::new(3))
            .dataset();

        assert!(a.better_than(&b));
        assert!(!b.better_than(&a));
    }

    #[test]
    fn listening_bmca_prunes_stale_foreign_clocks_on_next_announce_reception() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let high = TestClockDS::default_high_grade();
        let mid = TestClockDS::default_mid_grade();
        let low = TestClockDS::default_low_grade_slave_only();
        let gm = TestClockDS::gps_grandmaster();

        let stale_records = vec![
            ForeignClockRecord::qualified(
                PortIdentity::new(high.clock_identity(), PortNumber::new(1)),
                high.dataset(),
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::qualified(
                PortIdentity::new(mid.clock_identity(), PortNumber::new(1)),
                mid.dataset(),
                LogInterval::new(0),
                Instant::from_secs(1),
            ),
            ForeignClockRecord::qualified(
                PortIdentity::new(mid.clock_identity(), PortNumber::new(1)),
                low.dataset(),
                LogInterval::new(0),
                Instant::from_secs(2),
            ),
        ];
        let records = ForeignClockRecordsVec::from_records(&stale_records);

        let gm_port_id = PortIdentity::new(gm.clock_identity(), PortNumber::new(1));
        let gm_foreign = gm.dataset();

        // Consider a new announce from a different foreign clock.
        let default_ds = TestClockDS::default_high_grade().dataset();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), records),
        );
        bmca.consider(
            gm_port_id,
            gm_foreign,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        // Second announce qualifies the record.
        bmca.consider(
            gm_port_id,
            gm_foreign,
            LogInterval::new(0),
            Instant::from_secs(10),
        );

        assert_eq!(
            bmca.best_foreign.foreign_clock_records.len(),
            1,
            "stale foreign clock records should have been pruned"
        );
        let best = bmca
            .best_foreign
            .foreign_clock_records
            .best_qualified()
            .expect("record should be qualified after two announces");
        assert_eq!(best.source_port_identity(), &gm_port_id);
        assert_eq!(best.qualified_ds(), Some(&gm_foreign));
    }

    #[test]
    fn listening_bmca_gm_capable_local_with_no_qualified_foreign_is_undecided() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::gps_grandmaster().dataset();
        let bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn listening_bmca_gm_capable_local_loses_tuple_returns_passive() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::gps_grandmaster().dataset();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        // Foreign uses lower priority1 so it is better, even though clock class is worse.
        let foreign = TestClockDS::default_low_grade_slave_only().with_priority1(Priority1::new(1));
        let port_id = PortIdentity::new(foreign.clock_identity(), PortNumber::new(1));
        let foreign_strong = foreign.dataset();

        bmca.consider(
            port_id,
            foreign_strong,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id,
            foreign_strong,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(), Some(BmcaDecision::Passive));
    }

    #[test]
    fn listening_bmca_gm_capable_local_better_than_foreign_returns_master_m1() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::gps_grandmaster().dataset();
        let local_identity = *default_ds.identity();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        // Foreign is worse (higher priority1), so local GM-capable should become Master(M1).
        let foreign = TestClockDS::default_high_grade().dataset();
        let port_id = PortIdentity::new(
            TestClockDS::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(),
            Some(BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0),
                local_identity,
            )))
        );
    }

    #[test]
    fn listening_bmca_non_gm_local_better_than_foreign_returns_master_m2() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let local_identity = *default_ds.identity();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        // Foreign is slightly worse quality; local should be better and take M2.
        let foreign = TestClockDS::default_low_grade_slave_only().dataset();
        let port_id = PortIdentity::new(
            TestClockDS::default_low_grade_slave_only().clock_identity(),
            PortNumber::new(1),
        );

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(),
            Some(BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0),
                local_identity,
            )))
        );
    }

    #[test]
    fn listening_bmca_non_gm_local_loses_tuple_returns_slave() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        let foreign = TestClockDS::default_high_grade().dataset();
        let port_id = PortIdentity::new(
            TestClockDS::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        assert_eq!(
            bmca.decision(),
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id))),
        );
    }

    #[test]
    fn listening_bmca_recommends_slave_from_interleaved_announce_sequence() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_low_grade().dataset();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        let high = TestClockDS::default_high_grade();
        let mid = TestClockDS::default_mid_grade();
        let foreign_high = high.dataset();
        let foreign_mid = mid.dataset();

        let port_id_high = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let port_id_mid = PortIdentity::new(mid.clock_identity(), PortNumber::new(1));

        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let decision = bmca.decision();

        assert_eq!(
            decision,
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id_high))),
        );
    }

    #[test]
    fn listening_bmca_recommends_slave_from_non_interleaved_announce_sequence() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_low_grade().dataset();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        let high = TestClockDS::default_high_grade();
        let mid = TestClockDS::default_mid_grade();
        let foreign_high = high.dataset();
        let foreign_mid = mid.dataset();

        let port_id_high = PortIdentity::new(high.clock_identity(), PortNumber::new(1));
        let port_id_mid = PortIdentity::new(mid.clock_identity(), PortNumber::new(1));

        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_high,
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            port_id_mid,
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        let decision = bmca.decision();

        assert_eq!(
            decision,
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id_high)))
        );
    }

    #[test]
    fn listening_bmca_undecided_when_no_announces_yet() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn listening_bmca_undecided_when_no_qualified_clock_records_yet() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        let foreign_high = TestClockDS::default_high_grade().dataset();

        bmca.consider(
            PortIdentity::fake(),
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn listening_bmca_undecided_when_only_single_announces_each() {
        let foreign_candidates = Cell::new(BestForeignSnapshot::Empty);
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, &foreign_candidates, PortNumber::new(1)),
            BestForeignRecord::new(PortNumber::new(1), ForeignClockRecordsVec::new()),
        );

        let high = TestClockDS::default_high_grade();
        let mid = TestClockDS::default_mid_grade();
        let low = TestClockDS::default_low_grade_slave_only();
        let foreign_high = high.dataset();
        let foreign_mid = mid.dataset();
        let foreign_low = low.dataset();

        bmca.consider(
            PortIdentity::new(high.clock_identity(), PortNumber::new(0)),
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            PortIdentity::new(mid.clock_identity(), PortNumber::new(0)),
            foreign_mid,
            LogInterval::new(0),
            Instant::from_secs(0),
        );
        bmca.consider(
            PortIdentity::new(low.clock_identity(), PortNumber::new(0)),
            foreign_low,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(), None);
    }

    #[test]
    fn qualification_timeout_policy_duration_m1_is_zero() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M1, StepsRemoved::new(5));

        let qualification_timeout_interval = policy.duration(LogInterval::new(4));

        assert_eq!(qualification_timeout_interval, Duration::from_secs(0));
    }

    #[test]
    fn qualification_timeout_policy_duration_m2_is_zero() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M2, StepsRemoved::new(5));

        let qualification_timeout_interval = policy.duration(LogInterval::new(4));

        assert_eq!(qualification_timeout_interval, Duration::from_secs(0));
    }

    #[test]
    fn qualification_timeout_policy_duration_m3_scales_with_steps_removed() {
        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(0));
        let qualification_timeout_interval = policy.duration(LogInterval::new(0));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(1));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(1));
        let qualification_timeout_interval = policy.duration(LogInterval::new(0));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(2));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(2));
        let qualification_timeout_interval = policy.duration(LogInterval::new(1));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(6));

        let policy =
            QualificationTimeoutPolicy::new(BmcaMasterDecisionPoint::M3, StepsRemoved::new(3));
        let qualification_timeout_interval = policy.duration(LogInterval::new(2));
        assert_eq!(qualification_timeout_interval, Duration::from_secs(16));
    }

    #[test]
    fn qualification_timeout_policy_duration_m3_large_inputs() {
        let policy = QualificationTimeoutPolicy::new(
            BmcaMasterDecisionPoint::M3,
            StepsRemoved::new(u16::MAX),
        );

        let qualification_timeout_interval = policy.duration(LogInterval::new(10));

        assert_eq!(
            qualification_timeout_interval,
            Duration::from_secs(67_108_864)
        );
    }
}
