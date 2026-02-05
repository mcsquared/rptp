//! Best Master Clock Algorithm (BMCA) and roles.
//!
//! This module implements the *algorithm* and *roles* of the IEEE 1588-2019 best-master selection
//! story: comparing local and foreign candidates, producing high-level decisions, and gating
//! decision emission while tracking an already-selected parent or grandmaster.
//!
//! - **Algorithm** – [`BestMasterClockAlgorithm`] implements the core BMCA comparison and decision
//!   logic (IEEE 1588-2019 §9.3.3, Fig. 33). It consumes a local candidate ([`LocalGrandMasterCandidate`])
//!   and best-foreign snapshots ([`BestForeignSnapshot`]) and produces a [`BmcaDecision`] (Master /
//!   Slave / Passive).
//! - **Roles** – [`ListeningBmca`], [`GrandMasterTrackingBmca`], [`ParentTrackingBmca`],
//!   [`PassiveBmca`] wrap the core algorithm to gate when decisions are emitted (e.g. only when
//!   a qualified foreign exists, or when the tracked grandmaster/parent changes).
//! - **State decision events** – [`StateDecisionEvent`] and [`StateDecisionEventTrigger`] allow
//!   the clock to coordinate BMCA evaluation across ports (e.g. when `e_rbest` changes).
//!
//! Clock dataset types and BMCA comparison tuple wire format live in [`crate::dataset`]. Foreign
//! master tracking, qualification, and storage ([`ForeignClockRecords`], [`BestForeignSnapshot`],
//! etc.) live in [`crate::foreign`].
//!
//! ## Current limitations
//!
//! - Multi-port / boundary-clock scenarios are still evolving; some collaborators are intentionally
//!   modeled as traits so that storage and topology can be swapped without changing core logic.
//! - Some topology-only comparisons are currently simplified; TODOs in this module mark those
//!   places explicitly.

use crate::clock::{ClockIdentity, TimeScale};
use crate::dataset::{ClockDS, StepsRemoved, TopologyDS};
use crate::foreign::{
    BestForeignRecord, BestForeignSnapshot, ForeignClockRecords, QualifiedForeignSnapshot,
};
use crate::message::{AnnounceMessage, SequenceId};
use crate::port::{ParentPortIdentity, Port, PortIdentity, PortNumber};
use crate::portstate::{PortState, StateDecision};
use crate::time::{Duration, Instant, LogInterval, LogMessageInterval};

/// Best Master Clock Algorithm interface.
///
/// The BMCA is driven by Announce reception: each received Announce contributes a new observation
/// about a foreign clock candidate. Implementations keep local state (e.g. qualification windows)
/// and can emit a high-level decision once enough information is available.
pub(crate) trait Bmca {
    /// Consider a new Announce-derived snapshot for a foreign clock candidate.
    ///
    /// `log_announce_interval` should be the interval announced by the sender, and is used for
    /// qualification and staleness decisions.
    ///
    /// Implementations should check for changes in the best foreign dataset and trigger state
    /// decision events when changes occur.
    fn consider(
        &mut self,
        source_port_identity: PortIdentity,
        foreign_clock_ds: ClockDS,
        log_announce_interval: LogInterval,
        now: Instant,
    );

    /// Return the latest BMCA decision, if one should be acted upon.
    ///
    /// Returning `None` means "no new decision": either not enough information is available yet
    /// (e.g. no qualified foreign candidates), or the decision would be identical to the currently
    /// tracked parent/grandmaster.
    fn decision(&self, e_best: &BestForeignSnapshot) -> Option<BmcaDecision>;
}

/// Trait for triggering state decision events in clocks.
///
/// This is used internally by BMCA roles to notify the clock when e_rbest changes,
/// allowing the clock to coordinate state decisions across multiple ports.
pub trait StateDecisionEvent {
    /// Trigger a state decision event with the port's current e_rbest observation.
    ///
    /// The clock implementation should:
    /// 1. Call `remember(port_number, e_rbest)` on ForeignGrandMasterCandidates
    /// 2. Get the updated `e_best = foreign_candidates.best()`
    /// 3. Call `port.state_decision_event(e_best)` to trigger BMCA evaluation
    fn trigger(&self, port_number: PortNumber, e_rbest: BestForeignSnapshot);
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

/// Best Master Clock Algorithm implementation.
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
///  - M3 decision logic is still simplified: the current implementation does not yet plumb the
///    correct `stepsRemoved` value into the M3 master decision; see TODO in
///    [`BetterByTopologyDecision::decision`].
pub(crate) struct BestMasterClockAlgorithm<'a> {
    local_candidate: &'a dyn LocalGrandMasterCandidate,
    port_number: PortNumber,
}

impl<'a> BestMasterClockAlgorithm<'a> {
    /// Create a new BMCA evaluator for `port_number`.
    ///
    /// `local_candidate` supplies the local data set snapshot (`D_0`).
    /// `foreign_candidates` aggregates best foreign snapshots across ports. (`E_best`)
    pub fn new(
        local_candidate: &'a dyn LocalGrandMasterCandidate,
        port_number: PortNumber,
    ) -> Self {
        Self {
            local_candidate,
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
    pub(crate) fn decision(
        &self,
        e_best: &BestForeignSnapshot,
        e_rbest: &BestForeignSnapshot,
    ) -> BmcaDecision {
        let d_0 = LocalDS::new(self.local_candidate.snapshot());
        if d_0.is_authoritative() {
            AuthoritativeDecision::new(&d_0, e_rbest).decision()
        } else {
            NonAuthoritativeDecision::new(
                &d_0,
                e_best,
                ReceivedOnPortDecision::new(
                    self.port_number,
                    BetterByTopologyDecision::new(e_rbest),
                ),
            )
            .decision()
        }
    }
}

/// Local dataset view used by BMCA decisions.
///
/// In IEEE 1588-2019 BMCA terminology, this is the local candidate dataset `D_0`: the data set
/// snapshot the clock compares against the best foreign datasets (`E_best` / `E_rbest`).
///
/// This wrapper exists to keep [`ClockDS`] itself focused on dataset fields and ordering rules,
/// while `LocalDS` carries BMCA-specific context such as the “self topology” port identity
/// (`PortIdentity(local_clock_id, 0)`) used when comparing the local dataset against foreign
/// snapshots with topology tie-breaks.
struct LocalDS {
    ds: ClockDS,
    local_topo_id: PortIdentity,
}

impl LocalDS {
    fn new(ds: ClockDS) -> Self {
        // IEEE 1588-2019 §9.3.4, Tbl. 29: the “self” topology for the local dataset D_0 uses
        // PortIdentity(local_clock_identity, 0) for both sender and receiver.
        let local_topo_id = PortIdentity::new(ds.identity(), PortNumber::new(0));

        Self { ds, local_topo_id }
    }

    fn is_authoritative(&self) -> bool {
        self.ds.is_authoritative()
    }

    /// Compare this local dataset against a foreign snapshot with topology awareness.
    ///
    /// Returns `Ok(local_clock_id)` if this dataset is better than the foreign snapshot according
    /// to BMCA rules including topology tie-breaks.
    ///
    /// Returns `Err(qualified_foreign)` if the foreign snapshot is qualified and not beaten by the
    /// local dataset. This is intended to let callers handle the "local loses" branch without
    /// re-matching on `BestForeignSnapshot` (e.g. via `map(..).unwrap_or_else(..)`).
    ///
    /// This uses the IEEE 1588 “self topology” identity: `PortIdentity(local_clock_id, 0)` for both
    /// sender and receiver when comparing the local dataset against a foreign snapshot
    /// (IEEE 1588-2019 §9.3.4, Tbl. 29).
    fn is_better_or_better_by_topology<'b>(
        &self,
        other: &'b BestForeignSnapshot,
    ) -> Result<ClockIdentity, QualifiedForeignSnapshot<'b>> {
        let local_clock_id = self.ds.identity();

        match other {
            BestForeignSnapshot::Qualified {
                ds: foreign_ds,
                source_port_identity,
                receiver,
            } => TopologyDS::local(&self.ds, &self.local_topo_id)
                .is_better_or_better_by_topology(TopologyDS::foreign(
                    foreign_ds,
                    source_port_identity,
                    receiver,
                ))
                .then_some(local_clock_id)
                .ok_or_else(|| {
                    QualifiedForeignSnapshot::new(foreign_ds, source_port_identity, receiver)
                }),
            BestForeignSnapshot::Empty => Ok(local_clock_id),
        }
    }
}

/// Decision between M1 and P1
struct AuthoritativeDecision<'a> {
    d_0: &'a LocalDS,
    e_rbest: &'a BestForeignSnapshot,
}

impl<'a> AuthoritativeDecision<'a> {
    fn new(d_0: &'a LocalDS, e_rbest: &'a BestForeignSnapshot) -> Self {
        Self { d_0, e_rbest }
    }

    fn decision(&self) -> BmcaDecision {
        self.d_0
            .is_better_or_better_by_topology(self.e_rbest)
            .map(|local_clock_id| {
                BmcaDecision::Master(BmcaMasterDecision::new(
                    BmcaMasterDecisionPoint::M1,
                    StepsRemoved::new(0), // IEEE 1588-2019 §3.9.5; Tbl. 30 (stepsRemoved=0 when acting as GM)
                    local_clock_id,
                ))
            })
            .unwrap_or(BmcaDecision::Passive) // Passive decision point P1
    }
}

/// Decision between M2 and the S1/P2/M3 alternatives (IEEE 1588-2019 §9.3.3; Fig. 33).
struct NonAuthoritativeDecision<'a> {
    d_0: &'a LocalDS,
    e_best: &'a BestForeignSnapshot,
    received_on_port: ReceivedOnPortDecision<'a>,
}

impl<'a> NonAuthoritativeDecision<'a> {
    fn new(
        d_0: &'a LocalDS,
        e_best: &'a BestForeignSnapshot,
        received_on_port: ReceivedOnPortDecision<'a>,
    ) -> Self {
        Self {
            d_0,
            e_best,
            received_on_port,
        }
    }

    fn decision(&self) -> BmcaDecision {
        self.d_0
            .is_better_or_better_by_topology(self.e_best)
            .map(|local_clock_id| {
                BmcaDecision::Master(BmcaMasterDecision::new(
                    BmcaMasterDecisionPoint::M2,
                    StepsRemoved::new(0), // IEEE 1588-2019 §9.3.5; Tbl. 30 (stepsRemoved=0 when acting as master)
                    local_clock_id,
                ))
            })
            .unwrap_or_else(|qualified_e_best| self.received_on_port.decision(qualified_e_best))
    }
}

/// Decision between S1 when e_best was received on this port, and P2/M3 when e_best was not
/// received on this port (IEEE 1588-2019 §9.3.3; Fig. 33).
struct ReceivedOnPortDecision<'a> {
    port_number: PortNumber,
    better_by_topology: BetterByTopologyDecision<'a>,
}

impl<'a> ReceivedOnPortDecision<'a> {
    fn new(port_number: PortNumber, better_by_topology: BetterByTopologyDecision<'a>) -> Self {
        Self {
            port_number,
            better_by_topology,
        }
    }

    fn decision(&self, e_best: QualifiedForeignSnapshot) -> BmcaDecision {
        if let Some(parent) = e_best.parent_if_received_on_port(self.port_number) {
            BmcaDecision::Slave(parent) // Slave decision point S1
        } else {
            self.better_by_topology.decision(e_best)
        }
    }
}

/// Decision between P2 and M3 (IEEE 1588-2019 §9.3.3; Fig. 33).
struct BetterByTopologyDecision<'a> {
    e_rbest: &'a BestForeignSnapshot,
}

impl<'a> BetterByTopologyDecision<'a> {
    fn new(e_rbest: &'a BestForeignSnapshot) -> Self {
        Self { e_rbest }
    }

    fn decision(&self, e_best: QualifiedForeignSnapshot) -> BmcaDecision {
        // Topology-only comparison: P2 if e_best is better than e_rbest by topology only.
        // See IEEE 1588-2019 §9.3.3; Fig. 33, diamond for decision point P2 vs M3.
        if e_best.is_better_by_topology(self.e_rbest) {
            BmcaDecision::Passive // Passive decision point P2
        } else {
            BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M3,
                StepsRemoved::new(0), // TODO: use currently used steps removed field here
                e_best.grandmaster_id(),
            ))
        }
    }
}

/// Encapsulates change detection and state decision event triggering.
///
/// Tracks the previous `e_rbest` snapshot and triggers a state decision event
/// when the snapshot changes. Also provides a plain `trigger()` method for
/// manual triggering with the current snapshot.
pub(crate) struct StateDecisionEventTrigger<'a> {
    previous_best_foreign: BestForeignSnapshot,
    state_decision_event: &'a dyn StateDecisionEvent,
    received_on_port: PortNumber,
}

impl<'a> StateDecisionEventTrigger<'a> {
    /// Create a new trigger starting from the given previous snapshot.
    ///
    /// `previous_best_foreign` is the initial snapshot state. Use `BestForeignSnapshot::Empty`
    /// for a fresh start, or pass the current snapshot from `BestForeignRecord` when
    /// initializing with pre-existing records to avoid spurious change events.
    ///
    /// `received_on_port` identifies the port this trigger is associated with.
    pub fn new(
        state_decision_event: &'a dyn StateDecisionEvent,
        previous_best_foreign: BestForeignSnapshot,
        received_on_port: PortNumber,
    ) -> Self {
        Self {
            previous_best_foreign,
            state_decision_event,
            received_on_port,
        }
    }

    /// Trigger the state decision event with the current snapshot.
    ///
    /// This method always triggers the event, regardless of whether the snapshot
    /// has changed. It also updates the internal change-tracking state so that
    /// a subsequent `gated_trigger()` with the same snapshot will not
    /// produce a redundant event.
    ///
    /// Common use cases include:
    /// - Before converting BMCA roles (e.g., in `into_current_grandmaster_tracking()`)
    /// - After timeouts or explicit state transitions
    /// - When forcing a state decision evaluation
    pub fn trigger(&mut self, current: BestForeignSnapshot) {
        self.previous_best_foreign = current;
        self.state_decision_event
            .trigger(self.received_on_port, current);
    }

    /// Check if the current snapshot differs from the previous, and trigger
    /// the event only if it changed.
    ///
    /// This method is used during normal BMCA operation (e.g., in `Bmca::consider()`)
    /// to avoid redundant state decision events when the best foreign snapshot
    /// hasn't changed.
    ///
    /// Unlike `trigger()`, this method only invokes the event when the snapshot
    /// has actually changed; both methods update the internal change-tracking state.
    pub fn gated_trigger(&mut self, current: BestForeignSnapshot) {
        if self.previous_best_foreign != current {
            self.previous_best_foreign = current;
            self.state_decision_event
                .trigger(self.received_on_port, current);
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
    state_decision_trigger: StateDecisionEventTrigger<'a>,
}

impl<'a, S: ForeignClockRecords> ListeningBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        state_decision_trigger: StateDecisionEventTrigger<'a>,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            state_decision_trigger,
        }
    }

    /// Convert to a ParentTrackingBmca that tracks the given parent.
    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(
            self.bmca,
            self.best_foreign,
            parent_port_identity,
            self.state_decision_trigger,
        )
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given grandmaster.
    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(
            self.bmca,
            self.best_foreign,
            grandmaster_id,
            self.state_decision_trigger,
        )
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the currently known grandmaster.
    ///
    /// Triggers a state decision event before converting to ensure the clock is notified
    /// of the current best foreign snapshot.
    pub(crate) fn into_current_grandmaster_tracking(mut self) -> GrandMasterTrackingBmca<'a, S> {
        let current = self.best_foreign.snapshot();
        self.state_decision_trigger.trigger(current);
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }

    /// Convert to a PassiveBmca for transitioning to passive state.
    pub(crate) fn into_passive(self) -> PassiveBmca<'a, S> {
        PassiveBmca::new(self.bmca, self.best_foreign, self.state_decision_trigger)
    }

    /// Decompose ListeningBmca into its parts.
    pub(crate) fn into_parts(
        self,
    ) -> (
        BestMasterClockAlgorithm<'a>,
        BestForeignRecord<S>,
        StateDecisionEventTrigger<'a>,
    ) {
        (self.bmca, self.best_foreign, self.state_decision_trigger)
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
        let current = self.best_foreign.snapshot();
        self.state_decision_trigger.gated_trigger(current);
    }

    /// Return a decision in listening mode only if there is a qualified foreign dataset, as in
    /// IEEE 1588-2019 §9.3.3; Fig. 33 - first flow chart diamond.
    fn decision(&self, e_best: &BestForeignSnapshot) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.snapshot();

        match e_rbest {
            BestForeignSnapshot::Qualified { .. } => Some(self.bmca.decision(e_best, &e_rbest)),
            BestForeignSnapshot::Empty => None,
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

    pub fn identity(&self) -> ClockIdentity {
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
    state_decision_trigger: StateDecisionEventTrigger<'a>,
}

impl<'a, S: ForeignClockRecords> GrandMasterTrackingBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        grandmaster_id: ClockIdentity,
        state_decision_trigger: StateDecisionEventTrigger<'a>,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            grandmaster_id,
            state_decision_trigger,
        }
    }

    /// Access the currently tracked grandmaster via the given closure.
    pub(crate) fn using_grandmaster<R>(&self, f: impl FnOnce(GrandMaster<'_>) -> R) -> R {
        self.bmca.using_grandmaster(f)
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given new grandmaster.
    pub(crate) fn with_grandmaster_id(self, grandmaster_id: ClockIdentity) -> Self {
        Self {
            grandmaster_id,
            ..self
        }
    }

    /// Convert to a ParentTrackingBmca that tracks the given parent.
    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(
            self.bmca,
            self.best_foreign,
            parent_port_identity,
            self.state_decision_trigger,
        )
    }

    /// Convert to a PassiveBmca for transitioning to passive state.
    pub(crate) fn into_passive(self) -> PassiveBmca<'a, S> {
        PassiveBmca::new(self.bmca, self.best_foreign, self.state_decision_trigger)
    }

    /// Decompose GrandMasterTrackingBmca into its parts.
    pub(crate) fn into_parts(
        self,
    ) -> (
        BestMasterClockAlgorithm<'a>,
        BestForeignRecord<S>,
        StateDecisionEventTrigger<'a>,
    ) {
        (self.bmca, self.best_foreign, self.state_decision_trigger)
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
        let current = self.best_foreign.snapshot();
        self.state_decision_trigger.gated_trigger(current);
    }

    /// Return a decision while suppressing identical master decisions.
    fn decision(&self, e_best: &BestForeignSnapshot) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.snapshot();
        let decision = self.bmca.decision(e_best, &e_rbest);

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
    state_decision_trigger: StateDecisionEventTrigger<'a>,
}

impl<'a, S: ForeignClockRecords> ParentTrackingBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        parent_port_identity: ParentPortIdentity,
        state_decision_trigger: StateDecisionEventTrigger<'a>,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            parent_port_identity,
            state_decision_trigger,
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
            parent_port_identity: parent,
            ..self
        }
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given grandmaster.
    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(
            self.bmca,
            self.best_foreign,
            grandmaster_id,
            self.state_decision_trigger,
        )
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the currently known grandmaster.
    ///
    /// Triggers a state decision event before converting to ensure the clock is notified
    /// of the current best foreign snapshot.
    pub(crate) fn into_current_grandmaster_tracking(mut self) -> GrandMasterTrackingBmca<'a, S> {
        let current = self.best_foreign.snapshot();
        self.state_decision_trigger.trigger(current);
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }

    /// Convert to a PassiveBmca for transitioning to passive state.
    pub(crate) fn into_passive(self) -> PassiveBmca<'a, S> {
        PassiveBmca::new(self.bmca, self.best_foreign, self.state_decision_trigger)
    }

    /// Decompose ParentTrackingBmca into its parts.
    pub(crate) fn into_parts(
        self,
    ) -> (
        BestMasterClockAlgorithm<'a>,
        BestForeignRecord<S>,
        StateDecisionEventTrigger<'a>,
    ) {
        (self.bmca, self.best_foreign, self.state_decision_trigger)
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
        let current = self.best_foreign.snapshot();
        self.state_decision_trigger.gated_trigger(current);
    }

    /// Return a decision while suppressing identical decisions with the same parent.
    fn decision(&self, e_best: &BestForeignSnapshot) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.snapshot();
        let decision = self.bmca.decision(e_best, &e_rbest);

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
    state_decision_trigger: StateDecisionEventTrigger<'a>,
}

impl<'a, S: ForeignClockRecords> PassiveBmca<'a, S> {
    pub fn new(
        bmca: BestMasterClockAlgorithm<'a>,
        best_foreign: BestForeignRecord<S>,
        state_decision_trigger: StateDecisionEventTrigger<'a>,
    ) -> Self {
        Self {
            bmca,
            best_foreign,
            state_decision_trigger,
        }
    }

    /// Convert to a ParentTrackingBmca that tracks the given parent.
    pub(crate) fn into_parent_tracking(
        self,
        parent_port_identity: ParentPortIdentity,
    ) -> ParentTrackingBmca<'a, S> {
        ParentTrackingBmca::new(
            self.bmca,
            self.best_foreign,
            parent_port_identity,
            self.state_decision_trigger,
        )
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the given grandmaster.
    pub(crate) fn into_grandmaster_tracking(
        self,
        grandmaster_id: ClockIdentity,
    ) -> GrandMasterTrackingBmca<'a, S> {
        GrandMasterTrackingBmca::new(
            self.bmca,
            self.best_foreign,
            grandmaster_id,
            self.state_decision_trigger,
        )
    }

    /// Convert to a GrandMasterTrackingBmca that tracks the currently known grandmaster.
    ///
    /// Triggers a state decision event before converting to ensure the clock is notified
    /// of the current best foreign snapshot.
    pub(crate) fn into_current_grandmaster_tracking(mut self) -> GrandMasterTrackingBmca<'a, S> {
        let current = self.best_foreign.snapshot();
        self.state_decision_trigger.trigger(current);
        let current_grandmaster_id = self.bmca.using_grandmaster(|gm| gm.identity());
        self.into_grandmaster_tracking(current_grandmaster_id)
    }

    /// Decompose PassiveBmca into its parts.
    pub(crate) fn into_parts(
        self,
    ) -> (
        BestMasterClockAlgorithm<'a>,
        BestForeignRecord<S>,
        StateDecisionEventTrigger<'a>,
    ) {
        (self.bmca, self.best_foreign, self.state_decision_trigger)
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
        let current = self.best_foreign.snapshot();
        self.state_decision_trigger.gated_trigger(current);
    }

    /// Return a decision while suppressing passive decisions.
    ///
    /// Suppresses `BmcaDecision::Passive` to avoid repeatedly re-emitting identical transitions
    /// while in passive state. Master and slave decisions are always emitted to allow transitions
    /// away from passive state.
    fn decision(&self, e_best: &BestForeignSnapshot) -> Option<BmcaDecision> {
        let e_rbest = self.best_foreign.snapshot();
        let decision = self.bmca.decision(e_best, &e_rbest);

        match decision {
            BmcaDecision::Passive => None,
            _ => Some(decision),
        }
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
        // NoopBmca never changes state
    }

    fn decision(&self, _e_best: &BestForeignSnapshot) -> Option<BmcaDecision> {
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dataset::Priority1;
    use crate::foreign::{ForeignClockRecord, ForeignGrandMasterCandidates};
    use crate::infra::infra_support::{ForeignClockRecordsVec, MultiPortForeignCandidates};
    use crate::port::{PortIdentity, PortNumber};
    use crate::test_support::{FakeStateDecisionEvent, TestClockDS};
    use crate::time::{Duration, Instant};

    #[test]
    fn trigger_always_fires_and_uses_received_on_port() {
        let event = FakeStateDecisionEvent::new();
        let mut trigger =
            StateDecisionEventTrigger::new(&event, BestForeignSnapshot::Empty, PortNumber::new(2));
        let snapshot = BestForeignSnapshot::Qualified {
            ds: TestClockDS::default_high_grade().dataset(),
            source_port_identity: PortIdentity::fake(),
            receiver: PortIdentity::fake_on_port(PortNumber::new(2)),
        };

        trigger.trigger(snapshot);

        let events = event.take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, PortNumber::new(2));
        assert_eq!(events[0].1, snapshot);
    }

    #[test]
    fn trigger_updates_state_so_gated_trigger_same_snapshot_does_not_fire_again() {
        let event = FakeStateDecisionEvent::new();
        let mut trigger =
            StateDecisionEventTrigger::new(&event, BestForeignSnapshot::Empty, PortNumber::new(1));
        let snapshot = BestForeignSnapshot::Qualified {
            ds: TestClockDS::default_high_grade().dataset(),
            source_port_identity: PortIdentity::fake(),
            receiver: PortIdentity::fake_on_port(PortNumber::new(1)),
        };

        trigger.trigger(snapshot);
        trigger.gated_trigger(snapshot);

        let events = event.take_events();
        assert_eq!(
            events.len(),
            1,
            "gated_trigger(same) must not produce a redundant event"
        );
    }

    #[test]
    fn gated_trigger_fires_only_when_snapshot_changes() {
        let event = FakeStateDecisionEvent::new();
        let mut trigger =
            StateDecisionEventTrigger::new(&event, BestForeignSnapshot::Empty, PortNumber::new(1));

        trigger.gated_trigger(BestForeignSnapshot::Empty);
        trigger.gated_trigger(BestForeignSnapshot::Empty);

        let events = event.take_events();
        assert_eq!(
            events.len(),
            0,
            "identical Empty snapshots must not trigger"
        );
    }

    #[test]
    fn gated_trigger_fires_when_snapshot_changes() {
        let event = FakeStateDecisionEvent::new();
        let mut trigger =
            StateDecisionEventTrigger::new(&event, BestForeignSnapshot::Empty, PortNumber::new(1));
        let snapshot = BestForeignSnapshot::Qualified {
            ds: TestClockDS::default_high_grade().dataset(),
            source_port_identity: PortIdentity::fake(),
            receiver: PortIdentity::fake_on_port(PortNumber::new(1)),
        };

        trigger.gated_trigger(BestForeignSnapshot::Empty);
        trigger.gated_trigger(snapshot);

        let events = event.take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1, snapshot);
    }

    #[test]
    fn gated_trigger_second_call_with_same_value_does_not_fire() {
        let event = FakeStateDecisionEvent::new();
        let mut trigger =
            StateDecisionEventTrigger::new(&event, BestForeignSnapshot::Empty, PortNumber::new(1));
        let snapshot = BestForeignSnapshot::Qualified {
            ds: TestClockDS::default_high_grade().dataset(),
            source_port_identity: PortIdentity::fake(),
            receiver: PortIdentity::fake_on_port(PortNumber::new(1)),
        };

        trigger.gated_trigger(snapshot);
        trigger.gated_trigger(snapshot);

        let events = event.take_events();
        assert_eq!(events.len(), 1, "second gated_trigger(same) must not fire");
    }

    #[test]
    fn trigger_methods_update_state_consistently() {
        let event = FakeStateDecisionEvent::new();
        let mut trigger =
            StateDecisionEventTrigger::new(&event, BestForeignSnapshot::Empty, PortNumber::new(1));
        let snapshot_a = BestForeignSnapshot::Qualified {
            ds: TestClockDS::default_high_grade().dataset(),
            source_port_identity: PortIdentity::fake(),
            receiver: PortIdentity::fake_on_port(PortNumber::new(1)),
        };
        let snapshot_b = BestForeignSnapshot::Empty;

        trigger.trigger(snapshot_a);
        trigger.gated_trigger(snapshot_a);
        trigger.gated_trigger(snapshot_b);

        let events = event.take_events();
        assert_eq!(
            events.len(),
            2,
            "trigger(snap_a) and gated_trigger(snap_b) should fire; gated_trigger(snap_a) should not"
        );
        assert_eq!(events[0].1, snapshot_a);
        assert_eq!(events[1].1, snapshot_b);
    }

    #[test]
    fn grandmaster_tracking_bmca_gates_when_grandmaster_id_matches() {
        let test_clock = TestClockDS::default_high_grade();
        let default_ds = test_clock.dataset();

        let state_decision_event = FakeStateDecisionEvent::new();
        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(
                PortIdentity::fake_on_port(PortNumber::new(1)),
                ForeignClockRecordsVec::new(),
            ),
            test_clock.clock_identity(),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        assert_eq!(bmca.decision(&BestForeignSnapshot::Empty), None);
    }

    #[test]
    fn grandmaster_tracking_bmca_emits_when_grandmaster_id_differs() {
        let default_ds = TestClockDS::default_high_grade().dataset();

        let other_grandmaster_id = TestClockDS::gps_grandmaster().clock_identity();
        let state_decision_event = FakeStateDecisionEvent::new();
        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(
                PortIdentity::fake_on_port(PortNumber::new(1)),
                ForeignClockRecordsVec::new(),
            ),
            other_grandmaster_id,
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        assert!(matches!(
            bmca.decision(&BestForeignSnapshot::Empty),
            Some(BmcaDecision::Master(_))
        ));
    }

    #[test]
    fn grandmaster_tracking_bmca_does_not_gate_passive_decisions() {
        let now = Instant::from_secs(0);

        let gps_clock = TestClockDS::gps_grandmaster();
        let gps_ds = gps_clock.dataset();

        let better = TestClockDS::atomic_grandmaster();
        let better_port_id = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        let better_ds = better.dataset();
        let records = [ForeignClockRecord::new_qualified(
            better_port_id,
            better_ds,
            LogInterval::new(0),
            now,
        )];

        let state_decision_event = FakeStateDecisionEvent::new();
        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&gps_ds, PortNumber::new(1)),
            BestForeignRecord::new(
                PortIdentity::fake_on_port(PortNumber::new(1)),
                ForeignClockRecordsVec::from_records(&records),
            ),
            gps_clock.clock_identity(),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        assert_eq!(
            bmca.decision(&BestForeignSnapshot::Empty),
            Some(BmcaDecision::Passive)
        );
    }

    #[test]
    fn grandmaster_tracking_bmca_does_not_gate_slave_decisions() {
        let now = Instant::from_secs(0);

        let test_clock = TestClockDS::default_low_grade_slave_only();
        let slave_only_ds = test_clock.dataset();

        let better = TestClockDS::default_high_grade();
        let better_port_id = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        let better_ds = better.dataset();
        let records = [ForeignClockRecord::new_qualified(
            better_port_id,
            better_ds,
            LogInterval::new(0),
            now,
        )];

        let state_decision_event = FakeStateDecisionEvent::new();
        let receiver = PortIdentity::fake_on_port(PortNumber::new(1));
        let bmca = GrandMasterTrackingBmca::new(
            BestMasterClockAlgorithm::new(&slave_only_ds, PortNumber::new(1)),
            BestForeignRecord::new(receiver, ForeignClockRecordsVec::from_records(&records)),
            test_clock.clock_identity(),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        // In a single-port scenario, e_best equals e_rbest (the qualified record on this port)
        let e_best = BestForeignSnapshot::Qualified {
            ds: better_ds,
            source_port_identity: better_port_id,
            receiver,
        };
        assert!(matches!(
            bmca.decision(&e_best),
            Some(BmcaDecision::Slave(_))
        ));
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

        // Seed Ebest on another local receive port. It must remain best after Erbest is remembered.
        let better = TestClockDS::default_high_grade().with_priority1(Priority1::new(1));
        let e_best_parent = PortIdentity::new(better.clock_identity(), PortNumber::new(1));
        foreign_candidates.remember(
            other_port,
            BestForeignSnapshot::Qualified {
                ds: better.dataset(),
                source_port_identity: e_best_parent,
                receiver: PortIdentity::fake_on_port(other_port),
            },
        );

        let bmca = BestMasterClockAlgorithm::new(&default_ds, local_port);

        let worse = TestClockDS::default_mid_grade();
        let e_rbest_parent = PortIdentity::new(worse.clock_identity(), PortNumber::new(1));
        let e_rbest_ds = worse.dataset();
        let e_rbest = BestForeignSnapshot::Qualified {
            ds: e_rbest_ds,
            source_port_identity: e_rbest_parent,
            receiver: PortIdentity::fake_on_port(local_port),
        };

        // Not-S1 condition: when Ebest was not received on this port, we must not recommend Slave.
        let e_best = BestForeignSnapshot::Empty; // Ebest from other port
        let decision = bmca.decision(&e_best, &e_rbest);
        assert!(!matches!(decision, BmcaDecision::Slave(_)));
    }

    #[test]
    fn bmca_selects_passive_p2_when_ebest_is_better_only_by_topology_than_erbest() {
        let local_port = PortNumber::new(1);
        let d_0 = TestClockDS::default_low_grade_slave_only().dataset();
        let bmca = BestMasterClockAlgorithm::new(&d_0, local_port);

        let foreign_ds = TestClockDS::default_high_grade().dataset();

        let e_best_sender = PortIdentity::new(
            TestClockDS::atomic_grandmaster().clock_identity(),
            PortNumber::new(1),
        );
        let e_rbest_sender = PortIdentity::new(
            TestClockDS::gps_grandmaster().clock_identity(),
            PortNumber::new(1),
        );

        let e_best = BestForeignSnapshot::Qualified {
            ds: foreign_ds,
            source_port_identity: e_best_sender,
            receiver: PortIdentity::fake_on_port(PortNumber::new(2)),
        };
        let e_rbest = BestForeignSnapshot::Qualified {
            ds: foreign_ds,
            source_port_identity: e_rbest_sender,
            receiver: PortIdentity::fake_on_port(local_port),
        };

        assert_eq!(bmca.decision(&e_best, &e_rbest), BmcaDecision::Passive);
    }

    #[test]
    fn local_ds_uses_self_sender_and_receiver_for_topology_tie_break() {
        let id = ClockIdentity::new(&[0x42; 8]);
        let local_ds = TestClockDS::default_high_grade()
            .with_clock_identity(id)
            .with_steps_removed(StepsRemoved::new(0))
            .dataset();
        let foreign_ds = TestClockDS::default_high_grade()
            .with_clock_identity(id)
            .with_steps_removed(StepsRemoved::new(0))
            .dataset();

        // Artificial snapshot: foreign sender equals self topology identity.
        // This forces the topology tie-break into the receiver port-number comparison.
        let snapshot = BestForeignSnapshot::Qualified {
            ds: foreign_ds,
            source_port_identity: PortIdentity::new(id, PortNumber::new(0)),
            receiver: PortIdentity::new(id, PortNumber::new(1)),
        };

        assert!(
            matches!(
                LocalDS::new(local_ds).is_better_or_better_by_topology(&snapshot),
                Ok(gm_id) if gm_id == id
            ),
            "expected local dataset to win topology tie-break"
        );
    }

    #[test]
    fn listening_bmca_prunes_stale_foreign_clocks_on_next_announce_reception() {
        let high = TestClockDS::default_high_grade();
        let mid = TestClockDS::default_mid_grade();
        let low = TestClockDS::default_low_grade_slave_only();
        let gm = TestClockDS::gps_grandmaster();

        let stale_records = vec![
            ForeignClockRecord::new_qualified(
                PortIdentity::new(high.clock_identity(), PortNumber::new(1)),
                high.dataset(),
                LogInterval::new(0),
                Instant::from_secs(0),
            ),
            ForeignClockRecord::new_qualified(
                PortIdentity::new(mid.clock_identity(), PortNumber::new(1)),
                mid.dataset(),
                LogInterval::new(0),
                Instant::from_secs(1),
            ),
            ForeignClockRecord::new_qualified(
                PortIdentity::new(mid.clock_identity(), PortNumber::new(1)),
                low.dataset(),
                LogInterval::new(0),
                Instant::from_secs(2),
            ),
        ];
        let mut records = ForeignClockRecordsVec::from_records(&stale_records);

        let gm_port_id = PortIdentity::new(gm.clock_identity(), PortNumber::new(1));
        let gm_foreign = gm.dataset();

        // Consider a new announce from a different foreign clock.
        let default_ds = TestClockDS::default_high_grade().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), &mut records),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
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
            records.len(),
            1,
            "stale foreign clock records should have been pruned"
        );
        // Verify the single remaining record is the newly qualified one.
        let expected_record = ForeignClockRecord::new_qualified(
            gm_port_id,
            gm_foreign,
            LogInterval::new(0),
            Instant::from_secs(10),
        );
        let remaining: Vec<_> = records.iter().collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0], &expected_record);
    }

    #[test]
    fn listening_bmca_gm_capable_local_with_no_qualified_foreign_is_undecided() {
        let default_ds = TestClockDS::gps_grandmaster().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        assert_eq!(bmca.decision(&BestForeignSnapshot::Empty), None);
    }

    #[test]
    fn listening_bmca_gm_capable_local_loses_tuple_returns_passive() {
        let default_ds = TestClockDS::gps_grandmaster().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
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

        assert_eq!(
            bmca.decision(&BestForeignSnapshot::Empty),
            Some(BmcaDecision::Passive)
        );
    }

    #[test]
    fn listening_bmca_gm_capable_local_better_than_foreign_returns_master_m1() {
        let default_ds = TestClockDS::gps_grandmaster().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let local_identity = default_ds.identity();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
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
            bmca.decision(&BestForeignSnapshot::Empty),
            Some(BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M1,
                StepsRemoved::new(0),
                local_identity,
            )))
        );
    }

    #[test]
    fn listening_bmca_non_gm_local_better_than_foreign_returns_master_m2() {
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let local_identity = default_ds.identity();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
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
            bmca.decision(&BestForeignSnapshot::Empty),
            Some(BmcaDecision::Master(BmcaMasterDecision::new(
                BmcaMasterDecisionPoint::M2,
                StepsRemoved::new(0),
                local_identity,
            )))
        );
    }

    #[test]
    fn listening_bmca_non_gm_local_loses_tuple_returns_slave() {
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        let foreign = TestClockDS::default_high_grade().dataset();
        let port_id = PortIdentity::new(
            TestClockDS::default_high_grade().clock_identity(),
            PortNumber::new(1),
        );

        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));
        bmca.consider(port_id, foreign, LogInterval::new(0), Instant::from_secs(0));

        // In a single-port scenario, e_best equals e_rbest (the qualified record on this port)
        let e_best = BestForeignSnapshot::Qualified {
            ds: foreign,
            source_port_identity: port_id,
            receiver: PortIdentity::fake(),
        };
        assert_eq!(
            bmca.decision(&e_best),
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id))),
        );
    }

    #[test]
    fn listening_bmca_recommends_slave_from_interleaved_announce_sequence() {
        let default_ds = TestClockDS::default_low_grade().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
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

        // In a single-port scenario, e_best equals e_rbest (the best qualified record on this port)
        // After the interleaved sequence, the high-grade clock should be the best
        let e_best = BestForeignSnapshot::Qualified {
            ds: foreign_high,
            source_port_identity: port_id_high,
            receiver: PortIdentity::fake(),
        };
        let decision = bmca.decision(&e_best);

        assert_eq!(
            decision,
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id_high))),
        );
    }

    #[test]
    fn listening_bmca_recommends_slave_from_non_interleaved_announce_sequence() {
        let default_ds = TestClockDS::default_low_grade().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
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

        // In a single-port scenario, e_best equals e_rbest (the best qualified record on this port)
        // After the non-interleaved sequence, the high-grade clock should be the best
        let e_best = BestForeignSnapshot::Qualified {
            ds: foreign_high,
            source_port_identity: port_id_high,
            receiver: PortIdentity::fake(),
        };
        let decision = bmca.decision(&e_best);

        assert_eq!(
            decision,
            Some(BmcaDecision::Slave(ParentPortIdentity::new(port_id_high)))
        );
    }

    #[test]
    fn listening_bmca_undecided_when_no_announces_yet() {
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        assert_eq!(bmca.decision(&BestForeignSnapshot::Empty), None);
    }

    #[test]
    fn listening_bmca_undecided_when_no_qualified_clock_records_yet() {
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
        );

        let foreign_high = TestClockDS::default_high_grade().dataset();

        bmca.consider(
            PortIdentity::fake(),
            foreign_high,
            LogInterval::new(0),
            Instant::from_secs(0),
        );

        assert_eq!(bmca.decision(&BestForeignSnapshot::Empty), None);
    }

    #[test]
    fn listening_bmca_undecided_when_only_single_announces_each() {
        let default_ds = TestClockDS::default_mid_grade().dataset();
        let state_decision_event = FakeStateDecisionEvent::new();
        let mut bmca = ListeningBmca::new(
            BestMasterClockAlgorithm::new(&default_ds, PortNumber::new(1)),
            BestForeignRecord::new(PortIdentity::fake(), ForeignClockRecordsVec::new()),
            StateDecisionEventTrigger::new(
                &state_decision_event,
                BestForeignSnapshot::Empty,
                PortNumber::new(1),
            ),
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

        assert_eq!(bmca.decision(&BestForeignSnapshot::Empty), None);
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
